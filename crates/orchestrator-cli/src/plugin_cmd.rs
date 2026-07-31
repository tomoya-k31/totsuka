//! `totsuka plugin ...` subcommands (F-52/F-55/F-56/F-57).

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::Subcommand;
use orchestrator_core::config::{RootConfig, set_plugin_enabled};
use serde::Serialize;

use crate::bundled;
use crate::common::{CliError, Cx, JsonFlag, print_json};
use crate::from_source;

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
        /// Build the plugin from a totsuka checkout and install it. Development
        /// affordance: requires a clone, not just the CLI.
        #[arg(long)]
        from_source: bool,
        /// With `--from-source`: the checkout to build in (default: search
        /// upwards from the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// With `--from-source`: which Cargo profile to build.
        #[arg(long, value_enum, default_value_t = BuildProfile::Release)]
        profile: BuildProfile,
        /// With `--from-source`: print the cargo invocation and what would be
        /// installed, then stop without building.
        #[arg(long)]
        print_plan: bool,
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
            from_source,
            repo,
            profile,
            print_plan,
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
                from_source,
                repo,
                profile,
                print_plan,
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
    from_source: bool,
    repo: Option<PathBuf>,
    profile: BuildProfile,
    print_plan: bool,
}

/// Which Cargo profile `--from-source` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BuildProfile {
    /// `cargo build --release` → `target/release`.
    Release,
    /// `cargo build` → `target/debug`.
    Dev,
}

/// Where a plugin's manifest and binary are read from. Kept as two paths
/// rather than one directory because `--from-source` reads the manifest from
/// `plugins/<pkg>/` and the binary from `target/<profile>/` (#345's parts API).
struct InstallSource {
    manifest_path: PathBuf,
    binary_dir: PathBuf,
    /// What to show as "Source:" — the directory a human would look in.
    label: PathBuf,
}

impl InstallSource {
    /// A plain directory holding both `plugin.toml` and the binary.
    fn dir(dir: PathBuf) -> Self {
        Self {
            manifest_path: dir.join("plugin.toml"),
            binary_dir: dir.clone(),
            label: dir,
        }
    }
}

fn install(cx: &Cx, env: &HashMap<String, String>, args: InstallArgs) -> Result<(), CliError> {
    // Config first: a broken `TOTSUKA_*` override must fail here, before the
    // store is touched — not after "Installed" has already been printed.
    let config = cx.load_config_or_default(env)?;

    // `--enable` edits config.toml *after* the binary is in the store, so a
    // missing or unparseable file would leave "installed but the command
    // failed". Reject it up front, while nothing has been written yet. The
    // read is the same one `set_enabled` does, so the error text matches.
    if args.enable {
        read_config_for_edit(cx)?;
    }

    let Some(sources) = resolve_sources(&args)? else {
        // `--print-plan` printed what it would do and stops here.
        return Ok(());
    };
    for source in &sources {
        install_one(cx, &config, source, &args)?;
    }
    Ok(())
}

/// Read `config.toml` as raw text for a `set_plugin_enabled` edit, mapping a
/// missing file to the "run `totsuka init`" guidance.
fn read_config_for_edit(cx: &Cx) -> Result<String, CliError> {
    let text = std::fs::read_to_string(&cx.config_path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            CliError::from(format!(
                "config.toml not found at {} → run `totsuka init` to create it",
                cx.config_path.display()
            ))
        } else {
            CliError::from(e)
        }
    })?;
    // Parse too: an unparseable config fails the edit just as hard as a
    // missing one, and it is just as cheap to find out now.
    RootConfig::from_toml_str(&text)?;
    Ok(text)
}

/// Turn the flag combination into what to install. `Ok(None)` means
/// `--print-plan` already reported and nothing should be installed. Every
/// rejection names the flag that is wrong and what to do instead.
fn resolve_sources(args: &InstallArgs) -> Result<Option<Vec<InstallSource>>, CliError> {
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

    if args.bundled && args.from_source {
        return Err("`--bundled` and `--from-source` are different sources → pick one".into());
    }
    if args.from_source {
        return from_source_sources(args);
    }

    if !args.bundled {
        if args.all {
            return Err("`--all` only applies to `--bundled` or `--from-source`".into());
        }
        if args.bundled_dir.is_some() {
            return Err("`--bundled-dir` only applies to `--bundled`".into());
        }
        if args.print_plan {
            return Err("`--print-plan` only applies to `--from-source`".into());
        }
        let source = args.source.as_deref().ok_or(
            "`totsuka plugin install` needs a directory → pass one, or use `--bundled <name>` \
             to install a plugin shipped with this binary",
        )?;
        return Ok(Some(vec![InstallSource::dir(PathBuf::from(source))]));
    }
    if args.repo.is_some() {
        return Err("`--repo` only applies to `--from-source`".into());
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
        (Some(name), false) => {
            // Format the "available" list from the same snapshot that was
            // searched: re-reading the directory could report a set that does
            // not match what the lookup actually saw.
            let names = available
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            available
                .into_iter()
                .find(|p| &p.name == name)
                .map(|p| Some(vec![InstallSource::dir(p.dir)]))
                .ok_or_else(|| {
                    format!("`{name}` is not bundled with this `totsuka` → available: {names}")
                        .into()
                })
        }
        (None, true) => Ok(Some(
            available
                .into_iter()
                .map(|p| InstallSource::dir(p.dir))
                .collect(),
        )),
        (Some(_), true) => {
            Err("pass either a plugin name or `--all` to `--bundled`, not both".into())
        }
        (None, false) => Err(
            "`--bundled` needs a plugin name, or `--all` to install every bundled plugin".into(),
        ),
    }
}

/// Resolve (and build) plugins out of a totsuka checkout.
///
/// This is the one place in `plugin install` that shells out to `cargo`. The
/// CLI already shells out to `git` and `op`, so the dependency is not new, and
/// putting it here rather than in `scripts/` keeps the name→package mapping and
/// the config edit with the code that owns the store.
fn from_source_sources(args: &InstallArgs) -> Result<Option<Vec<InstallSource>>, CliError> {
    if args.bundled_dir.is_some() {
        return Err("`--bundled-dir` only applies to `--bundled`".into());
    }

    let requested: Vec<&str> = match (&args.source, args.all) {
        (Some(_), true) => {
            return Err("pass either a plugin name or `--all` to `--from-source`, not both".into());
        }
        (None, false) => {
            return Err(
                "`--from-source` needs a plugin name, or `--all` to build every plugin in the \
                 checkout"
                    .into(),
            );
        }
        (Some(name), false) => vec![name.as_str()],
        (None, true) => Vec::new(),
    };

    let root = match &args.repo {
        Some(dir) => {
            if !from_source::is_checkout(dir) {
                return Err(format!(
                    "{} is not a totsuka checkout → expected a Cargo workspace root with a \
                     `plugins/` directory",
                    dir.display()
                )
                .into());
            }
            dir.clone()
        }
        None => {
            let cwd = std::env::current_dir()?;
            from_source::find_checkout_root(&cwd, &from_source::is_checkout).ok_or(
                "`--from-source` needs a totsuka checkout → cd into your clone, or pass \
                 `--repo <dir>`",
            )?
        }
    };

    let available = from_source::resolve_plugins(&root);
    if available.is_empty() {
        return Err(format!(
            "no plugins under {}/plugins → expected `<dir>/plugin.toml` + `<dir>/Cargo.toml`",
            root.display()
        )
        .into());
    }

    let selected: Vec<&from_source::SourcePlugin> = if requested.is_empty() {
        available.values().collect()
    } else {
        let names = available.keys().cloned().collect::<Vec<_>>().join(", ");
        requested
            .iter()
            .map(|name| {
                available.get(*name).ok_or_else(|| {
                    CliError::from(format!(
                        "`{name}` is not a plugin in {} → available: {names}",
                        root.display()
                    ))
                })
            })
            .collect::<Result<_, _>>()?
    };

    let release = args.profile == BuildProfile::Release;
    let packages: Vec<&str> = selected.iter().map(|p| p.package.as_str()).collect();
    let argv = from_source::cargo_argv(release, &packages);
    let binary_dir = root.join("target").join(from_source::profile_dir(release));

    let sources: Vec<InstallSource> = selected
        .iter()
        .map(|p| InstallSource {
            manifest_path: p.manifest_path.clone(),
            binary_dir: binary_dir.clone(),
            label: root.join("plugins"),
        })
        .collect();

    if args.print_plan {
        // Exists so the wiring can be tested without invoking Cargo — running
        // `cargo build` inside a test is forbidden (ADR-0018).
        println!("Checkout: {}", root.display());
        println!("Build:    cargo {}", argv.join(" "));
        println!("Binaries: {}", binary_dir.display());
        for plugin in &selected {
            println!(
                "Install:  {} ({}) from {}",
                plugin.name,
                plugin.package,
                plugin.manifest_path.display()
            );
        }
        return Ok(None);
    }

    println!("Checkout: {}", root.display());
    println!("Building: cargo {}", argv.join(" "));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(&cargo)
        .args(&argv)
        .current_dir(&root)
        .status()
        .map_err(|e| CliError::from(format!("failed to run cargo: {e} → is cargo on PATH?")))?;
    if !status.success() {
        return Err("cargo build failed → fix the build before installing".into());
    }

    Ok(Some(sources))
}

fn install_one(
    cx: &Cx,
    config: &RootConfig,
    source: &InstallSource,
    args: &InstallArgs,
) -> Result<(), CliError> {
    let store = cx.store();
    let plan = store.prepare_install_from(&source.manifest_path, &source.binary_dir)?;

    // Show the source and checksum, and require confirmation (§5.4).
    println!(
        "Plugin:   {} v{} ({:?})",
        plan.name, plan.manifest.version, plan.manifest.kind
    );
    println!("Source:   {}", source.label.display());
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
    let current = read_config_for_edit(cx)?;

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
