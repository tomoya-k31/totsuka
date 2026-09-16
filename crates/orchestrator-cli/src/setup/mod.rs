//! `totsuka setup` — from a fresh install to a `config.toml` that loads, and a
//! clear instruction to go edit it (#705).
//!
//! # What it does, and what it deliberately does not
//!
//! It writes the **whole** configuration surface into `config.toml`, commented
//! out, and tells you where the file is. It does not try to fill it in.
//!
//! That is the correction #705 made to [ADR-0028]. The interactive wizard this
//! replaces asked a dozen questions and produced a working config — for the
//! four recipes it knew. Everything else (notion, discord, orca, `[hooks]`,
//! `[tools.*]`, `[log]`, most of `[[workflows]]`) was unreachable, and adding
//! it meant more questions and more recipes, combinatorially. Writing every
//! option down as a comment costs one line each and has no such ceiling, so
//! the wizard's own knowledge — which columns pair with which profile — moved
//! into the skeleton's recipe section rather than being lost.
//!
//! One question survives, because it is the one answer that changes what is
//! *installed* rather than what is written: which plugins you will use.
//!
//! # Why nothing is enabled
//!
//! Selected plugins are installed, and their `[plugins.<name>] enabled = true`
//! stays commented. `totsuka config validate` **launches** every enabled
//! plugin, so enabling `github` before `[github].token` exists would make the
//! command that is supposed to confirm the setup fail on the setup itself.
//!
//! # Secrets
//!
//! `setup` never handles a secret value. It picks a backend, writes the
//! *references* into the config, and prints the commands to register them
//! (F-65). See [`secrets`].
//!
//! [ADR-0028]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0028-setup-wizard.md

mod secrets;
mod template;

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::common::{CliError, Cx, EXIT_USAGE, ExitWith};
use crate::{bundled, from_source, plugin_cmd};

pub use secrets::SecretBackend;

/// Options parsed from the command line.
#[derive(Debug, Default)]
pub struct SetupArgs {
    /// Which plugins to install: a comma-separated list, `all`, or `none`.
    ///
    /// The non-interactive form of the one remaining question. Without a
    /// terminal and without this, `setup` stops rather than guessing.
    pub plugins: Option<String>,
    /// Which secret store the written references point at.
    pub secret_backend: SecretBackend,
    /// Print the plan and stop.
    pub dry_run: bool,
    /// Pin where bundled plugins are looked up, instead of detecting.
    ///
    /// Hidden, and the same affordance `plugin install --bundled-dir`
    /// provides, for the same reason: an E2E runs `totsuka` as a child process
    /// whose working directory is inside this checkout, so without a pin the
    /// command would detect a checkout and shell out to `cargo build` — which
    /// tests are not allowed to do (ADR-0018). An env var is not an option
    /// either (ADR-0009).
    pub bundled_dir: Option<PathBuf>,
}

/// Run the command.
pub fn run(cx: &Cx, args: &SetupArgs) -> Result<(), CliError> {
    let selected = select_plugins(args)?;
    let source = PluginSource::detect(args.bundled_dir.as_deref());
    let rendered = template::render(&selected, args.secret_backend);
    let existing = read_existing(&cx.config_path)?;
    let plan = Plan::new(cx, &selected, &rendered, existing.as_deref(), &source);

    print!("{}", plan.render());
    if args.dry_run {
        println!("\n--dry-run: nothing was written.");
        return Ok(());
    }

    ensure_dirs(cx)?;
    match &plan.write {
        ConfigWrite::Fresh(text) => {
            write_atomically(&cx.config_path, text)?;
            println!("created: {}", cx.config_path.display());
        }
        ConfigWrite::Append(text) => {
            let mut merged = existing.clone().unwrap_or_default();
            if !merged.ends_with('\n') {
                merged.push('\n');
            }
            merged.push('\n');
            merged.push_str(text);
            write_atomically(&cx.config_path, &merged)?;
            println!("updated: {}", cx.config_path.display());
        }
        ConfigWrite::Unchanged => {
            println!(
                "unchanged: {} already documents every section setup would add",
                cx.config_path.display()
            );
        }
    }

    install_plugins(cx, &selected, &source)?;

    // The check `init` used to carry. Nothing else in this command needs git,
    // but everything downstream does — a worktree cannot be created without
    // it — and with `doctor` no longer run here, this is the only place a
    // fresh machine hears about it.
    match crate::common::git_version() {
        Some(version) => println!("ok: git {version}"),
        None => println!("warning: git not found on PATH → install git (worktrees require it)"),
    }

    print_next_steps(cx, &selected, args.secret_backend);
    Ok(())
}

// ---------------------------------------------------------------------------
// The one question
// ---------------------------------------------------------------------------

/// Which plugins to install: from `--plugins`, or by asking.
fn select_plugins(args: &SetupArgs) -> Result<BTreeSet<String>, CliError> {
    let known = template::known_plugins();
    match &args.plugins {
        Some(spec) => parse_plugins(spec, &known),
        None => {
            if !std::io::stdin().is_terminal() {
                return Err(ExitWith::new(
                    EXIT_USAGE,
                    format!(
                        "`totsuka setup` needs a terminal to ask which plugins you will use → \
                         pass them instead: `--plugins {}`, or `--plugins all` / `--plugins none`",
                        known
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                )
                .into());
            }
            ask_plugins(&known)
        }
    }
}

/// Parse `--plugins`.
///
/// An unknown name is an error naming the valid ones rather than a silent
/// omission: a typo'd `--plugins gihub` that installed nothing would look
/// exactly like a successful run.
fn parse_plugins(
    spec: &str,
    known: &[template::KnownPlugin],
) -> Result<BTreeSet<String>, CliError> {
    let spec = spec.trim();
    if spec == "all" {
        return Ok(known.iter().map(|p| p.name.clone()).collect());
    }
    if spec == "none" {
        return Ok(BTreeSet::new());
    }
    // An empty list is the state the TTY gate and `--plugins` exist to rule
    // out: a run that installs nothing and reports success. `none` is how you
    // say it on purpose.
    if spec.split(',').all(|s| s.trim().is_empty()) {
        return Err(ExitWith::new(
            EXIT_USAGE,
            "`--plugins` names no plugin → list them, or say `--plugins none` if that is what you meant",
        )
        .into());
    }
    let mut selected = BTreeSet::new();
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !known.iter().any(|p| p.name == name) {
            return Err(ExitWith::new(
                EXIT_USAGE,
                format!(
                    "unknown plugin `{name}` → pick from {}, or use `all` / `none`",
                    known
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
            )
            .into());
        }
        selected.insert(name.to_string());
    }
    Ok(selected)
}

/// Ask, on a terminal.
fn ask_plugins(known: &[template::KnownPlugin]) -> Result<BTreeSet<String>, CliError> {
    let items: Vec<String> = known
        .iter()
        .map(|p| format!("{:<8} {}", p.name, p.label))
        .collect();
    println!("totsuka setup — this installs the plugins you pick and writes a config.toml");
    println!("with every setting in it, commented out. It never asks for a secret.");
    println!();
    let chosen = dialoguer::MultiSelect::new()
        .with_prompt("Which plugins will you use? (space to toggle, enter to confirm)")
        .items(&items)
        .interact()
        .map_err(|e| CliError::from(format!("could not read the selection ({e})")))?;
    Ok(chosen.into_iter().map(|i| known[i].name.clone()).collect())
}

// ---------------------------------------------------------------------------
// The config file
// ---------------------------------------------------------------------------

/// Read the config, distinguishing "absent" from "unreadable".
///
/// Any failure other than absence is reported rather than read as "no file":
/// the answer decides whether `setup` writes into a file it could not inspect,
/// and that file holds the operator's secret references.
fn read_existing(path: &Path) -> Result<Option<String>, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CliError::from(format!(
            "cannot read {} ({e}) → refusing to write into a config it cannot inspect",
            path.display()
        ))),
    }
}

/// What will happen to `config.toml`.
enum ConfigWrite {
    /// No file yet: write the rendered skeleton.
    Fresh(String),
    /// A file exists: append the sections it does not have yet.
    Append(String),
    /// A file exists and has them all.
    Unchanged,
}

/// Decided before anything is written, so it can be shown and so `--dry-run`
/// and the real run cannot disagree.
struct Plan<'a> {
    config_path: PathBuf,
    selected: &'a BTreeSet<String>,
    write: ConfigWrite,
    /// Table paths the append is made of, for the printed plan.
    added: Vec<String>,
    source: &'a PluginSource,
}

impl<'a> Plan<'a> {
    fn new(
        cx: &Cx,
        selected: &'a BTreeSet<String>,
        rendered: &str,
        existing: Option<&str>,
        source: &'a PluginSource,
    ) -> Plan<'a> {
        let (write, added) = match existing {
            None => (ConfigWrite::Fresh(rendered.to_string()), Vec::new()),
            Some(existing) => {
                let missing: Vec<Block> = blocks(rendered)
                    .into_iter()
                    .filter(|b| !b.is_present_in(existing))
                    .collect();
                if missing.is_empty() {
                    (ConfigWrite::Unchanged, Vec::new())
                } else {
                    // Several commented examples can document the same table
                    // (`[[projects]]` appears once per source and again in the
                    // recipes); the plan names each table once.
                    let mut paths: Vec<String> = Vec::new();
                    for block in &missing {
                        if !paths.contains(&block.path) {
                            paths.push(block.path.clone());
                        }
                    }
                    let text = missing
                        .iter()
                        .map(|b| b.text.as_str())
                        .collect::<Vec<_>>()
                        .join("");
                    (ConfigWrite::Append(text), paths)
                }
            }
        };
        Plan {
            config_path: cx.config_path.clone(),
            selected,
            write,
            added,
            source,
        }
    }

    fn render(&self) -> String {
        let mut out = String::from("\nSetup plan\n\n");
        out.push_str(&format!("  config   {}\n", self.config_path.display()));
        out.push_str(&match &self.write {
            ConfigWrite::Fresh(_) => {
                "           write it, with every setting commented out\n".into()
            }
            ConfigWrite::Append(_) => format!(
                "           append the {} table(s) it does not document yet:\n{}",
                self.added.len(),
                self.added
                    .iter()
                    .map(|t| format!("             [{t}]\n"))
                    .collect::<String>()
            ),
            ConfigWrite::Unchanged => "           leave it alone; nothing to add\n".into(),
        });
        out.push_str(&format!(
            "  plugins  {}\n",
            if self.selected.is_empty() {
                "none".to_string()
            } else {
                self.selected.iter().cloned().collect::<Vec<_>>().join(", ")
            }
        ));
        out.push_str(&match self.source {
            PluginSource::Bundled(root) => format!("           install from {}\n", root.display()),
            PluginSource::Checkout(root) => format!("           build from {}\n", root.display()),
            PluginSource::Unavailable => {
                "           nothing to install from; you will be told what to run\n".into()
            }
        });
        out
    }
}

/// One documented table of the rendered skeleton, with the prose above it.
///
/// **The append unit is a table, not a banner section.** Two things follow from
/// that, and both are the reason it is not a section:
///
/// * Content that is *not* under a table — the file's preamble, and the
///   top-level keys — is never appended. Appending it would drop root-level
///   keys at the end of a file that ends inside a table, where TOML reads them
///   as that table's keys: the one live line in the skeleton, `version = 1`,
///   lands inside the last `[[workflows]]` and `deny_unknown_fields` rejects
///   it. (Appending a *second* root `version` is the other half of the same
///   bug, and equally unwanted.)
/// * A plugin picked on a later run gets its roster entry. All seven
///   `[plugins.<name>]` entries share one banner, so a section-sized unit is
///   "already there" the moment any of them is — and the flagship case of
///   [`ADR-0077`] decision 6, adding `notion` months later, would append
///   `[notion]` with no `[plugins.notion]` to enable it by.
///
/// [`ADR-0077`]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0077-setup-writes-the-whole-surface.md
struct Block {
    /// The dotted path of the table this block documents (`plugins.notion`,
    /// `notion.property_map`, `projects`).
    path: String,
    /// The block, including the banner and prose that introduce it.
    text: String,
}

impl Block {
    /// Whether `existing` already documents or defines this table.
    ///
    /// Both halves matter, and for different callers. The **active** form
    /// catches a hand-written config: appending a commented block about
    /// `[github]` below a live `[github]` would read as a second,
    /// contradictory definition. The **commented** form catches a config this
    /// command wrote before — where nothing is active yet, so without it every
    /// re-run would append the whole file again.
    fn is_present_in(&self, existing: &str) -> bool {
        let active = [format!("[{}]", self.path), format!("[[{}]]", self.path)];
        let documented = [format!("# [{}]", self.path), format!("# [[{}]]", self.path)];
        // A header may carry a trailing comment (`# [herdr.kind_map]   # …`),
        // so this is a prefix test. The closing bracket makes it exact enough:
        // `# [github.prompts]` does not start with `# [github]`.
        existing.lines().any(|line| {
            let line = line.trim();
            active
                .iter()
                .chain(documented.iter())
                .any(|f| line.starts_with(f.as_str()))
        })
    }
}

/// The banner line the skeleton separates its sections with.
fn banner_prefix() -> &'static str {
    "# ========"
}

/// Split rendered text into table blocks.
///
/// Lines between blocks attach to the block that *follows* them, so an
/// appended block carries the banner and the explanation that introduce it and
/// reads exactly as it does in a fresh file. Whatever trails the last block is
/// prose, and is dropped.
fn blocks(rendered: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut pending = String::new();
    let mut current: Option<Block> = None;
    // Distance from the last banner line, to tell a banner that opens a new
    // section from the one that closes the same section's title.
    let mut since_banner = usize::MAX;

    for line in rendered.lines() {
        if line.starts_with(banner_prefix()) {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            // A new section starts here, so whatever is still unattributed
            // belongs to the section that just ended and is not part of any
            // table. Dropping it is the whole point: that is where the
            // skeleton's root-level keys live.
            if since_banner > 2 {
                pending.clear();
            }
            since_banner = 0;
            pending.push_str(line);
            pending.push('\n');
            continue;
        }
        since_banner = since_banner.saturating_add(1);
        // The line right after a banner is the section's title, and titles are
        // written as `# [llm] — the AI Gateway …`. Reading that as a table
        // header would make every section start a block of its own, on top of
        // the one its real header starts.
        if since_banner == 1 {
            pending.push_str(line);
            pending.push('\n');
            continue;
        }
        if let Some(path) = table_path(line) {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            let mut text = std::mem::take(&mut pending);
            text.push_str(line);
            text.push('\n');
            current = Some(Block { path, text });
            continue;
        }
        match &mut current {
            Some(block) => {
                block.text.push_str(line);
                block.text.push('\n');
            }
            None => {
                pending.push_str(line);
                pending.push('\n');
            }
        }
    }
    if let Some(block) = current {
        blocks.push(block);
    }
    blocks
}

/// The table path a commented header line documents, if it is one.
fn table_path(line: &str) -> Option<String> {
    let line = line.trim().strip_prefix("# ").unwrap_or(line.trim());
    let line = line.trim();
    let inner = line
        .strip_prefix("[[")
        .and_then(|r| r.split("]]").next())
        .or_else(|| line.strip_prefix('[').and_then(|r| r.split(']').next()))?;
    inner
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-')
        .then(|| inner.to_string())
        .filter(|s| !s.is_empty())
}

fn write_atomically(path: &Path, contents: &str) -> Result<(), CliError> {
    let staged = path.with_extension("toml.new");
    std::fs::write(&staged, contents)?;
    if let Err(e) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(e.into());
    }
    Ok(())
}

/// Create the XDG directories totsuka writes into (§5.6).
pub fn ensure_dirs(cx: &Cx) -> Result<(), CliError> {
    for (label, dir) in [
        ("config", cx.config_path.parent().unwrap_or(Path::new("."))),
        ("data", cx.paths.data_dir()),
        ("state", cx.paths.state_dir()),
        ("cache", cx.paths.cache_dir()),
    ] {
        std::fs::create_dir_all(dir)?;
        println!("ok: {label} directory {}", dir.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum PluginSource {
    /// A bundled tree next to the binary (#345).
    Bundled(PathBuf),
    /// A totsuka checkout to `cargo build` in (#346).
    Checkout(PathBuf),
    /// Neither; the user is told what to run.
    Unavailable,
}

impl PluginSource {
    fn detect(explicit: Option<&Path>) -> PluginSource {
        // An explicit root is taken as given — never falling through to a
        // checkout build, which is the whole point of pinning it.
        if let Some(root) = bundled::locate(explicit) {
            return PluginSource::Bundled(root);
        }
        if explicit.is_none()
            && let Ok(cwd) = std::env::current_dir()
            && let Some(root) = from_source::find_checkout_root(&cwd, &from_source::is_checkout)
        {
            return PluginSource::Checkout(root);
        }
        PluginSource::Unavailable
    }
}

/// Install the selected plugins — **without enabling them**.
fn install_plugins(
    cx: &Cx,
    selected: &BTreeSet<String>,
    source: &PluginSource,
) -> Result<(), CliError> {
    if selected.is_empty() {
        return Ok(());
    }
    match source {
        PluginSource::Bundled(root) => {
            println!();
            println!("Installing plugins from {}", root.display());
        }
        PluginSource::Checkout(root) => {
            println!();
            println!("Building plugins from {}", root.display());
        }
        PluginSource::Unavailable => {
            println!();
            println!("No plugins to install from: this `totsuka` ships none and you are not in");
            println!("a checkout. Install them, then re-run `totsuka setup`:");
            for name in selected {
                println!("  totsuka plugin install <dir-with-{name}>");
            }
            return Ok(());
        }
    }

    for name in selected {
        plugin_cmd::run(
            cx,
            plugin_cmd::PluginCommand::Install {
                source: Some(name.clone()),
                bundled: matches!(source, PluginSource::Bundled(_)),
                from_source: matches!(source, PluginSource::Checkout(_)),
                repo: match source {
                    PluginSource::Checkout(root) => Some(root.clone()),
                    _ => None,
                },
                bundled_dir: match source {
                    PluginSource::Bundled(root) => Some(root.clone()),
                    _ => None,
                },
                all: false,
                // Deliberately not enabled: `config validate` launches every
                // enabled plugin, and the settings it needs are still
                // commented out. See the module docs.
                enable: false,
                yes: true,
                profile: plugin_cmd::BuildProfile::Release,
                print_plan: false,
            },
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// What to do next
// ---------------------------------------------------------------------------

fn print_next_steps(cx: &Cx, selected: &BTreeSet<String>, backend: SecretBackend) {
    let path = cx.config_path.display();
    println!();
    println!("Your configuration is at:");
    println!();
    println!("    {path}");
    println!();
    println!("**Nothing in it is active yet.** Every setting totsuka understands is in");
    println!("there, commented out, with a line saying what it does. Open it and uncomment");
    println!("what you need — at minimum:");
    println!();
    println!("  1. a `[[repositories]]` block: the clone tasks are dispatched into");
    if !selected.is_empty() {
        println!(
            "  2. `[plugins.<name>] enabled = true` for the plugins you just installed ({})",
            selected.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        println!("  3. that plugin's own `[<name>]` table, including its token reference");
        println!("  4. a `[[projects]]` block and a `[[workflows]]` block — the recipe section");
        println!("     at the end of the file has combinations that work, ready to uncomment");
    } else {
        println!("  2. a `[[projects]]` block and a `[[workflows]]` block — the recipe section");
        println!("     at the end of the file has combinations that work, ready to uncomment");
    }

    let accounts = template::secret_accounts(selected);
    if !accounts.is_empty() {
        println!();
        println!(
            "Secrets the file references ({backend}) — setup never handles the values, only \
             the references:"
        );
        println!();
        for account in &accounts {
            println!(
                "  {}  — {}",
                backend.reference(account),
                secrets::purpose_of(account)
            );
            if let Some(command) = backend.register_command(account) {
                println!("    {command}");
            }
        }
        if let Some(note) = backend.register_note() {
            println!();
            println!("  {note}");
        }
        println!();
        println!("  Register only the ones whose lines you actually uncomment: an unregistered");
        println!("  reference stops that plugin from starting.");
    }

    println!();
    println!("Then:");
    println!();
    println!("    totsuka config validate      # the config parses and hangs together");
    println!("    totsuka doctor               # the environment around it is ready");
    println!("    totsuka run --dry-run        # what one cycle would do");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<template::KnownPlugin> {
        template::known_plugins()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// **An empty list is refused, not read as "none".** It is the same state
    /// the TTY gate exists to rule out: a run that installs nothing and
    /// reports success.
    #[test]
    fn an_empty_plugin_list_is_refused_and_points_at_none() {
        for spec in ["", "   ", ",", " , "] {
            let err = parse_plugins(spec, &known()).unwrap_err().to_string();
            assert!(err.contains("--plugins none"), "for {spec:?}: {err}");
        }
    }

    #[test]
    fn plugins_all_and_none_are_the_two_bulk_answers() {
        assert_eq!(parse_plugins("all", &known()).unwrap().len(), known().len());
        assert!(parse_plugins("none", &known()).unwrap().is_empty());
    }

    #[test]
    fn plugins_takes_a_comma_separated_list_and_tolerates_spaces() {
        assert_eq!(
            parse_plugins("github, herdr ,macos", &known()).unwrap(),
            set(&["github", "herdr", "macos"])
        );
    }

    /// **A typo is refused, not silently dropped.** `--plugins gihub`
    /// installing nothing looks exactly like a run that worked.
    #[test]
    fn an_unknown_plugin_name_is_an_error_that_lists_the_real_ones() {
        let err = parse_plugins("gihub", &known()).unwrap_err().to_string();
        assert!(err.contains("gihub"), "{err}");
        assert!(err.contains("github"), "{err}");
    }

    /// **Root-level content is never appended.** The skeleton's one live line,
    /// `version = 1`, sits under no table; appending it to a file that ends
    /// inside `[[workflows]]` makes it that workflow's key, which
    /// `deny_unknown_fields` rejects — and appending it to one that ends at
    /// root level is a duplicate key. Neither is recoverable by the operator
    /// without reading the diff.
    #[test]
    fn nothing_outside_a_table_is_ever_appended() {
        let rendered = template::render(&set(&["github"]), SecretBackend::OnePassword);
        assert!(
            rendered.contains("\nversion = 1"),
            "fixture assumption: the skeleton sets version"
        );
        let appended: String = blocks(&rendered).iter().map(|b| b.text.clone()).collect();
        assert!(
            !appended.lines().any(|l| l.trim() == "version = 1"),
            "a root-level key reached the append path"
        );
        assert!(
            !appended
                .lines()
                .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')),
            "every appended line must be a comment"
        );
    }

    /// **A plugin picked on a later run gets its roster entry too.** All seven
    /// `[plugins.<name>]` entries share one banner, so a section-sized append
    /// unit would call the roster "already there" and add `[notion]` with
    /// nothing to enable it by — the exact scenario ADR-0077 decision 6 exists
    /// for.
    #[test]
    fn a_plugin_added_later_brings_its_roster_entry() {
        let first = template::render(&set(&["github"]), SecretBackend::OnePassword);
        let second = template::render(&set(&["notion"]), SecretBackend::OnePassword);
        let missing: Vec<String> = blocks(&second)
            .into_iter()
            .filter(|b| !b.is_present_in(&first))
            .map(|b| b.path)
            .collect();
        assert!(
            missing.contains(&"plugins.notion".to_string()),
            "the roster entry was not appended: {missing:?}"
        );
        assert!(
            missing.contains(&"notion".to_string()),
            "the settings table was not appended: {missing:?}"
        );
        assert!(
            !missing.contains(&"plugins.github".to_string()),
            "an entry the file already has must not be added twice: {missing:?}"
        );
    }

    /// Re-running with the same selection changes nothing.
    ///
    /// Nothing in a generated file is active, so presence cannot be judged by
    /// live tables alone — without the commented form counting too, every
    /// re-run would append the whole file again.
    #[test]
    fn a_rerun_with_the_same_selection_appends_nothing() {
        let rendered = template::render(&set(&["github", "herdr"]), SecretBackend::OnePassword);
        let missing: Vec<String> = blocks(&rendered)
            .into_iter()
            .filter(|b| !b.is_present_in(&rendered))
            .map(|b| b.path)
            .collect();
        assert!(missing.is_empty(), "would append again: {missing:?}");
    }

    /// **A live table stops its block from being appended.** Adding a
    /// commented block about `[github]` below a `[github]` the operator wrote
    /// would read as a second, contradictory definition.
    #[test]
    fn a_table_the_config_already_defines_is_not_appended() {
        let rendered = template::render(&set(&["github"]), SecretBackend::OnePassword);
        let github = blocks(&rendered)
            .into_iter()
            .find(|b| b.path == "github")
            .expect("the [github] block");
        assert!(github.is_present_in("[github]\ntoken = \"op://Dev/totsuka/github-token\"\n"));
        assert!(!github.is_present_in("[worktree]\ncleanup = \"manual\"\n"));
    }
}
