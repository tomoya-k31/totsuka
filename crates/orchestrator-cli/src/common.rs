//! Shared CLI plumbing: path/config resolution and the "cause + next action"
//! error convention (§7). Plugin-spec assembly and secret resolution live in
//! core (`orchestrator_core::plugins::spec` / `config::resolve`, #217).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::StateDb;
use orchestrator_core::config::{self, Finding, FindingSeverity, RootConfig};
use orchestrator_core::paths::Paths;
use orchestrator_core::plugins::PluginStore;

/// A boxed error for CLI operations.
pub type CliError = Box<dyn std::error::Error>;

// Process exit codes (#177; the table lives in
// docs/components/orchestrator-cli.md). 0 = success is expressed as
// `ExitCode::SUCCESS` at the call site; 2 also covers clap's own parse
// failures, which exit before `main`'s mapping is reached.

/// A runtime failure (any error without a more specific code).
pub const EXIT_ERROR: u8 = 1;
/// A usage error (no/unknown subcommand, bad flags).
pub const EXIT_USAGE: u8 = 2;
/// Diagnostics ran to completion and found problems (`totsuka doctor`).
/// Distinct from [`EXIT_ERROR`] so scripts can tell "doctor itself failed"
/// from "doctor worked and the environment has issues".
pub const EXIT_PROBLEMS_FOUND: u8 = 3;

/// A failure that maps to a specific process exit code. `main` downcasts the
/// returned [`CliError`] to this; any other error exits [`EXIT_ERROR`]. The
/// message keeps the "cause → next action" convention (§7) like every other
/// CLI error.
#[derive(Debug)]
pub struct ExitWith {
    /// The process exit code to return.
    pub code: u8,
    /// The "cause → next action" message.
    pub message: String,
}

impl ExitWith {
    /// A failure exiting with `code` and the conventional message.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExitWith {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitWith {}

/// The shared `--json` flag (machine-readable contract: parseable output on
/// stdout, nothing else). Flattened into every command that supports JSON
/// output so the flag is declared once.
#[derive(Debug, Default, clap::Args)]
pub struct JsonFlag {
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// Resolved locations every command operates on. `--config` overrides the
/// config file path (highest layer of F-66); everything else stays XDG.
pub struct Cx {
    /// XDG-resolved application directories.
    pub paths: Paths,
    /// Path of `config.toml` (possibly overridden by `--config`).
    pub config_path: PathBuf,
}

impl Cx {
    /// Resolve paths, honoring a `--config <path>` override.
    pub fn resolve(config_override: Option<&Path>) -> Result<Self, CliError> {
        let paths = Paths::from_system()?;
        let config_path = match config_override {
            Some(path) => path.to_path_buf(),
            None => paths.config_dir().join("config.toml"),
        };
        Ok(Self { paths, config_path })
    }

    /// Load and parse `config.toml` and apply the `TOTSUKA_*` overrides
    /// (F-66 layer 2), with an actionable error when the file is absent.
    ///
    /// Every command that reads config goes through here so the layers stay
    /// consistent: `totsuka focus` / `doctor` resolve `[hooks].socket_path` to
    /// find the socket `totsuka run` bound, so applying the env layer in `run`
    /// alone would leave them looking at a different socket. CLI flags are
    /// applied by the individual commands *after* this call, which is what
    /// makes "CLI > env" hold.
    ///
    /// Warnings (unknown `TOTSUKA_*` names, empty values) go to stderr so the
    /// `--json` commands' stdout contract stays parseable.
    pub fn load_config(&self, env: &HashMap<String, String>) -> Result<RootConfig, CliError> {
        match std::fs::read_to_string(&self.config_path) {
            Ok(s) => self.parse_and_overlay(&s, env),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
                "config not found at {} → run `totsuka init` to create it",
                self.config_path.display()
            )
            .into()),
            Err(e) => Err(e.into()),
        }
    }

    /// Like [`Cx::load_config`], but a missing file is not an error: the
    /// empty default config is used instead (#175). The `TOTSUKA_*` env layer
    /// still applies on top — so an invalid override value fails here exactly
    /// like it does with a present file. For the commands that must work
    /// before `totsuka init` — `plugin install` / `uninstall` / `list` only
    /// consult the config to cross-check declarations, so an absent file
    /// simply means "nothing declared". Every other command errors via
    /// [`Cx::load_config`]; which command gets which behavior is documented
    /// in docs/components/orchestrator-cli.md.
    pub fn load_config_or_default(
        &self,
        env: &HashMap<String, String>,
    ) -> Result<RootConfig, CliError> {
        match std::fs::read_to_string(&self.config_path) {
            Ok(s) => self.parse_and_overlay(&s, env),
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.parse_and_overlay("", env),
            Err(e) => Err(e.into()),
        }
    }

    /// Parse a config body and apply the `TOTSUKA_*` overrides (F-66 layer 2).
    fn parse_and_overlay(
        &self,
        s: &str,
        env: &HashMap<String, String>,
    ) -> Result<RootConfig, CliError> {
        let mut cfg = RootConfig::from_toml_str(s)?;
        let warnings =
            config::apply_env_overrides(&mut cfg, env.iter().map(|(k, v)| (k.clone(), v.clone())))?;
        for warning in warnings {
            eprintln!("config warning: {warning}");
        }
        Ok(cfg)
    }

    /// Path of the state database.
    pub fn state_db_path(&self) -> PathBuf {
        self.paths.state_dir().join("state.db")
    }

    /// Open the state DB for every CLI command **except** `run`, with an
    /// actionable error when it does not exist yet (opening would silently
    /// create an empty one).
    ///
    /// Uses the **non-migrating** entry point (#275). The dividing line is
    /// `run.lock`, not read vs write: `task cancel` / `retry` / `verify`
    /// write through here too, they just do not hold the lock. Letting any
    /// of them migrate meant two processes could start changing the schema
    /// of the same file at once right after an upgrade. Pending migrations
    /// now surface as `SchemaOutdated`, pointing the operator at
    /// `totsuka run`.
    pub fn open_state_db(&self) -> Result<StateDb, CliError> {
        let path = self.state_db_path();
        if !path.exists() {
            return Err(format!(
                "state database not found at {} → run `totsuka run` at least once",
                path.display()
            )
            .into());
        }
        Ok(StateDb::open_no_migrate(&path)?)
    }

    /// The plugin store under the data directory.
    pub fn store(&self) -> PluginStore {
        PluginStore::new(self.paths.data_dir().join("plugins"))
    }

    /// Run the full offline config validation (static checks, workflow
    /// semantics, hook advisories) against the installed plugin manifests.
    /// Shared by `config validate`, `run`, and `doctor` so all three agree on
    /// what a broken config means.
    ///
    /// Manifest health comes first (#214): an enabled plugin whose
    /// `plugin.toml` exists but does not parse is an **error** finding, even
    /// offline — reading the manifest launches nothing, so F-63 holds. The
    /// capability closures below still fold `Err` into `None` ("unknown",
    /// which skips the capability-based advisories); that stays safe only
    /// because the error above already fails validation — without it, a broken
    /// manifest would silently disable those checks.
    pub fn validate_config(&self, cfg: &RootConfig, env: &HashMap<String, String>) -> Vec<Finding> {
        let env_fn = |k: &str| env.get(k).cloned();
        let store = self.store();
        let mut findings: Vec<Finding> = cfg
            .plugins
            .iter()
            .filter(|(_, p)| p.enabled)
            .filter_map(|(name, _)| store.manifest_of(name).err().map(|e| (name, e)))
            .map(|(name, e)| Finding {
                severity: FindingSeverity::Error,
                // `e` already says what is wrong ("invalid plugin.toml: …"
                // for a parse failure), so no prefix beyond the plugin name.
                message: format!(
                    "plugin `{name}`: {e} → reinstall it (`totsuka plugin install <dir>`)"
                ),
            })
            .collect();
        findings.extend(config::validate(
            cfg,
            &env_fn,
            |name| {
                store
                    .manifest_of(name)
                    .ok()
                    .flatten()
                    .map(|m| m.capabilities.outputs)
            },
            |name| {
                store
                    .manifest_of(name)
                    .ok()
                    .flatten()
                    .map(|m| m.capabilities.hook_capable())
            },
        ));
        findings
    }

    /// The `plugins/` directory itself (next to config.toml), holding one
    /// `{name}.toml` per plugin.
    pub fn plugin_config_dir(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins")
    }
}

/// The hook/control UDS socket path: `[hooks].socket_path` (with `~`/`${ENV}`
/// expanded) or the XDG runtime default. Shared by `doctor`'s probe and
/// `totsuka focus` so both always target the socket `totsuka run` binds.
pub fn hook_socket_path(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
) -> Result<PathBuf, CliError> {
    let env_fn = |k: &str| env.get(k).cloned();
    match &cfg.hooks.socket_path {
        Some(raw) => config::expand_path(raw, &env_fn)
            .map_err(|e| format!("[hooks].socket_path does not expand: {e}").into()),
        None => Ok(cx.paths.runtime_dir().join("agent-events.sock")),
    }
}

/// Print a value as pretty JSON (the `--json` contract: parseable output on
/// stdout, nothing else).
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// True for characters that let text rewrite the screen rather than appear on
/// it (#280).
///
/// [`char::is_control`] covers C0, DEL and C1 — the escape-sequence
/// introducers. The bidi overrides and isolates are *not* control characters
/// by that definition (they are `Cf`), but they forge the reading order of a
/// line, which is the same attack on the same screen, so they go through the
/// same door.
fn is_screen_control(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Render `text` safely for a terminal: control characters become their
/// visible escaped form (`\u{1b}`, `\n`, `\t`, …), so the string can only be
/// *read*, never *executed* by the terminal.
///
/// Titles, bodies, authors, urls and source ids are written by whoever can
/// post in the connected Slack channel or file the GitHub issue — which may
/// include people outside the team. Printed verbatim they can repaint the
/// operator's screen: `ESC[2J` clears it, `ESC[1A` walks the cursor back over
/// a row already printed (so one task's state can be made to show another's),
/// OSC 8 makes the displayed text and the real link disagree, and OSC 52
/// writes to the clipboard on terminals that honour it. None of that is code
/// execution, but all of it breaks the assumption that reading CLI output
/// tells you the truth.
///
/// Escaping rather than stripping is deliberate: a deleted character is a
/// character the operator cannot see was ever there, which is its own way of
/// lying about the content.
///
/// **Human output only.** `--json` goes through [`print_json`], and
/// `serde_json` already escapes control characters as `\u00xx`; sending it
/// through here as well would double-escape values that a machine, not a
/// terminal, is going to read.
pub fn safe(text: &str) -> Cow<'_, str> {
    // Almost every title is clean, and `task list` calls this once per row —
    // borrow rather than allocate when there is nothing to rewrite.
    if !text.contains(is_screen_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_screen_control(c) {
            // `escape_debug` spells the familiar ones `\n` / `\r` / `\t` and
            // falls back to `\u{...}` for everything else.
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of `safe` is that the string can still be *read* — escaping
    /// must not become its own way of destroying the content.
    #[test]
    fn safe_leaves_ordinary_text_exactly_as_it_was() {
        for text in [
            "リポジトリ選択のバグを直す",
            "deploy 🚀 done — 100% ✅",
            "https://example.com/a?b=c&d=e",
            "slack:C0123ABC:1700000000.000100",
            r"C:\Users\dev\repo",
            "",
        ] {
            assert_eq!(safe(text), text, "rewrote clean text: {text:?}");
            // Clean text is borrowed, not rebuilt — `task list` calls this
            // once per row.
            assert!(matches!(safe(text), Cow::Borrowed(_)), "{text:?}");
        }
    }

    /// The three sequences named in #280, plus the bidi override. Each must
    /// survive as visible text (nothing silently dropped) and must not carry
    /// a live control byte through to the terminal.
    #[test]
    fn safe_defuses_the_sequences_that_repaint_a_terminal() {
        let esc = char::from_u32(0x1b).unwrap();

        // ESC[2J — clear screen. ESC[1A — cursor up, i.e. overwrite the row
        // already printed above (forging another task's state).
        let clear = format!("title{esc}[2J");
        let up = format!("title{esc}[1A{esc}[2Kfake");
        // OSC 8 — displayed text and real link disagree.
        let osc8 = format!("{esc}]8;;https://evil.example{esc}\\click me{esc}]8;;{esc}\\");
        // CR alone rewrites the current row from column 0.
        let cr = "real title\rFORGED";
        // U+202E flips the reading order of everything after it.
        let bidi = "invoice\u{202e}gnp.exe";

        for raw in [
            clear.as_str(),
            up.as_str(),
            osc8.as_str(),
            cr,
            bidi,
            "bell\u{7}",
            "nul\u{0}",
        ] {
            let out = safe(raw);
            assert!(
                !out.chars().any(is_screen_control),
                "a live control character survived: {out:?}"
            );
            // The visible payload is still there to be read.
            assert!(out.len() >= raw.len(), "text shrank: {raw:?} -> {out:?}");
        }

        // Spot-check the actual rendering rather than only the property.
        assert_eq!(safe("real title\rFORGED"), r"real title\rFORGED");
        assert_eq!(safe(&format!("x{esc}y")), r"x\u{1b}y");
        assert_eq!(safe("a\nb"), r"a\nb");
        assert_eq!(safe("a\tb"), r"a\tb");
        assert_eq!(safe("invoice\u{202e}gnp.exe"), r"invoice\u{202e}gnp.exe");
    }

    /// A row must stay a row: whatever we print, the terminal sees exactly one
    /// line, so a table cannot be made to grow or shrink rows.
    #[test]
    fn safe_output_is_always_a_single_line() {
        let esc = char::from_u32(0x1b).unwrap();
        for raw in [
            "one\ntwo\nthree",
            "one\r\ntwo",
            &format!("a{esc}[2Jb"),
            "trailing\n",
        ] {
            let out = safe(raw);
            assert_eq!(out.lines().count().max(1), 1, "row split: {out:?}");
            assert!(!out.contains('\n'), "row split: {out:?}");
        }
    }
}
