//! `totsuka plugin ...` subcommands (F-52/F-55/F-56/F-57).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use orchestrator_core::config::{RootConfig, set_plugin_enabled};
use orchestrator_core::paths::Paths;
use orchestrator_core::plugins::PluginStore;
use serde::Serialize;

/// A boxed error for CLI operations.
type CliError = Box<dyn std::error::Error>;

/// Plugin management subcommands.
#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// Install a plugin from a local directory (containing plugin.toml + binary).
    Install {
        /// Source: a local directory path (`github:owner/repo` is not yet
        /// supported in v1).
        source: String,
        /// Skip the confirmation prompt (for CI).
        #[arg(long)]
        yes: bool,
    },
    /// Uninstall a plugin (removes its binary; the config declaration remains).
    Uninstall {
        /// Plugin name.
        name: String,
    },
    /// Enable a plugin in config.toml.
    Enable {
        /// Plugin name.
        name: String,
    },
    /// Disable a plugin in config.toml.
    Disable {
        /// Plugin name.
        name: String,
    },
    /// List installed and configured plugins.
    List {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Resolved locations the plugin commands operate on.
struct Locations {
    store: PluginStore,
    config_path: PathBuf,
}

impl Locations {
    fn resolve() -> Result<Self, CliError> {
        let paths = Paths::from_system()?;
        Ok(Self {
            store: PluginStore::new(paths.data_dir().join("plugins")),
            config_path: paths.config_dir().join("config.toml"),
        })
    }

    /// Load config.toml (an empty config if the file does not exist).
    fn load_config(&self) -> Result<RootConfig, CliError> {
        match std::fs::read_to_string(&self.config_path) {
            Ok(s) => Ok(RootConfig::from_toml_str(&s)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(RootConfig::from_toml_str("")?),
            Err(e) => Err(e.into()),
        }
    }
}

/// Dispatch a plugin subcommand.
pub fn run(command: PluginCommand) -> Result<(), CliError> {
    let loc = Locations::resolve()?;
    match command {
        PluginCommand::Install { source, yes } => install(&loc, &source, yes),
        PluginCommand::Uninstall { name } => uninstall(&loc, &name),
        PluginCommand::Enable { name } => set_enabled(&loc, &name, true),
        PluginCommand::Disable { name } => set_enabled(&loc, &name, false),
        PluginCommand::List { json } => list(&loc, json),
    }
}

fn install(loc: &Locations, source: &str, yes: bool) -> Result<(), CliError> {
    if let Some(rest) = source.strip_prefix("github:") {
        return Err(format!(
            "GitHub Release install (`github:{rest}`) is not yet available in v1 → download the \
             plugin's `plugin.toml` and binary into a directory and run \
             `totsuka plugin install <dir>`"
        )
        .into());
    }

    let source_dir = Path::new(source);
    let plan = loc.store.prepare_install(source_dir)?;

    // Show the source and checksum, and require confirmation (§5.4).
    println!(
        "Plugin:   {} v{} ({:?})",
        plan.name, plan.manifest.version, plan.manifest.kind
    );
    println!("Source:   {}", plan.source.display());
    println!("SHA-256:  {}", plan.checksum);
    if !yes && !confirm("Install this plugin?")? {
        println!("Aborted; nothing was installed.");
        return Ok(());
    }

    loc.store.commit_install(&plan)?;
    println!(
        "Installed `{}` to {}",
        plan.name,
        loc.store.plugin_dir(&plan.name).display()
    );
    if !loc.load_config()?.plugins.contains_key(&plan.name) {
        println!(
            "Note: `{}` is installed but not enabled. Run `totsuka plugin enable {}`.",
            plan.name, plan.name
        );
    }
    Ok(())
}

fn uninstall(loc: &Locations, name: &str) -> Result<(), CliError> {
    if loc.store.uninstall(name)? {
        println!("Uninstalled `{name}`.");
    } else {
        println!("`{name}` was not installed; nothing to do.");
    }
    // Warn if config still declares it (config is the source of truth, F-56).
    if loc.load_config()?.plugins.contains_key(name) {
        eprintln!(
            "warning: `{name}` is still declared in config.toml → remove `[plugins.{name}]` or it will error on `config validate`"
        );
    }
    Ok(())
}

fn set_enabled(loc: &Locations, name: &str, enabled: bool) -> Result<(), CliError> {
    let current = std::fs::read_to_string(&loc.config_path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            format!(
                "config.toml not found at {} → create it first (`totsuka init` in a later release)",
                loc.config_path.display()
            )
            .into()
        } else {
            CliError::from(e)
        }
    })?;

    // If a new `[plugins.{name}]` section will be created, it needs `kind` to be
    // schema-valid. Take it from the installed manifest; if the plugin is
    // neither declared nor installed, we cannot know its kind — refuse rather
    // than write an unloadable config.
    let already_declared = loc.load_config()?.plugins.contains_key(name);
    let kind_if_new = if already_declared {
        None
    } else {
        match loc.store.kind_str_of(name)? {
            Some(kind) => Some(kind),
            None => {
                return Err(format!(
                    "cannot {} `{name}`: it is neither installed nor declared in config.toml → install it first (`totsuka plugin install <dir>`)",
                    if enabled { "enable" } else { "disable" }
                )
                .into());
            }
        }
    };

    let updated = set_plugin_enabled(&current, name, enabled, kind_if_new.as_deref())?;
    std::fs::write(&loc.config_path, updated)?;

    let verb = if enabled { "Enabled" } else { "Disabled" };
    println!("{verb} `{name}` in {}", loc.config_path.display());
    Ok(())
}

/// One row of `plugin list`.
#[derive(Debug, Serialize)]
struct PluginRow {
    name: String,
    installed: bool,
    enabled: bool,
    kind: Option<String>,
    version: Option<String>,
    protocol: Option<String>,
}

fn list(loc: &Locations, json: bool) -> Result<(), CliError> {
    let installed = loc.store.list()?;
    let config = loc.load_config()?;

    // Union of installed and configured plugin names.
    let mut names: Vec<String> = installed.iter().map(|p| p.name.clone()).collect();
    for name in config.plugins.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();

    let rows: Vec<PluginRow> = names
        .into_iter()
        .map(|name| {
            let inst = installed.iter().find(|p| p.name == name);
            let cfg = config.plugins.get(&name);
            PluginRow {
                installed: inst.is_some(),
                enabled: cfg.map(|c| c.enabled).unwrap_or(false),
                kind: inst
                    .map(|i| format!("{:?}", i.kind))
                    .or_else(|| cfg.map(|c| format!("{:?}", c.kind))),
                version: inst.map(|i| i.version.clone()),
                protocol: inst.map(|i| i.protocol_version.clone()),
                name,
            }
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("No plugins installed or configured.");
    } else {
        println!(
            "{:<16} {:<9} {:<8} {:<10} {:<8} {:<8}",
            "NAME", "INSTALLED", "ENABLED", "KIND", "VERSION", "PROTOCOL"
        );
        for r in &rows {
            println!(
                "{:<16} {:<9} {:<8} {:<10} {:<8} {}",
                r.name,
                r.installed,
                r.enabled,
                r.kind.as_deref().unwrap_or("-"),
                r.version.as_deref().unwrap_or("-"),
                r.protocol.as_deref().unwrap_or("-"),
            );
        }
    }
    Ok(())
}

/// Prompt on stdin for a yes/no confirmation (default no).
fn confirm(prompt: &str) -> io::Result<bool> {
    print!("{prompt} [y/N]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
