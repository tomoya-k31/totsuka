//! `totsuka plugin ...` subcommands (F-52/F-55/F-56/F-57).

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use orchestrator_core::config::{RootConfig, set_plugin_enabled};
use serde::Serialize;

use crate::bundled;
use crate::common::{CliError, Cx, JsonFlag, print_json};

/// Plugin management subcommands.
#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// Install a plugin: from a local directory (plugin.toml + binary), or with
    /// `--bundled` from the plugins shipped alongside this binary.
    Install {
        /// Source: a local directory path, or — with `--bundled` — the name of
        /// a plugin shipped alongside this binary. (`github:owner/repo` is not
        /// yet supported in v1.)
        source: Option<String>,
        /// Install from the plugins bundled with this `totsuka` (the release
        /// tarball ships them next to the binary), instead of a path.
        #[arg(long)]
        bundled: bool,
        /// With `--bundled`: install every bundled plugin.
        #[arg(long)]
        all: bool,
        /// Also enable the plugin in config.toml (install and enable stay
        /// separate concepts — this is the opt-in that does both, F-56).
        #[arg(long)]
        enable: bool,
        /// Skip the confirmation prompt (for CI).
        #[arg(long)]
        yes: bool,
        /// Override where bundled plugins are looked up. Testing affordance;
        /// an env var is deliberately avoided (unknown `TOTSUKA_*` warns to
        /// stderr per ADR-0009 and breaks E2Es that parse it).
        #[arg(long, hide = true)]
        bundled_dir: Option<PathBuf>,
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
        #[command(flatten)]
        json: JsonFlag,
    },
}

impl PluginCommand {
    /// Whether this subcommand was invoked with `--json` (drives the JSON
    /// error envelope in `main`).
    pub fn wants_json(&self) -> bool {
        matches!(self, Self::List { json } if json.json)
    }
}

/// Dispatch a plugin subcommand. Paths and config loading go through [`Cx`]
/// like every other command (#175), so `--config` and the `TOTSUKA_*` env
/// layer apply here too. `install` / `uninstall` / `list` load the config
/// with [`Cx::load_config_or_default`] — they only cross-check declarations,
/// and must work before `totsuka init`. `enable` / `disable` edit the file
/// and error when it is missing.
pub fn run(cx: &Cx, command: PluginCommand) -> Result<(), CliError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    match command {
        PluginCommand::Install {
            source,
            bundled,
            all,
            enable,
            yes,
            bundled_dir,
        } => install(
            cx,
            &env,
            InstallArgs {
                source,
                bundled,
                all,
                enable,
                yes,
                bundled_dir,
            },
        ),
        PluginCommand::Uninstall { name } => uninstall(cx, &env, &name),
        PluginCommand::Enable { name } => set_enabled(cx, &name, true),
        PluginCommand::Disable { name } => set_enabled(cx, &name, false),
        PluginCommand::List { json } => list(cx, &env, json.json),
    }
}

/// Parsed `plugin install` arguments (the subcommand has enough knobs that
/// threading them positionally stops being readable).
struct InstallArgs {
    source: Option<String>,
    bundled: bool,
    all: bool,
    enable: bool,
    yes: bool,
    bundled_dir: Option<PathBuf>,
}

fn install(cx: &Cx, env: &HashMap<String, String>, args: InstallArgs) -> Result<(), CliError> {
    // Config first: a broken `TOTSUKA_*` override must fail here, before the
    // store is touched — not after "Installed" has already been printed.
    let config = cx.load_config_or_default(env)?;

    let sources = resolve_sources(&args)?;
    for dir in &sources {
        install_one(cx, &config, dir, &args)?;
    }
    Ok(())
}

/// Turn the flag combination into the list of source directories to install
/// from. Every rejection names the flag that is wrong and what to do instead.
fn resolve_sources(args: &InstallArgs) -> Result<Vec<PathBuf>, CliError> {
    if let Some(source) = &args.source
        && let Some(rest) = source.strip_prefix("github:")
    {
        return Err(format!(
            "GitHub Release install (`github:{rest}`) is not yet available in v1 → download the \
             plugin's `plugin.toml` and binary into a directory and run \
             `totsuka plugin install <dir>`"
        )
        .into());
    }

    if !args.bundled {
        if args.all {
            return Err("`--all` only applies to `--bundled` → run `totsuka plugin install --bundled --all`".into());
        }
        if args.bundled_dir.is_some() {
            return Err("`--bundled-dir` only applies to `--bundled`".into());
        }
        let source = args.source.as_deref().ok_or(
            "`totsuka plugin install` needs a directory → pass one, or use `--bundled <name>` \
             to install a plugin shipped with this binary",
        )?;
        return Ok(vec![PathBuf::from(source)]);
    }

    let Some(root) = bundled::locate(args.bundled_dir.as_deref()) else {
        return Err(
            "no bundled plugins found next to this `totsuka` → this looks like a \
                    `cargo install` build; install from a directory instead \
                    (`totsuka plugin install <dir>`)"
                .into(),
        );
    };
    let available = bundled::list(&root);
    if available.is_empty() {
        return Err(format!(
            "no plugins under {} → expected `<name>/plugin.toml` subdirectories",
            root.display()
        )
        .into());
    }
    // Say which tree was chosen: with several install shapes in play, "it
    // installed something" is not enough to know *what*.
    println!("Bundled plugins: {}", root.display());

    match (&args.source, args.all) {
        (Some(name), false) => available
            .into_iter()
            .find(|p| &p.name == name)
            .map(|p| vec![p.dir])
            .ok_or_else(|| {
                format!(
                    "`{name}` is not bundled with this `totsuka` → available: {}",
                    bundled::list(&root)
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into()
            }),
        (None, true) => Ok(available.into_iter().map(|p| p.dir).collect()),
        (Some(_), true) => {
            Err("pass either a plugin name or `--all` to `--bundled`, not both".into())
        }
        (None, false) => Err(
            "`--bundled` needs a plugin name, or `--all` to install every bundled plugin".into(),
        ),
    }
}

fn install_one(
    cx: &Cx,
    config: &RootConfig,
    source_dir: &Path,
    args: &InstallArgs,
) -> Result<(), CliError> {
    let store = cx.store();
    let plan = store.prepare_install(source_dir)?;

    // Show the source and checksum, and require confirmation (§5.4).
    println!(
        "Plugin:   {} v{} ({:?})",
        plan.name, plan.manifest.version, plan.manifest.kind
    );
    println!("Source:   {}", source_dir.display());
    println!("SHA-256:  {}", plan.checksum);
    if !args.yes && !confirm("Install this plugin?")? {
        println!("Aborted; nothing was installed.");
        return Ok(());
    }

    store.commit_install(&plan)?;
    println!(
        "Installed `{}` to {}",
        plan.name,
        store.plugin_dir(&plan.name).display()
    );

    if args.enable {
        set_enabled(cx, &plan.name, true)?;
    } else if !config.plugins.contains_key(&plan.name) {
        println!(
            "Note: `{}` is installed but not enabled. Run `totsuka plugin enable {}`.",
            plan.name, plan.name
        );
    }
    Ok(())
}

fn uninstall(cx: &Cx, env: &HashMap<String, String>, name: &str) -> Result<(), CliError> {
    // Config first, for the same reason as `install`: fail on a broken env
    // override before the store is mutated.
    let still_declared = cx.load_config_or_default(env)?.plugins.contains_key(name);
    if cx.store().uninstall(name)? {
        println!("Uninstalled `{name}`.");
    } else {
        println!("`{name}` was not installed; nothing to do.");
    }
    // Warn if config still declares it (config is the source of truth, F-56).
    if still_declared {
        eprintln!(
            "warning: `{name}` is still declared in config.toml (it stays listed and possibly enabled) → remove `[plugins.{name}]` if you no longer want it"
        );
    }
    Ok(())
}

fn set_enabled(cx: &Cx, name: &str, enabled: bool) -> Result<(), CliError> {
    // The edit works on the raw file text (comments and formatting must
    // survive `set_plugin_enabled`), so the env layer is deliberately not
    // folded into what gets written back.
    let current = std::fs::read_to_string(&cx.config_path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            format!(
                "config.toml not found at {} → run `totsuka init` to create it",
                cx.config_path.display()
            )
            .into()
        } else {
            CliError::from(e)
        }
    })?;

    // If a new `[plugins.{name}]` section will be created, it needs `kind` to be
    // schema-valid. Take it from the installed manifest; if the plugin is
    // neither declared nor installed, we cannot know its kind — refuse rather
    // than write an unloadable config. Parsed from `current` (already read):
    // the check guards the raw-text edit above, so the env layer — which
    // cannot declare plugins anyway — has no business here.
    let already_declared = RootConfig::from_toml_str(&current)?
        .plugins
        .contains_key(name);
    let kind_if_new = if already_declared {
        None
    } else {
        match cx.store().kind_str_of(name)? {
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
    std::fs::write(&cx.config_path, updated)?;

    let verb = if enabled { "Enabled" } else { "Disabled" };
    println!("{verb} `{name}` in {}", cx.config_path.display());
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

fn list(cx: &Cx, env: &HashMap<String, String>, json: bool) -> Result<(), CliError> {
    let installed = cx.store().list()?;
    let config = cx.load_config_or_default(env)?;

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
                    .map(|i| i.kind.clone())
                    .or_else(|| cfg.map(|c| c.kind.as_str().to_string())),
                version: inst.map(|i| i.version.clone()),
                protocol: inst.map(|i| i.protocol_version.clone()),
                name,
            }
        })
        .collect();

    if json {
        print_json(&rows)?;
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
