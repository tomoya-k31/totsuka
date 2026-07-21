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

use crate::common::{CliError, Cx, plugin_spec};

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
    let env: HashMap<String, String> = std::env::vars().collect();
    let cfg = cx.load_config(&env)?;
    let env_fn = |k: &str| env.get(k).cloned();
    let store = cx.store();

    // Hook capability is not yet declared in plugin manifests (protocol
    // 0.1.3, #132); `None` = unknown skips the `[hooks].auth_token_ref`
    // advisory until manifests can declare it.
    let findings = config::validate(
        &cfg,
        &env_fn,
        |name| {
            store
                .manifest_of(name)
                .ok()
                .flatten()
                .map(|m| m.capabilities.outputs)
        },
        |_| None,
    );
    let mut errors = config::has_errors(&findings);
    for finding in &findings {
        let label = match finding.severity {
            FindingSeverity::Error => "error",
            FindingSeverity::Warning => "warning",
        };
        println!("{label}: {}", finding.message);
    }

    // Online part (F-59): each enabled plugin validates its own config
    // (its kind is irrelevant — every kind implements `config/validate`).
    if !offline && !errors {
        let mut specs = Vec::new();
        for (name, _) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
            // `plugin_spec` already read and secret-resolved plugins/{name}.toml
            // into `init_config`; reuse it rather than resolving secrets twice
            // (a second Keychain access could trigger a second Touch prompt).
            let spec = plugin_spec(cx, &cfg, name, &env)?;
            let init_config = spec.init_config.clone();
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

    // `show` prints the *files*, so the env layer is not folded into the TOML
    // above — but leaving it out entirely would misrepresent what the daemon
    // will actually use, which is the very silence this command should break.
    let env: HashMap<String, String> = std::env::vars().collect();
    print_active_env_overrides(&env, redacted);
    Ok(())
}

/// List the `TOTSUKA_*` overrides that are actually set (F-66 layer 2), so
/// `show` cannot imply the files are the whole story.
fn print_active_env_overrides(env: &HashMap<String, String>, redacted: bool) {
    let active: Vec<(&str, &String)> = config::override_keys()
        .filter_map(|key| env.get_key_value(key).map(|(k, v)| (k.as_str(), v)))
        .collect();
    if active.is_empty() {
        return;
    }
    println!("\n# active env overrides (TOTSUKA_*)");
    for (key, value) in active {
        // Same masking rule as the TOML bodies above, applied to the variable
        // name (`..._AUTH_TOKEN_REF`, `..._API_KEY_REF`).
        let shown = if redacted && is_secret_key(key) {
            "***redacted***"
        } else {
            value.as_str()
        };
        println!("# {key}={shown}");
    }
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

/// Whether a key looks secret-bearing (§5.2 masking convention). Conservative:
/// `key` matches `api_key` / `access_key` / `private_key` / `apikey` too, so
/// `--redacted` masks anything the help text promises (token/key/secret/
/// password) — over-masking is safe, under-masking leaks.
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "key", "secret", "password", "credential"]
        .iter()
        .any(|pat| k.contains(pat))
}

/// Recursively mask values under secret-looking keys. A secret key's value is
/// masked **whole**, whatever its shape — a bare string, an array of strings
/// (`api_keys = ["…", "…"]`), or an inline table — so no secret survives via a
/// non-string container. Non-secret keys are descended into so nested secrets
/// are still caught.
fn redact_table(table: &mut toml::Table) {
    for (key, value) in table.iter_mut() {
        if is_secret_key(key) {
            *value = toml::Value::String("***redacted***".to_string());
            continue;
        }
        match value {
            toml::Value::Table(inner) => redact_table(inner),
            toml::Value::Array(items) => {
                for item in items {
                    if let toml::Value::Table(inner) = item {
                        redact_table(inner);
                    }
                }
            }
            _ => {}
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
private_key = "-----BEGIN-----"
api_keys = ["ghp_one", "ghp_two"]
name = "visible"

[nested]
github_token = "ghp_plain"

[[creds]]
password = "hunter2"
label = "shown"
"#
        .parse()
        .unwrap();
        redact_table(&mut table);
        assert_eq!(
            table["api_key_ref"].as_str().unwrap(),
            "***redacted***",
            "key containing api_key is masked"
        );
        // A bare `key`-bearing name is masked (help promises it).
        assert_eq!(table["private_key"].as_str().unwrap(), "***redacted***");
        // A secret-looking key whose value is an array of strings is masked
        // whole, not leaked element-by-element.
        assert_eq!(table["api_keys"].as_str().unwrap(), "***redacted***");
        assert_eq!(table["name"].as_str().unwrap(), "visible");
        assert_eq!(
            table["nested"]["github_token"].as_str().unwrap(),
            "***redacted***"
        );
        // Array-of-tables: secret keys inside each table are still caught.
        assert_eq!(
            table["creds"][0]["password"].as_str().unwrap(),
            "***redacted***"
        );
        assert_eq!(table["creds"][0]["label"].as_str().unwrap(), "shown");
    }
}
