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
    /// Section titles the append is made of, for the printed plan.
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
                let missing: Vec<Section> = sections(rendered)
                    .into_iter()
                    .filter(|s| !s.title.is_empty() && !s.is_present_in(existing))
                    .collect();
                if missing.is_empty() {
                    (ConfigWrite::Unchanged, Vec::new())
                } else {
                    let titles = missing.iter().map(|s| s.title.clone()).collect();
                    let text = missing
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    (ConfigWrite::Append(text), titles)
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
                "           append {} section(s) it does not have yet:\n{}",
                self.added.len(),
                self.added
                    .iter()
                    .map(|t| format!("             {t}\n"))
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

/// One `# ===` banner block of the rendered skeleton.
struct Section {
    /// The banner's title line, which identifies the section.
    title: String,
    /// The block itself, banner included.
    text: String,
    /// Table paths the block documents (`github`, `plugins.github`, …).
    tables: Vec<String>,
}

impl Section {
    /// Whether `existing` already has this section — either verbatim, or as
    /// tables the operator wrote themselves.
    ///
    /// Both halves matter. The title catches a config this command wrote
    /// before; the tables catch a hand-written one, where appending a
    /// commented block about `[github]` under a live `[github]` would read as
    /// a second, contradictory definition.
    fn is_present_in(&self, existing: &str) -> bool {
        if existing.lines().any(|l| l.trim_end() == self.title) {
            return true;
        }
        self.tables.iter().any(|path| {
            existing.lines().any(|line| {
                let line = line.trim();
                !line.starts_with('#')
                    && (line.starts_with(&format!("[{path}]"))
                        || line.starts_with(&format!("[[{path}]]")))
            })
        })
    }
}

/// The banner line the skeleton separates its sections with.
fn banner_prefix() -> &'static str {
    "# ========"
}

/// Split rendered text into banner-delimited sections.
///
/// The first one is the preamble above the first banner; it has no title and
/// is never appended to an existing file.
fn sections(rendered: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current = Section {
        title: String::new(),
        text: String::new(),
        tables: Vec::new(),
    };
    let mut lines = rendered.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(banner_prefix()) && !current.text.is_empty() {
            // A banner opens a section only when it is the *first* of the pair
            // wrapping a title; the closing one is part of the same block.
            if current.text.lines().next_back().map(str::trim) != Some("")
                && !current.title.is_empty()
            {
                current.text.push_str(line);
                current.text.push('\n');
                continue;
            }
            sections.push(std::mem::replace(
                &mut current,
                Section {
                    title: lines
                        .peek()
                        .map(|t| t.trim_end().to_string())
                        .unwrap_or_default(),
                    text: String::new(),
                    tables: Vec::new(),
                },
            ));
        }
        current.text.push_str(line);
        current.text.push('\n');
        if let Some(path) = table_path(line) {
            current.tables.push(path);
        }
    }
    sections.push(current);
    sections
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

    /// The preamble is never appended to someone's existing config: it is the
    /// file's opening explanation, not a section.
    #[test]
    fn the_preamble_has_no_title_and_the_rest_do() {
        let rendered = template::render(&set(&["github"]), SecretBackend::OnePassword);
        let sections = sections(&rendered);
        assert!(sections[0].title.is_empty());
        assert!(
            sections.iter().skip(1).all(|s| !s.title.is_empty()),
            "every section after the preamble is titled"
        );
        assert!(
            sections.iter().any(|s| s.title.contains("[github]")),
            "titles: {:?}",
            sections.iter().map(|s| &s.title).collect::<Vec<_>>()
        );
    }

    /// **A live table stops its section from being appended.** Adding a
    /// commented block about `[github]` below a `[github]` the operator wrote
    /// would read as a second, contradictory definition.
    #[test]
    fn a_section_the_config_already_has_is_not_appended() {
        let rendered = template::render(&set(&["github"]), SecretBackend::OnePassword);
        let github = sections(&rendered)
            .into_iter()
            .find(|s| s.title.contains("[github] —"))
            .expect("the github section");
        assert!(github.is_present_in("[github]\ntoken = \"op://Dev/totsuka/github-token\"\n"));
        assert!(!github.is_present_in("[worktree]\ncleanup = \"manual\"\n"));
        assert!(
            !github.is_present_in("# [github]\n# token = \"…\"\n"),
            "a commented table is documentation, not a definition"
        );
    }
}
