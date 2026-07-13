//! `totsuka config ...` — validation and display (F-59/63, §5.1).
//!
//! `validate` runs the offline checks (schema, static references, workflow
//! semantics) and — unless `--offline` — briefly launches each enabled plugin
//! to delegate `config/validate` (F-59). `show` prints the effective files,
//! masking secret-looking values with `--redacted`.

use std::collections::HashMap;
use std::io;

use clap::Subcommand;
use orchestrator_core::adapters::plugin_host;
use orchestrator_core::config::{self, FindingSeverity};

use crate::common::{CliError, Cx, plugin_init_config, plugin_spec};

/// Config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Validate config.toml and plugins/{name}.toml.
    Validate {
        /// Skip the online part (launching plugins for `config/validate`).
        #[arg(long)]
        offline: bool,
    },
    /// Print the effective configuration files.
    Show {
        /// Mask values of secret-looking keys (token/key/secret/password).
        #[arg(long)]
        redacted: bool,
    },
}

/// Dispatch a config subcommand.
pub fn run(cx: &Cx, command: ConfigCommand) -> Result<(), CliError> {
    match command {
        ConfigCommand::Validate { offline } => validate(cx, offline),
        ConfigCommand::Show { redacted } => show(cx, redacted),
    }
}

fn validate(cx: &Cx, offline: bool) -> Result<(), CliError> {
    let cfg = cx.load_config()?;
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();
    let store = cx.store();

    let findings = config::validate(&cfg, &env_fn, |name| {
        store
            .manifest_of(name)
            .ok()
            .flatten()
            .map(|m| m.capabilities.outputs)
    });
    let mut errors = config::has_errors(&findings);
    for finding in &findings {
        let label = match finding.severity {
            FindingSeverity::Error => "error",
            FindingSeverity::Warning => "warning",
        };
        println!("{label}: {}", finding.message);
    }

    // Online part (F-59): each enabled plugin validates its own config.
    if !offline && !errors {
        let mut specs = Vec::new();
        for (name, plugin_cfg) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
            let spec = plugin_spec(cx, &cfg, name, &env)?;
            let init_config = plugin_init_config(cx, name, &env)?;
            let _ = plugin_cfg; // kind is irrelevant here: every kind validates
            specs.push((spec, init_config));
        }
        let runtime = tokio::runtime::Runtime::new()?;
        for (name, result) in runtime.block_on(plugin_host::validate_all(specs)) {
            match result {
                Ok(v) if v.valid => println!("ok: plugin `{name}` accepted its config"),
                Ok(v) => {
                    errors = true;
                    for problem in v.errors {
                        println!("error: plugin `{name}`: {problem}");
                    }
                }
                Err(e) => {
                    errors = true;
                    println!("error: plugin `{name}` could not be probed: {e}");
                }
            }
        }
    } else if offline {
        println!("note: --offline skipped plugin config/validate probes (F-63)");
    }

    if errors {
        return Err("configuration is invalid → fix the errors above".into());
    }
    println!("configuration is valid");
    Ok(())
}

fn show(cx: &Cx, redacted: bool) -> Result<(), CliError> {
    let contents = std::fs::read_to_string(&cx.config_path).map_err(|e| -> CliError {
        if e.kind() == io::ErrorKind::NotFound {
            format!(
                "config not found at {} → run `totsuka init` to create it",
                cx.config_path.display()
            )
            .into()
        } else {
            e.into()
        }
    })?;
    println!("# {}", cx.config_path.display());
    print_toml(&contents, redacted)?;

    let plugin_dir = cx.plugin_config_dir();
    if let Ok(entries) = std::fs::read_dir(&plugin_dir) {
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();
        for path in paths {
            println!("\n# {}", path.display());
            print_toml(&std::fs::read_to_string(&path)?, redacted)?;
        }
    }
    Ok(())
}

/// Print a TOML document, optionally masking secret-looking keys.
fn print_toml(contents: &str, redacted: bool) -> Result<(), CliError> {
    if !redacted {
        print!("{contents}");
        if !contents.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let mut table: toml::Table = contents
        .parse()
        .map_err(|e| format!("failed to parse TOML: {e}"))?;
    redact_table(&mut table);
    print!("{}", toml::to_string_pretty(&table)?);
    Ok(())
}

/// Whether a key looks secret-bearing (§5.2 masking convention).
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "secret", "password", "api_key", "apikey"]
        .iter()
        .any(|pat| k.contains(pat))
}

/// Recursively mask string values under secret-looking keys.
fn redact_table(table: &mut toml::Table) {
    for (key, value) in table.iter_mut() {
        match value {
            toml::Value::Table(inner) => redact_table(inner),
            toml::Value::Array(items) => {
                for item in items {
                    if let toml::Value::Table(inner) = item {
                        redact_table(inner);
                    }
                }
            }
            other => {
                if is_secret_key(key) {
                    *other = toml::Value::String("***redacted***".to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_keys_recursively() {
        let mut table: toml::Table = r#"
api_key_ref = "keychain:totsuka/x"
name = "visible"

[nested]
github_token = "ghp_plain"
"#
        .parse()
        .unwrap();
        redact_table(&mut table);
        assert_eq!(
            table["api_key_ref"].as_str().unwrap(),
            "***redacted***",
            "key containing api_key is masked"
        );
        assert_eq!(table["name"].as_str().unwrap(), "visible");
        assert_eq!(
            table["nested"]["github_token"].as_str().unwrap(),
            "***redacted***"
        );
    }
}
