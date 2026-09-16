//! `totsuka doctor` — environment diagnosis (§5.1, F-24): git, config, state
//! DB, installed plugins (with a live probe), LLM key resolution, and orphan
//! worktrees (with an interactive cleanup proposal).

use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use orchestrator_core::adapters::StateError;
use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::{OpenAiConfig, OpenAiRouter};
use orchestrator_core::adapters::plugin_host;
// Aliased on purpose: `plugin_protocol::manifest::PluginKind` (the manifest's
// declaration) also appears in this file, and the two are different types.
// This one is the config roster's, which is readable without touching the
// plugin — the property #289 needs.
use orchestrator_core::config::{
    self, PluginKind as ConfigPluginKind, RootConfig, secret_resolver,
};
use orchestrator_core::plugins::claims::ClaimRegistry;
use orchestrator_core::ports::git::GitRunner;
use orchestrator_core::ports::{SecretRef, SecretString};
use orchestrator_core::worktree::WorktreeManager;
use serde::Serialize;

use orchestrator_core::plugins::plugin_spec;

use crate::bundled;
use crate::common::{self, CliError, Cx, safe};
use crate::init_cmd::git_version;

/// `serde` `skip_serializing_if` predicate: omit a `false` flag from the JSON.
fn is_false(b: &bool) -> bool {
    !*b
}

/// One diagnostic result. `action` follows the "cause + next action" rule (§7).
///
/// Four severities: an `ok` check passes silently; a `warning` is advisory
/// (`ok` stays true, so it never fails `doctor`) yet still carries an action;
/// a `skipped` check **did not run at all** and says why; a failure
/// (`ok = false`) is what makes `doctor` exit non-zero.
///
/// `skipped` exists because "passed" and "never ran" were previously
/// indistinguishable (#289). Both `warning` and `skipped` are
/// `skip_serializing_if`, so a consumer that never saw them still parses the
/// `--json` document unchanged.
#[derive(Debug, Serialize)]
struct Check {
    name: String,
    ok: bool,
    /// Advisory finding: reported with its action but does not fail `doctor`.
    #[serde(skip_serializing_if = "is_false")]
    warning: bool,
    /// The check did not run. `ok` stays true — not running is not a failure —
    /// but the operator (and `--json`) must be able to tell it apart from a
    /// check that ran and passed.
    #[serde(skip_serializing_if = "is_false")]
    skipped: bool,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: true,
            warning: false,
            skipped: false,
            detail: detail.into(),
            action: None,
        }
    }
    fn fail(name: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: false,
            warning: false,
            skipped: false,
            detail: detail.into(),
            action: Some(action.into()),
        }
    }
    /// An advisory finding: `ok` (does not fail `doctor`) but surfaced with an
    /// action so the operator can act if they choose (e.g. a spool backlog).
    fn warn(name: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: true,
            warning: true,
            skipped: false,
            detail: detail.into(),
            action: Some(action.into()),
        }
    }
    /// The check was deliberately not run. `detail` says why, `action` says how
    /// to make it runnable — reporting nothing at all would read as "fine"
    /// (#289).
    fn skip(name: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: true,
            warning: false,
            skipped: true,
            detail: detail.into(),
            action: Some(action.into()),
        }
    }
}

/// Whether one secret backend's CLI can resolve a reference without prompting
/// (#289).
///
/// [ADR-0006](../../ai-docs/decisions/adr-0006-onepassword-secret-backend.md)
/// requires `doctor` to stay non-interactive, but a shell-out backend only
/// prompts when no session is established. So rather than approximating with
/// "is there a TTY", doctor asks the question directly, with a query the CLI
/// answers without prompting itself (`op whoami`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendReadiness {
    /// No reference for this backend anywhere in config: nothing can prompt,
    /// so no check needs gating.
    NotUsed,
    /// A session is established — the CLI answers from it. Probes that need a
    /// secret run exactly as before.
    Ready,
    /// The CLI is missing, broken, or has no session. Resolving would pop a
    /// prompt, or hang forever when nobody is watching.
    WouldPrompt,
}

impl BackendReadiness {
    /// Whether a probe that must resolve one of this backend's references may
    /// proceed.
    fn may_resolve(self) -> bool {
        !matches!(self, Self::WouldPrompt)
    }
}

/// A secret-reference scheme, as `doctor` needs to reason about it.
///
/// Every gate in doctor asks one question — "would resolving this reference
/// prompt, hang, or run something?" — and #444 showed the cost of letting each
/// site answer it for itself with its own `starts_with`: `cmd:` was wired into
/// 1 of the 3 gates and the other 2 kept resolving it behind the operator's
/// back until a follow-up fix.
///
/// [`SecretScheme::of`] is **exhaustive over [`SecretRef`] on purpose**. A new
/// variant in `orchestrator-core` breaks this compile, which is the only
/// mechanism that makes "doctor was never taught about the new scheme"
/// impossible rather than merely unlikely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretScheme {
    /// `keychain:`, `${ENV}`, and plain values — resolving is local and
    /// silent, so nothing needs gating.
    Silent,
    /// `op://` — resolving prompts unless a session is live, which
    /// [`check_onepassword`] measures.
    OnePassword,
    /// `cmd:` — resolving runs an arbitrary command. There is nothing to
    /// measure (`cmd:op read …` is a real spelling), so it is never resolved.
    Command,
    /// `bw:` — resolving prompts for the master password on **stdin** unless
    /// the vault is unlocked, which [`check_bitwarden`] measures. The stdin
    /// prompt is why this must be gated rather than merely reported: it hangs
    /// an unattended run silently instead of failing.
    Bitwarden,
}

impl SecretScheme {
    /// Classify a reference exactly as written in config.
    ///
    /// An unparseable string (`${ENV}`, a plain value, a malformed `op://…`)
    /// is [`Silent`](Self::Silent): none of them reach a backend, so none can
    /// prompt. A malformed reference fails in the resolver with
    /// `InvalidReference` without ever spawning a CLI.
    fn of(reference: &str) -> Self {
        match reference.parse::<SecretRef>() {
            Err(_) => Self::Silent,
            Ok(SecretRef::Keychain { .. }) => Self::Silent,
            Ok(SecretRef::OnePassword { .. }) => Self::OnePassword,
            Ok(SecretRef::Command { .. }) => Self::Command,
            Ok(SecretRef::Bitwarden { .. }) => Self::Bitwarden,
        }
    }

    /// The wording for skipping this scheme, ignoring session state — `None`
    /// when resolving it is silent.
    fn skip(self) -> Option<SecretSkip> {
        match self {
            Self::Silent => None,
            Self::OnePassword => Some(SecretSkip::ONEPASSWORD),
            Self::Command => Some(SecretSkip::COMMAND),
            Self::Bitwarden => Some(SecretSkip::BITWARDEN),
        }
    }
}

/// Why a probe that would resolve secrets is being skipped, and what to tell
/// the operator (§7).
///
/// One table for every scheme, so a new scheme writes its wording once instead
/// of once per gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SecretSkip {
    /// How the reference is named in prose ("an op:// reference").
    label: &'static str,
    /// Why resolution cannot happen; reads as the tail of "… but {detail}".
    detail: &'static str,
    /// Short phrase for the "not probed" roll-up in `check_plugins`.
    summary: &'static str,
    /// Next action. `{target}` is replaced with whatever would have been
    /// probed ("this plugin", "the receiver").
    action: &'static str,
    /// The parenthetical of the "left unresolved here" note, for checks that
    /// report on a reference rather than probing with it.
    note: &'static str,
    /// The same parenthetical when the backend *is* ready — `None` for a
    /// scheme that is never ready, because there is nothing to measure.
    note_ready: Option<&'static str>,
}

impl SecretSkip {
    /// `op://` — gated on the session `check_onepassword` measured.
    const ONEPASSWORD: Self = Self {
        label: "an op:// reference",
        detail: "resolving its op:// reference would prompt for 1Password unlock \
                 (doctor stays non-interactive)",
        summary: "its op:// reference would prompt",
        action: "run `op signin`, then re-run `totsuka doctor` to probe {target}",
        note: "doctor stays non-interactive; see the 1password checks above",
        note_ready: Some("a 1Password session is active, so `totsuka run` will resolve it"),
    };

    /// `cmd:` — unconditional, because there is no session to measure (#444).
    const COMMAND: Self = Self {
        label: "a cmd: reference",
        detail: "resolving its cmd: reference would execute a command \
                 (doctor stays non-interactive)",
        summary: "its cmd: reference would run a command",
        action: "the command runs when `totsuka run` resolves the config; \
                 test {target} by hand if unsure",
        note: "doctor stays non-interactive; the command runs when `totsuka run` \
               resolves the config",
        // A command has no session, so it is never "ready".
        note_ready: None,
    };

    /// `bw:` — gated on the vault state `check_bitwarden` measured.
    const BITWARDEN: Self = Self {
        label: "a bw: reference",
        detail: "resolving its bw: reference would prompt for the Bitwarden master \
                 password on stdin (doctor stays non-interactive)",
        summary: "its bw: reference would prompt",
        action: "run `bw unlock` and export BW_SESSION, then re-run `totsuka doctor` \
                 to probe {target}",
        note: "doctor stays non-interactive; see the bitwarden checks above",
        note_ready: Some("the Bitwarden vault is unlocked, so `totsuka run` will resolve it"),
    };

    /// The next action, naming what would have been probed.
    fn action(self, target: &str) -> String {
        self.action.replace("{target}", target)
    }
}

/// Whether `doctor` may resolve a reference right now.
///
/// Holds one [`BackendReadiness`] per *measurable* backend; a scheme with
/// nothing to measure needs no field. Gate sites ask this type instead of
/// testing prefixes themselves — that is what keeps a new scheme from being
/// wired into some gates and forgotten in others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SecretReadiness {
    /// The `op://` backend's session state.
    onepassword: BackendReadiness,
    /// The `bw:` backend's vault state.
    bitwarden: BackendReadiness,
}

impl SecretReadiness {
    /// What was measured for `scheme`.
    ///
    /// `Silent` is `NotUsed` (nothing to gate) and `Command` is permanently
    /// `WouldPrompt` (nothing to measure); the rest report their backend.
    fn readiness_of(self, scheme: SecretScheme) -> BackendReadiness {
        match scheme {
            SecretScheme::Silent => BackendReadiness::NotUsed,
            SecretScheme::OnePassword => self.onepassword,
            SecretScheme::Bitwarden => self.bitwarden,
            SecretScheme::Command => BackendReadiness::WouldPrompt,
        }
    }

    /// The skip for `reference`, or `None` when doctor may resolve it.
    fn skip_for(self, reference: &str) -> Option<SecretSkip> {
        let scheme = SecretScheme::of(reference);
        if self.readiness_of(scheme).may_resolve() {
            return None;
        }
        scheme.skip()
    }

    /// The skip for the first blocked string leaf under `value`, if any.
    ///
    /// Only *actual string values* count, so a commented-out example — like
    /// the one `totsuka init` generates — never gates anything.
    fn skip_in_toml(self, value: &toml::Value) -> Option<SecretSkip> {
        match value {
            toml::Value::String(s) => self.skip_for(s),
            toml::Value::Array(items) => items.iter().find_map(|v| self.skip_in_toml(v)),
            toml::Value::Table(table) => table.values().find_map(|v| self.skip_in_toml(v)),
            _ => None,
        }
    }

    /// The "left unresolved here" note for a check that *reports on* a
    /// reference instead of probing with it, or `None` when the reference is
    /// resolved normally.
    ///
    /// These sites read like pure reporting, but they are gates too: whatever
    /// this returns `None` for falls through to a real `resolve()` call. A
    /// scheme missing from [`SecretScheme`] would therefore be spawned here,
    /// prompting on stdin with nobody watching.
    fn deferred_note(self, reference: &str, subject: &str) -> Option<String> {
        let scheme = SecretScheme::of(reference);
        let skip = scheme.skip()?;
        let note = match (self.readiness_of(scheme), skip.note_ready) {
            (BackendReadiness::Ready, Some(ready)) => ready,
            _ => skip.note,
        };
        Some(format!(
            "{subject} is {}, left unresolved here ({note})",
            skip.label
        ))
    }
}

/// How this `doctor` invocation was asked to behave.
///
/// A struct rather than three positional `bool`s: they are all the same type,
/// so a swapped pair at a call site would compile and quietly change what runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct DoctorArgs {
    /// Emit the machine-readable report instead of the human one.
    pub json: bool,
    /// Opt into the live probes (#267) — the only checks that reach the network.
    pub online: bool,
    /// Inspect only: skip every write `doctor` would otherwise perform.
    ///
    /// `doctor` is deliberately not read-only by default. It re-materialises
    /// the hook assets, syncs `$CODEX_HOME/hooks.json` and the opencode
    /// assets, and creates the spool directory — the same writes `run` does,
    /// which is what lets `doctor` double as "finish the setup" (#137/#196).
    /// That leaves no way to express a pure audit: a read-only CI check, or a
    /// look at a machine you would rather not modify, still writes into the
    /// user's `$CODEX_HOME`. This flag is that way. See [`DoctorArgs::no_repair`]
    /// usages for exactly which writes it suppresses.
    pub no_repair: bool,
}

/// Execute `totsuka doctor`.
pub fn run(cx: &Cx, args: DoctorArgs) -> Result<(), CliError> {
    let DoctorArgs { json, .. } = args;
    let mut checks = Vec::new();
    // One environment snapshot, threaded through every check that needs it.
    let env: HashMap<String, String> = std::env::vars().collect();

    // git availability (worktrees are mandatory).
    match git_version() {
        Some(version) => checks.push(Check::ok("git", format!("git {version}"))),
        None => checks.push(Check::fail(
            "git",
            "git not found on PATH",
            "install git (worktree management requires it)",
        )),
    }

    // Which plugins ship next to this binary. A `cargo install` build has none,
    // which is normal — so this can never be worse than a warning, and the
    // 0/1/3 exit-code contract is unaffected.
    checks.push(match bundled::locate(None) {
        Some(root) => {
            let found = bundled::list(&root);
            if found.is_empty() {
                Check::warn(
                    "bundled-plugins",
                    format!("no plugins under {}", root.display()),
                    "reinstall from the release tarball, or install from a directory",
                )
            } else {
                Check::ok(
                    "bundled-plugins",
                    format!(
                        "{} in {} ({})",
                        found.len(),
                        root.display(),
                        found
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        }
        None => Check::warn(
            "bundled-plugins",
            "no plugins bundled next to this binary",
            "expected for a `cargo install` build — install plugins from a directory \
             (`totsuka plugin install <dir>`)",
        ),
    });

    // Config presence + full offline validation. `config_ok` gates the checks
    // with side effects outside totsuka's own dirs (codex hooks.json sync) —
    // a config that validation rejects must not cause writes `run` would
    // never perform (it aborts on errors before dispatch).
    let mut config_ok = false;
    let cfg = match cx.load_config(&env) {
        Ok(cfg) => {
            let findings = cx.validate_config(&cfg, &env);
            if config::has_errors(&findings) {
                let first = findings
                    .iter()
                    .find(|f| f.severity == config::FindingSeverity::Error)
                    .map(|f| f.message.clone())
                    .unwrap_or_default();
                checks.push(Check::fail(
                    "config",
                    format!("{} has errors (first: {first})", cx.config_path.display()),
                    "run `totsuka config validate` for the full list",
                ));
            } else {
                config_ok = true;
                checks.push(Check::ok(
                    "config",
                    format!("{} is valid", cx.config_path.display()),
                ));
            }
            Some(cfg)
        }
        Err(e) => {
            checks.push(Check::fail(
                "config",
                e.to_string(),
                "run `totsuka init`, then edit the generated config.toml",
            ));
            None
        }
    };

    // State DB.
    let db = match cx.open_state_db() {
        Ok(db) => {
            // Report the schema version and who applied it (#275): after an
            // upgrade or a rollback, "which schema is this DB on" is the
            // first thing worth knowing, and it is otherwise only visible by
            // running sqlite3 by hand.
            let schema = match db.schema_version() {
                Ok((version, applied_by)) => format!(
                    " — schema v{version} (applied by {})",
                    applied_by.as_deref().unwrap_or("unknown")
                ),
                Err(e) => format!(" — schema version unreadable: {e}"),
            };
            checks.push(Check::ok(
                "state-db",
                format!("{} opens{schema}", cx.state_db_path().display()),
            ));
            Some(db)
        }
        Err(e) => {
            let schema_mismatch = e.downcast_ref::<StateError>().is_some_and(|s| {
                matches!(
                    s,
                    StateError::SchemaTooNew { .. } | StateError::SchemaOutdated { .. }
                )
            });
            let msg = e.to_string();
            // The schema errors already carry their own `→ <action>` clause
            // (ADR-0012). Split it back out instead of appending a second
            // hint, which would render as two arrows on one line.
            let (detail, action) = match (schema_mismatch, msg.rsplit_once(" → ")) {
                (true, Some((cause, action))) => (cause.to_string(), action.to_string()),
                _ => (msg, "run `totsuka run` once to create it".to_string()),
            };
            checks.push(Check::fail("state-db", detail, action));
            None
        }
    };

    if let Some(cfg) = &cfg {
        // The backend probes go **first** (#289). Several checks below resolve
        // secrets, and `op read` / `bw get` prompt (or hang unattended)
        // without a session — so the answer to "may we resolve?" has to exist
        // before anything acts on it. Running this last, as it used to, meant
        // `check_plugins` had already resolved the very references the `llm`
        // and `hook-token` checks claimed the probes would cover.
        let secrets = SecretReadiness {
            onepassword: check_onepassword(cx, cfg, &env, &mut checks),
            bitwarden: check_bitwarden(cx, cfg, &env, &mut checks),
        };
        check_worktree_location(cfg, &env, &mut checks);
        check_hooks(cx, cfg, config_ok, &env, secrets, args, &mut checks);
        check_plugins(cx, cfg, &env, secrets, &mut checks);
        check_llm_key(cfg, &env, args, secrets, &mut checks);
        check_orphans(cfg, &env, db.as_ref(), args, &mut checks)?;
        check_orphan_panes(cx, cfg, &env, db.as_ref(), args, secrets, &mut checks)?;
    }

    if json {
        common::print_json(&checks)?;
    } else {
        // Every human line goes through `safe` *here*, after the `--json`
        // branch (#297). A `Check` can carry externally-authored text — a
        // pane label holds the source task id, an orphan worktree path holds
        // the branch built from the title, and git / tmux / plugin errors
        // quote whatever they were given — and doctor is read precisely when
        // something is already wrong. Sanitising the `Check` fields instead
        // would drag `--json` in with them, which must stay byte-exact.
        for check in &checks {
            let name = safe(&check.name);
            let detail = safe(&check.detail);
            let action = safe(check.action.as_deref().unwrap_or("see docs"));
            if !check.ok {
                println!("FAIL: {name} — {detail} → {action}");
            } else if check.skipped {
                println!("skip: {name} — {detail} → {action}");
            } else if check.warning {
                println!("warn: {name} — {detail} → {action}");
            } else {
                println!("ok:   {name} — {detail}");
            }
        }
    }
    if checks.iter().any(|c| !c.ok) {
        // Diagnostics ran to completion and found issues: exit 3, distinct
        // from a doctor execution failure (exit 1, any earlier `?`) so
        // scripts can tell the two apart (#177).
        return Err(common::ExitWith::new(
            common::EXIT_PROBLEMS_FOUND,
            "doctor found problems → follow the actions above",
        )
        .into());
    }
    Ok(())
}

/// The raw outcome of probing a secret-backend CLI.
///
/// Split from the spawning on purpose: the branching below is what decides
/// whether doctor resolves secrets, and it had **no test at all** while the
/// spawn was inlined. That is how the 1Password probes stayed dead code for
/// their whole life — `config_mentions_onepassword` parsed with the wrong TOML
/// entry point and always answered "no", and nothing noticed (#289).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CliProbe {
    /// The binary is not on PATH.
    Missing,
    /// `--version` ran but exited non-zero.
    VersionFailed {
        /// The exit code, or `-1` when the process was signalled.
        code: i32,
    },
    /// `--version` could not be spawned at all.
    Unrunnable {
        /// The spawn error, for the check's detail line.
        error: String,
    },
    /// `--version` succeeded.
    Probed {
        /// Whatever `--version` printed, trimmed.
        version: String,
        /// Whether the session query succeeded.
        session: bool,
    },
}

/// Run a backend CLI's two **non-prompting** probes: `--version` for presence,
/// then `session_args` for the session.
///
/// `session_args` must name a query that never prompts on its own — `op
/// whoami` and `bw status` do not, unlike `op read` and `bw get`. Getting that
/// wrong reopens exactly the unattended hang the gate exists to prevent.
///
/// `session_ok` reads the verdict out of the query's output, because the exit
/// code is not always the answer: `bw status` succeeds in every state and
/// reports the state in its JSON.
fn probe_cli(
    binary: &str,
    session_args: &[&str],
    session_ok: fn(&Output) -> bool,
    env: &HashMap<String, String>,
) -> CliProbe {
    let Some(path) = which(binary, env) else {
        return CliProbe::Missing;
    };
    match std::process::Command::new(&path).arg("--version").output() {
        Ok(out) if out.status.success() => CliProbe::Probed {
            version: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            session: std::process::Command::new(&path)
                .args(session_args)
                .output()
                .is_ok_and(|out| session_ok(&out)),
        },
        Ok(out) => CliProbe::VersionFailed {
            code: out.status.code().unwrap_or(-1),
        },
        Err(e) => CliProbe::Unrunnable {
            error: e.to_string(),
        },
    }
}

/// Everything that differs between one shell-out secret backend's probes and
/// another's.
///
/// A table rather than a function per backend: the two must stay symmetric —
/// a backend that is measured less precisely than its neighbour silently gets
/// a worse `doctor`, which is the failure this whole gate exists to avoid.
struct BackendProbe {
    /// Check name; the session check appends `-session`.
    name: &'static str,
    /// The binary as spelled on PATH.
    binary: &'static str,
    /// The scheme as written in config (`op://`, `bw:`).
    scheme: &'static str,
    /// Product name for prose (`1Password CLI (op)`).
    product: &'static str,
    /// Next action when the binary is absent or unrunnable.
    install: &'static str,
    /// Next action when `--version` misbehaves.
    reinstall: &'static str,
    /// The session query's argv. Must never prompt.
    session_args: &'static [&'static str],
    /// Reads the session verdict out of that query's output.
    session_ok: fn(&Output) -> bool,
    /// How a usable session reads.
    session_live: &'static str,
    /// How an unusable session reads.
    session_dead: &'static str,
    /// Next action for an unusable session.
    session_action: &'static str,
}

/// 1Password (#156): `op --version` + `op whoami` (which, unlike `op read`,
/// never triggers a biometric prompt).
const ONEPASSWORD_PROBE: BackendProbe = BackendProbe {
    name: "1password",
    binary: "op",
    scheme: "op://",
    product: "1Password CLI (op)",
    install: "install it (macOS: `brew install 1password-cli`, other platforms: \
              https://developer.1password.com/docs/cli) or switch the references to \
              `keychain:` / `${ENV}`",
    reinstall: "reinstall the 1Password CLI (macOS: `brew reinstall 1password-cli`)",
    session_args: &["whoami"],
    session_ok: |out| out.status.success(),
    session_live: "op session is active",
    session_dead: "no active 1Password session — probes that need an op:// secret are skipped",
    session_action: "run `op signin`, then re-run `totsuka doctor` for the full picture",
};

/// Bitwarden (#699): `bw --version` + `bw status`.
///
/// `bw status` is the counterpart of `op whoami` — it reports the vault state
/// and never prompts. It **exits 0 in every state**, so the verdict is the
/// `status` field of its JSON; reading the exit code instead would call a
/// locked vault ready and send the gated probes straight into the stdin
/// prompt.
const BITWARDEN_PROBE: BackendProbe = BackendProbe {
    name: "bitwarden",
    binary: "bw",
    scheme: "bw:",
    product: "Bitwarden CLI (bw)",
    install: "install it (macOS: `brew install bitwarden-cli`, other platforms: \
              https://bitwarden.com/help/cli/) or switch the references to \
              `keychain:` / `${ENV}`",
    reinstall: "reinstall the Bitwarden CLI (macOS: `brew reinstall bitwarden-cli`)",
    session_args: &["status", "--nointeraction"],
    session_ok: bw_vault_unlocked,
    session_live: "bw vault is unlocked",
    session_dead: "the Bitwarden vault is locked or BW_SESSION is not exported — probes \
                   that need a bw: secret are skipped",
    session_action: "run `bw unlock`, export the BW_SESSION it prints, then re-run \
                     `totsuka doctor` from that shell for the full picture",
};

/// Whether `bw status` reported an unlocked vault.
///
/// Anything unparseable counts as **locked**: skipping a probe costs a line of
/// output, while guessing "unlocked" costs an unattended run hanging on the
/// master-password prompt.
fn bw_vault_unlocked(out: &Output) -> bool {
    out.status.success()
        && serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .ok()
            .and_then(|v| v.get("status")?.as_str().map(str::to_string))
            .is_some_and(|status| status == "unlocked")
}

/// 1Password backend probes (#156), fired **only when** `config.toml` actually
/// contains an `op://` reference. No `op://` in config ⇒ no checks. Returns
/// whether the rest of `doctor` may resolve `op://` references without
/// prompting (#289).
fn check_onepassword(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) -> BackendReadiness {
    check_backend(
        &ONEPASSWORD_PROBE,
        SecretScheme::OnePassword,
        cx,
        cfg,
        env,
        checks,
    )
}

/// Bitwarden backend probes (#699), the exact counterpart of
/// [`check_onepassword`], fired only when `config.toml` contains a `bw:`
/// reference.
fn check_bitwarden(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) -> BackendReadiness {
    check_backend(
        &BITWARDEN_PROBE,
        SecretScheme::Bitwarden,
        cx,
        cfg,
        env,
        checks,
    )
}

/// Probe one backend's CLI, if its scheme reaches doctor at all.
fn check_backend(
    spec: &BackendProbe,
    scheme: SecretScheme,
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) -> BackendReadiness {
    if !scheme_in_use(cx, cfg, scheme) {
        return BackendReadiness::NotUsed;
    }
    backend_checks(
        spec,
        probe_cli(spec.binary, spec.session_args, spec.session_ok, env),
        checks,
    )
}

/// Turn a [`CliProbe`] into checks and a [`BackendReadiness`].
///
/// Pure, so every branch is reachable from a test without the real binaries
/// (CI has neither `op` nor `bw`, and the real ones need an account).
fn backend_checks(
    spec: &BackendProbe,
    probe: CliProbe,
    checks: &mut Vec<Check>,
) -> BackendReadiness {
    let (version, session) = match probe {
        CliProbe::Missing => {
            checks.push(Check::fail(
                spec.name,
                format!(
                    "config references {} secrets but the {} is not on PATH",
                    spec.scheme, spec.product
                ),
                spec.install,
            ));
            // No binary: every resolution would fail anyway, and the probes
            // that need one must not pretend otherwise.
            return BackendReadiness::WouldPrompt;
        }
        CliProbe::VersionFailed { code } => {
            checks.push(Check::fail(
                spec.name,
                format!("`{} --version` exited with {code}", spec.binary),
                spec.reinstall,
            ));
            return BackendReadiness::WouldPrompt;
        }
        CliProbe::Unrunnable { error } => {
            checks.push(Check::fail(
                spec.name,
                format!("cannot run `{}`: {error}", spec.binary),
                spec.install,
            ));
            return BackendReadiness::WouldPrompt;
        }
        CliProbe::Probed { version, session } => (version, session),
    };
    checks.push(Check::ok(
        spec.name,
        format!("{} {version} on PATH", spec.binary),
    ));
    // The session check is also the answer to "may the checks below resolve?"
    // — resolution prompts only when there is no session, so asking measures
    // the real condition instead of approximating it with a TTY test (#289).
    if session {
        checks.push(Check::ok(
            format!("{}-session", spec.name).as_str(),
            spec.session_live,
        ));
        BackendReadiness::Ready
    } else {
        checks.push(Check::warn(
            format!("{}-session", spec.name).as_str(),
            spec.session_dead,
            spec.session_action,
        ));
        BackendReadiness::WouldPrompt
    }
}

/// The next action for a set of skips: one clause per **distinct** reason.
///
/// Taking only the first reason would tell the operator to run `op signin`
/// (which does not unblock a `cmd:` agent) or to test by hand (which omits the
/// sign-in), while the detail line names every agent (Copilot review, #699).
///
/// Pulled out as a pure function so the mixed case has a test. The branch
/// exists *because* a review found the single-reason version wrong, and
/// shipping that fix untested would repeat exactly what let the probes stay
/// dead code in the first place (#289).
fn combined_skip_action(skips: &[SecretSkip], target: &str) -> String {
    let mut distinct: Vec<SecretSkip> = Vec::new();
    for skip in skips {
        if !distinct.contains(skip) {
            distinct.push(*skip);
        }
    }
    distinct
        .iter()
        .map(|skip| skip.action(target))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The false-negative note appended to every `agent-tool:*` failure.
///
/// A `const`, not a local, so `no_check_text_carries_collapsed_indentation` can
/// assert on it **unconditionally**. As a local it was only reachable through
/// the failure branch, which does not run on a machine where `gh` is set up —
/// the first version of that test passed while the bug it was written for was
/// still present.
///
/// `concat!`, not a `\`-continued literal: rustfmt collapses the continuation
/// onto one line and the indentation survives as a run of spaces in text the
/// operator reads.
const AGENT_TOOL_CAVEAT: &str = concat!(
    "if the tool is only reachable from the agent's pane (shell profile / mise), ",
    "this check is a false negative and can be ignored"
);

/// Whether the external tools the configured profiles need are usable here
/// (#399).
///
/// Emitted **only when something needs them** — a config of `answer`-only
/// workflows gets no line, because a check that always passes teaches the
/// reader to skip it.
///
/// # This check can be wrong, and says so
///
/// It runs in the CLI's environment. The agent runs in a pane with the user's
/// shell profile applied (`.zshenv`, `mise activate`, herdr's workspace env), so
/// a `gh` reachable only there reads as missing. The failure text says that
/// outright rather than leaving the operator to discover it: a check whose
/// false-negative mode is undocumented gets ignored entirely the first time it
/// is wrong.
///
/// # What it does not cover
///
/// `triage` and `design` write externally too, but where depends on the source
/// — and that is not something the Orchestrator can identify from a
/// user-chosen plugin instance name. The line says so when such a workflow
/// exists, rather than passing silently and reading as "checked".
fn check_agent_tools(cfg: &RootConfig, checks: &mut Vec<Check>) {
    use orchestrator_core::agent_tools::{self, AgentTool};
    use orchestrator_core::config::Profile;

    let mut needed: Vec<AgentTool> = Vec::new();
    let mut unchecked: Vec<&str> = Vec::new();
    for wf in &cfg.workflows {
        for tool in agent_tools::required(wf.profile) {
            if !needed.contains(tool) {
                needed.push(*tool);
            }
        }
        if matches!(wf.profile, Some(Profile::Triage | Profile::Design)) {
            unchecked.push(wf.name.as_str());
        }
    }
    if needed.is_empty() && unchecked.is_empty() {
        return; // nothing here writes outside its worktree
    }

    let caveat = AGENT_TOOL_CAVEAT;
    for tool in needed {
        let name = format!("agent-tool:{}", tool.as_str());
        if agent_tools::available(tool) {
            checks.push(Check::ok(
                &name,
                format!("{} is available and configured", tool.as_str()),
            ));
        } else {
            // **`warn`, not `fail`.** This check has a documented false
            // negative — the caveat below — and `fail` moves the exit code,
            // which would make `doctor` report a broken setup on a machine
            // where everything works. A check that can be wrong must not be
            // the one that says "stop"; the dispatch gate is what actually
            // protects the run, and it degrades to waiting rather than
            // failing for the same reason.
            checks.push(Check::warn(
                &name,
                format!(
                    "{} is not usable from here → implement-profile tasks will wait in the queue \
                     instead of stranding in the pane ({caveat})",
                    tool.as_str()
                ),
                tool.remedy(),
            ));
        }
    }
    if !unchecked.is_empty() {
        checks.push(Check::skip(
            "agent-tool:external-write",
            format!(
                "not checked for {}: a triage/design task writes to its source (GitHub via `gh`, \
                 Notion via MCP) and totsuka cannot tell which from a plugin instance name",
                unchecked.join(", ")
            ),
            "verify by hand that the agent can write to that source (`gh auth status`, or the \
             Notion MCP server in the agent's own config)",
        ));
    }
}

#[cfg(test)]
mod agent_tools_tests {
    use super::*;

    fn cfg_with(profile: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"
[[projects]]
name = "github"
source = "github"

[[workflows]]
name = "w"
projects = ["github"]
profile = "{profile}"
agent = "herdr"
"#
        ))
        .unwrap()
    }

    /// **No operator-visible string may contain a run of spaces.**
    ///
    /// `rustfmt` collapses a `\`-continued literal onto one line and the
    /// indentation survives inside the string, which reads as a typo in
    /// `totsuka doctor` output. It happened in this very function and only a
    /// reviewer caught it — a rendered-text assertion catches the next one.
    #[test]
    fn no_check_text_carries_collapsed_indentation() {
        let mut checks = Vec::new();
        check_agent_tools(&cfg_with("implement"), &mut checks);
        check_agent_tools(&cfg_with("design"), &mut checks);
        assert!(!checks.is_empty(), "the fixtures must produce checks");
        // Unconditionally, because the failure branch that carries it only
        // runs on a machine without `gh` — scanning the rendered checks alone
        // passed on a developer machine while the bug was present.
        assert!(!AGENT_TOOL_CAVEAT.contains("  "), "{AGENT_TOOL_CAVEAT:?}");
        for check in &checks {
            let texts = [Some(&check.detail), check.action.as_ref()];
            for text in texts.into_iter().flatten() {
                assert!(
                    !text.contains("  "),
                    "`{}` has a run of spaces: {text:?}",
                    check.name
                );
            }
        }
    }

    /// A config that writes nothing outside its worktree gets no line at all —
    /// a check that always passes teaches the reader to skip it.
    #[test]
    fn an_answer_only_config_produces_no_agent_tool_line() {
        let mut checks = Vec::new();
        check_agent_tools(&cfg_with("answer"), &mut checks);
        assert!(checks.is_empty(), "{checks:?}");
    }

    /// `design` is not checked, and says so rather than passing silently —
    /// silence would read as "checked and fine".
    #[test]
    fn design_reports_that_it_was_not_checked() {
        let mut checks = Vec::new();
        check_agent_tools(&cfg_with("design"), &mut checks);
        let skipped = checks
            .iter()
            .find(|c| c.name == "agent-tool:external-write")
            .unwrap_or_else(|| panic!("expected a skip line: {checks:?}"));
        assert!(skipped.detail.contains('w'), "{skipped:?}");
    }
}

/// The skip for launching `name`, when `plugin_spec` would resolve a reference
/// `doctor` must not resolve (#289, #444) — `None` when it is safe to probe.
///
/// Two independent doors, both inside `plugin_spec`: `plugin_init_config`
/// resolves **every string leaf** of the plugin's `[<name>]` table, and
/// `llm_info` resolves `[llm].api_key_ref` — but only for a task source.
/// Decided per plugin: one plugin's gated reference must not silence the
/// probes of plugins that need no secret at all.
///
/// "Task source" is asked of **both** the manifest and the config roster, and
/// either one saying yes is enough. `plugin_spec` itself branches on
/// `manifest.kind`, so the manifest is the authority — but the two can
/// disagree and nothing repairs it: `config validate` never reads
/// `manifest.kind` (it only checks the config's self-declared kind against
/// what a referencing workflow expects, and only when a workflow references
/// the plugin at all), and `plugin install` never writes config. A plugin
/// upgrade that changes its manifest kind therefore leaves the roster stale
/// indefinitely. Trusting either side alone would let that divergence reopen
/// the unattended hang, so this errs toward skipping.
///
/// An unreadable manifest needs no special case: `plugin_spec` reads it first
/// and fails before resolving anything.
fn plugin_secret_skip(
    cx: &Cx,
    cfg: &RootConfig,
    name: &str,
    readiness: SecretReadiness,
) -> Option<SecretSkip> {
    if let Some(skip) = cfg
        .plugin_settings(name)
        .and_then(|settings| readiness.skip_in_toml(settings))
    {
        return Some(skip);
    }
    let declared_task_source = cfg
        .plugin(name)
        .is_some_and(|p| p.kind == ConfigPluginKind::TaskSource);
    let manifest_task_source = cx
        .store()
        .manifest_of(name)
        .ok()
        .flatten()
        .is_some_and(|m| m.kind == plugin_protocol::manifest::PluginKind::TaskSource);
    if !(declared_task_source || manifest_task_source) {
        return None;
    }
    cfg.llm
        .as_ref()
        .and_then(|llm| llm.api_key_ref.as_deref())
        .and_then(|reference| readiness.skip_for(reference))
}

/// Whether a reference of `scheme` reaches `doctor` at all.
///
/// Checked against **both** the file on disk and the effective config, because
/// they are not the same document: `Cx::load_config` applies the `TOTSUKA_*`
/// env overrides *after* parsing, and two of them
/// (`TOTSUKA_HOOKS_AUTH_TOKEN_REF`, `TOTSUKA_LLM_API_KEY_REF`) carry secret
/// references.
///
/// Scanning only the file reports `NotUsed` for an `op://` supplied that way,
/// which opens the gate and lets `check_hook_socket` / `check_plugins` resolve
/// it for real — exactly the unattended prompt this gate exists to prevent
/// (Copilot review, #699).
fn scheme_in_use(cx: &Cx, cfg: &RootConfig, scheme: SecretScheme) -> bool {
    config_mentions_scheme(cx, scheme) || override_mentions_scheme(cfg, scheme)
}

/// The effective-config half of [`scheme_in_use`]: the two typed fields an env
/// override can replace after the file has been parsed.
///
/// Deliberately not a walk of the whole `RootConfig`: the file scan already
/// covers everything written in the document, and the override table is the
/// only way a reference can reach `doctor` without appearing there.
fn override_mentions_scheme(cfg: &RootConfig, scheme: SecretScheme) -> bool {
    [
        cfg.hooks.auth_token_ref.as_deref(),
        cfg.llm.as_ref().and_then(|llm| llm.api_key_ref.as_deref()),
    ]
    .into_iter()
    .flatten()
    .any(|reference| SecretScheme::of(reference) == scheme)
}

/// Whether `config.toml` holds a reference of `scheme` in an **actual string
/// value** (resolution stays lazy, this only decides whether to probe that
/// backend at all). The file is TOML-parsed and its string leaves walked, so a
/// commented-out example — like the one `totsuka init` generates — never
/// triggers the backend's checks.
///
/// One file since #554: plugin settings live in the same document, so the
/// separate `plugins/*.toml` sweep this used to do is now the same walk.
fn config_mentions_scheme(cx: &Cx, scheme: SecretScheme) -> bool {
    std::fs::read_to_string(&cx.config_path).is_ok_and(|content| {
        // `toml::Table`, not `toml::Value`: in toml 0.9 `FromStr for Value`
        // parses a *single value*, so `"a = 1".parse::<Value>()` is an
        // error ("unexpected content, expected nothing") for every real
        // config file. This helper silently answered "no op:// anywhere"
        // for its whole life, which meant the 1Password checks below never
        // ran at all (#289). `Table` is the document parser.
        content
            .parse::<toml::Table>()
            .is_ok_and(|table| table.values().any(|v| toml_mentions_scheme(v, scheme)))
    })
}

/// Whether any string leaf of a TOML value is a reference of `scheme`.
fn toml_mentions_scheme(value: &toml::Value, scheme: SecretScheme) -> bool {
    match value {
        toml::Value::String(s) => SecretScheme::of(s) == scheme,
        toml::Value::Array(items) => items.iter().any(|v| toml_mentions_scheme(v, scheme)),
        toml::Value::Table(table) => table.values().any(|v| toml_mentions_scheme(v, scheme)),
        _ => false,
    }
}

/// All Claude Code hook-mechanism probes (#141): assets, script dependencies,
/// the Bearer token, the spool backlog, and (when a receiver is live) UDS
/// connectivity. Extends the single asset check that shipped with #137.
fn check_hooks(
    cx: &Cx,
    cfg: &RootConfig,
    config_ok: bool,
    env: &HashMap<String, String>,
    secrets: SecretReadiness,
    args: DoctorArgs,
    checks: &mut Vec<Check>,
) {
    check_hook_assets(cx, cfg, args, checks);
    check_codex_hooks(cx, cfg, config_ok, env, args, checks);
    check_opencode_assets(cfg, config_ok, env, args, checks);
    check_hook_deps(env, checks);
    check_agent_tools(cfg, checks);
    // Which workflows actually need the Bearer token, decided from the static
    // manifests alone (plugin enablement / reference integrity belong to
    // `config validate` and the `plugin:*` checks, not here). An unparsable
    // manifest (`Err`) leaves the capability *unknown*, which must not read as
    // "not hook-capable" — those workflows are surfaced separately so the
    // check cannot be silenced by breaking a manifest (#214).
    let store = cx.store();
    let mut hook_workflows: Vec<(&str, &str)> = Vec::new();
    let mut unknown_workflows: Vec<(&str, &str)> = Vec::new();
    for wf in &cfg.workflows {
        match store.manifest_of(&wf.agent) {
            Ok(Some(m)) if m.capabilities.hook_completion => {
                hook_workflows.push((wf.name.as_str(), wf.agent.as_str()));
            }
            // Not installed (`plugin:*` reports that) or not hook-capable.
            Ok(_) => {}
            Err(_) => unknown_workflows.push((wf.name.as_str(), wf.agent.as_str())),
        }
    }
    check_hook_token(
        cfg,
        env,
        &hook_workflows,
        &unknown_workflows,
        secrets,
        checks,
    );
    check_spool(cx, cfg, env, args, checks);
    check_hook_socket(cx, cfg, env, secrets, checks);
}

/// Refresh the static hook scripts + per-workflow settings (idempotent, same
/// writeout as `totsuka run`, so `doctor` doubles as "materialize the hooks"),
/// then verify every asset exists with the embedded content and the expected
/// mode (0700 scripts / 0600 settings, N-02 tamper resistance).
fn check_hook_assets(cx: &Cx, cfg: &RootConfig, args: DoctorArgs, checks: &mut Vec<Check>) {
    // `verify_assets` below runs either way; only the refresh is suppressed, so
    // `--no-repair` still reports drift — it just does not silently repair it
    // first, which is what makes the report describe the machine as found.
    if !args.no_repair
        && let Err(e) = orchestrator_core::hooks::install(&cx.paths, cfg)
    {
        checks.push(Check::fail(
            "hooks",
            format!("could not write hook scripts/settings: {e}"),
            "check permissions on $XDG_DATA_HOME/totsuka/hooks",
        ));
        return;
    }
    let issues = orchestrator_core::hooks::verify_assets(&cx.paths, cfg);
    if issues.is_empty() {
        let dir = orchestrator_core::hooks::hooks_dir(&cx.paths);
        // Surface non-stock prompts here (#315): an operator debugging a task
        // that never completes needs to know the rendered settings came from an
        // override before they compare the text against the docs.
        //
        // Since #465 there is exactly one thing to count. The `[prompts]`
        // tables this used to walk are gone, and a config that still carries
        // one never reaches `doctor`'s later checks — validation refuses it by
        // name, which is a louder signal than a note on a passing check.
        let overrides = cfg
            .workflows
            .iter()
            .filter(|wf| wf.rubric.is_some())
            .count();
        let prompt_note = if overrides == 0 {
            String::new()
        } else {
            format!(" ({overrides} prompt override(s) active)")
        };
        checks.push(Check::ok(
            "hooks",
            format!(
                "{} scripts (0700) + {} workflow settings (0600) under {}{prompt_note}",
                orchestrator_core::hooks::script_count(),
                cfg.workflows.len(),
                dir.display()
            ),
        ));
    } else {
        let detail = issues
            .iter()
            .map(|i| format!("{}: {}", i.path.display(), i.problem))
            .collect::<Vec<_>>()
            .join("; ");
        // Tampering is only a fair reading when a repair *was* attempted and
        // did not stick. Under `--no-repair` a mismatch usually means the
        // assets were simply never installed.
        let (detail, action) = if args.no_repair {
            (
                format!("hook assets do not match the expected content: {detail}"),
                "run `totsuka doctor` without --no-repair (or `totsuka run`) to write them",
            )
        } else {
            (
                format!("hook assets are inconsistent after a repair attempt: {detail}"),
                "a persistent mismatch on a writable dir means the asset is being tampered with (N-02) → investigate",
            )
        };
        checks.push(Check::fail("hooks", detail, action));
    }
}

/// Codex hook registration (#196 Phase 2), only when the config references a
/// codex-kind tool (silent otherwise — a claude-only setup has nothing to
/// check). Mirrors `check_hook_assets`: sync (self-heal) then verify, plus the
/// codex-specific trust probe — codex **silently skips** untrusted hook
/// entries, which would strand every codex task in a timeout escalation, so an
/// untrusted entry is surfaced with the one-time TUI approval as the action.
fn check_codex_hooks(
    cx: &Cx,
    cfg: &RootConfig,
    config_ok: bool,
    env: &HashMap<String, String>,
    args: DoctorArgs,
    checks: &mut Vec<Check>,
) {
    use orchestrator_core::hooks::codex;
    if !codex::references_codex(cfg) {
        return;
    }
    // A config that validation rejects (e.g. a tool kind without an adapter)
    // must not trigger writes into the user's $CODEX_HOME — `run` would abort
    // on the same config before ever syncing. The failing `config` check
    // above already carries the fix; this row just explains the skip.
    if !config_ok {
        checks.push(Check::warn(
            "codex-hooks",
            "skipped: the config has validation errors, so the hooks.json sync did not run",
            "fix the config errors, then re-run doctor",
        ));
        return;
    }
    let home = codex::codex_home(|k| env.get(k).cloned());
    // The sync is the one write `doctor` makes **outside totsuka's own dirs**,
    // so it is the write `--no-repair` exists for. The verify + trust probes
    // below are reads and still run; without the sync they report the real
    // state of `hooks.json` instead of the state doctor just imposed on it.
    if args.no_repair {
        // `SyncOutcome::NoCodexHome` means "no *existing* codex home", but
        // `codex_home()` happily returns `$HOME/.codex` whether or not it is
        // there. Testing only `is_none()` would let an uninstalled codex fall
        // through to `verify_registration`, which reports every entry missing
        // and tells the operator to re-run without `--no-repair` — advice that
        // cannot work, because the repairing path returns `NoCodexHome` and
        // never creates the file. Audit mode is exactly where the tool is
        // likeliest to be absent, so the two conditions have to agree.
        if home.as_deref().is_none_or(|h| !h.is_dir()) {
            checks.push(Check::fail(
                "codex-hooks",
                "the config references a codex-kind tool but no codex home was found",
                "install the codex CLI (its home is $CODEX_HOME, default ~/.codex) or drop the codex tool reference",
            ));
            return;
        }
    } else {
        match codex::sync_registration(home.as_deref(), &cx.paths, cfg) {
            Ok(codex::SyncOutcome::NoCodexHome) => {
                checks.push(Check::fail(
                    "codex-hooks",
                    "the config references a codex-kind tool but no codex home was found",
                    "install the codex CLI (its home is $CODEX_HOME, default ~/.codex) or drop the codex tool reference",
                ));
                return;
            }
            Ok(_) => {}
            Err(e) => {
                checks.push(Check::fail(
                    "codex-hooks",
                    format!("could not sync the totsuka entries in hooks.json: {e}"),
                    "fix $CODEX_HOME/hooks.json (it is never overwritten when unparseable) and re-run doctor",
                ));
                return;
            }
        }
    }
    let home = home.expect("the no-home case returned above");
    let issues = codex::verify_registration(&home, &cx.paths);
    if !issues.is_empty() {
        let detail = issues
            .iter()
            .map(|i| format!("{}: {}", i.path.display(), i.problem))
            .collect::<Vec<_>>()
            .join("; ");
        let (detail, action) = if args.no_repair {
            (
                format!("hooks.json does not match the expected entries: {detail}"),
                "run `totsuka doctor` without --no-repair (or `totsuka run`) to sync it",
            )
        } else {
            (
                format!("hooks.json is inconsistent after a sync attempt: {detail}"),
                "a persistent mismatch on a writable file means it is being tampered with (N-02) → investigate",
            )
        };
        checks.push(Check::fail("codex-hooks", detail, action));
        return;
    }
    match codex::untrusted_events(&home, &cx.paths) {
        Ok(untrusted) if untrusted.is_empty() => checks.push(Check::ok(
            "codex-hooks",
            format!(
                "totsuka entries registered and trusted in {}",
                codex::hooks_json_path(&home).display()
            ),
        )),
        Ok(untrusted) => checks.push(Check::warn(
            "codex-hooks",
            format!(
                "codex will silently skip the untrusted totsuka entries: {}",
                untrusted.join(", ")
            ),
            "run `codex` once and choose \"Trust all and continue\" in the startup hooks review (re-needed only when the entries themselves change)",
        )),
        Err(e) => checks.push(Check::warn(
            "codex-hooks",
            format!("could not read the codex trust state: {e}"),
            "check $CODEX_HOME/config.toml is readable",
        )),
    }
}

/// OpenCode asset installation (#196 Phase 3), only when the config references
/// an opencode-kind tool. Mirrors `check_codex_hooks` (sync then verify, and
/// nothing runs on an invalid config), minus the trust probe — opencode has no
/// trust step, so a synced asset set is already fully active.
fn check_opencode_assets(
    cfg: &RootConfig,
    config_ok: bool,
    env: &HashMap<String, String>,
    args: DoctorArgs,
    checks: &mut Vec<Check>,
) {
    use orchestrator_core::hooks::opencode;
    if !opencode::references_opencode(cfg) {
        return;
    }
    if !config_ok {
        checks.push(Check::warn(
            "opencode-assets",
            "skipped: the config has validation errors, so the asset sync did not run",
            "fix the config errors, then re-run doctor",
        ));
        return;
    }
    let dir = opencode::opencode_config_dir(|k| env.get(k).cloned());
    // Same shape as the codex sync, and suppressed for the same reason: it
    // writes into a directory totsuka does not own.
    if args.no_repair {
        // Same trap as the codex guard: `SyncOutcome::NoConfigDir` tests for an
        // *existing* directory, so `is_none()` alone would accuse a machine
        // without opencode of tampering with assets it never had.
        if dir.as_deref().is_none_or(|d| !d.is_dir()) {
            checks.push(Check::fail(
                "opencode-assets",
                "the config references an opencode-kind tool but no opencode config dir was found",
                "install opencode and run it once (its config dir — $XDG_CONFIG_HOME/opencode, default ~/.config/opencode — must exist) or drop the opencode tool reference",
            ));
            return;
        }
    } else {
        match opencode::sync_assets(dir.as_deref(), cfg) {
            Ok(opencode::SyncOutcome::NoConfigDir) => {
                checks.push(Check::fail(
                    "opencode-assets",
                    "the config references an opencode-kind tool but no opencode config dir was found",
                    "install opencode and run it once (its config dir — $XDG_CONFIG_HOME/opencode, default ~/.config/opencode — must exist) or drop the opencode tool reference",
                ));
                return;
            }
            Ok(_) => {}
            Err(e) => {
                checks.push(Check::fail(
                    "opencode-assets",
                    format!("could not write the opencode assets: {e}"),
                    "check permissions on the opencode config dir ($XDG_CONFIG_HOME/opencode, default ~/.config/opencode)",
                ));
                return;
            }
        }
    }
    let dir = dir.expect("the no-dir case returned above");
    let issues = opencode::verify_assets(&dir);
    if issues.is_empty() {
        checks.push(Check::ok(
            "opencode-assets",
            format!(
                "totsuka plugin + plan agent installed under {}",
                dir.display()
            ),
        ));
    } else {
        let detail = issues
            .iter()
            .map(|i| format!("{}: {}", i.path.display(), i.problem))
            .collect::<Vec<_>>()
            .join("; ");
        let (detail, action) = if args.no_repair {
            (
                format!("assets do not match the expected content: {detail}"),
                "run `totsuka doctor` without --no-repair (or `totsuka run`) to install them",
            )
        } else {
            (
                format!("assets are inconsistent after a sync attempt: {detail}"),
                "a persistent mismatch on a writable dir means the asset is being tampered with (N-02) → investigate",
            )
        };
        checks.push(Check::fail("opencode-assets", detail, action));
    }
}

/// The Stop hook shells out to `curl` (POST) and `jq` (marker parse); both must
/// be on PATH (H-14). Neither is a build dependency, so a missing tool only
/// surfaces at hook time — `doctor` catches it up front.
fn check_hook_deps(env: &HashMap<String, String>, checks: &mut Vec<Check>) {
    let missing: Vec<&str> = ["curl", "jq"]
        .into_iter()
        .filter(|bin| which(bin, env).is_none())
        .collect();
    if missing.is_empty() {
        checks.push(Check::ok("hook-deps", "curl and jq are on PATH"));
    } else {
        checks.push(Check::fail(
            "hook-deps",
            format!("hook scripts need but cannot find: {}", missing.join(", ")),
            "install the missing tool(s): the Stop hook uses curl to POST and jq to parse the status marker",
        ));
    }
}

/// The Bearer token that authenticates hook POSTs (E-03) must resolve. Unlike
/// every other check, the severity of an *unset* `auth_token_ref` depends on
/// the config: it is a hard failure once some workflow uses a hook-capable
/// agent (that config would accept unauthenticated POSTs in production), and
/// merely advisory otherwise, since such a config never needs the token and
/// the 0600 socket is still a barrier.
///
/// `hook_workflows` is the `(workflow, agent)` list of workflows whose agent
/// declares `Capabilities::hook_completion`; `unknown_workflows` holds those
/// whose agent's capability could not be determined (unparsable manifest), so
/// the advisory can say *why* it might be under-reporting instead of silently
/// treating them as not hook-capable (#214).
fn check_hook_token(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    hook_workflows: &[(&str, &str)],
    unknown_workflows: &[(&str, &str)],
    secrets: SecretReadiness,
    checks: &mut Vec<Check>,
) {
    match &cfg.hooks.auth_token_ref {
        None if !hook_workflows.is_empty() => {
            let users = hook_workflows
                .iter()
                .map(|(wf, agent)| format!("`{wf}` uses hook-capable agent `{agent}`"))
                .collect::<Vec<_>>()
                .join("; ");
            checks.push(Check::fail(
                "hook-token",
                format!(
                    "[hooks].auth_token_ref is unset but {users} → hook POSTs would be accepted without a Bearer token (E-03)"
                ),
                "set [hooks].auth_token_ref (e.g. keychain:totsuka/hook-token)",
            ))
        }
        None => {
            let mut detail = "[hooks].auth_token_ref is unset → hook POSTs are accepted on the \
                 0600 socket without a Bearer token"
                .to_string();
            if !unknown_workflows.is_empty() {
                let unknown = unknown_workflows
                    .iter()
                    .map(|(wf, agent)| format!("`{wf}` uses `{agent}`"))
                    .collect::<Vec<_>>()
                    .join("; ");
                detail.push_str(&format!(
                    "; hook capability is unknown for {unknown} (invalid plugin.toml, see the \
                     `plugin:*` checks), so this could actually be a failure (E-03)"
                ));
            }
            checks.push(Check::warn(
                "hook-token",
                detail,
                "set [hooks].auth_token_ref (e.g. keychain:totsuka/hook-token) before using a hook-capable agent",
            ))
        }
        // A gated scheme is deliberately not resolved here: a real `op read`
        // can prompt for biometrics / hang unattended, and a `cmd:` reference
        // would execute a command (ADR-0006, #444). The 1password probes check
        // presence + session without prompting.
        //
        // This reads like a reporting site but it is a gate: anything
        // `deferred_note` declines falls through to the real `resolve` below.
        Some(reference) => match secrets.deferred_note(reference, "[hooks].auth_token_ref") {
            Some(note) => checks.push(Check::ok("hook-token", note)),
            None => match secret_resolver(env).resolve(reference) {
                Ok(_) => checks.push(Check::ok("hook-token", "[hooks].auth_token_ref resolves")),
                Err(e) => checks.push(Check::fail(
                    "hook-token",
                    format!("[hooks].auth_token_ref does not resolve: {e}"),
                    "export the referenced env var, store the token in the Keychain, or use an op:// reference",
                )),
            },
        },
    }
}

/// The spool directory (E-07 at-least-once fallback) must be writable, and a
/// non-empty backlog is surfaced as an advisory — spooled events replay
/// automatically on the next `totsuka run`, but a growing backlog signals the
/// receiver has been unreachable.
fn check_spool(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    args: DoctorArgs,
    checks: &mut Vec<Check>,
) {
    let env_fn = |k: &str| env.get(k).cloned();
    let dir = match &cfg.hooks.spool_dir {
        Some(p) => match config::expand_path(p, &env_fn) {
            Ok(dir) => dir,
            Err(e) => {
                checks.push(Check::fail(
                    "hook-spool",
                    format!("[hooks].spool_dir does not expand: {e}"),
                    "fix the ${{ENV}} reference in [hooks].spool_dir",
                ));
                return;
            }
        },
        None => cx.paths.state_dir().join("hooks").join("spool"),
    };
    // Both the create and the probe below write. Under `--no-repair` the
    // directory is reported as found and the backlog is still counted (reading
    // it is free) — the price is that writability goes unverified, which is
    // the honest trade for touching nothing.
    if args.no_repair {
        if !dir.is_dir() {
            checks.push(Check::warn(
                "hook-spool",
                format!(
                    "spool dir {} does not exist yet (--no-repair: not created)",
                    dir.display()
                ),
                "run `totsuka doctor` without --no-repair, or `totsuka run`, to create it",
            ));
            return;
        }
        let backlog = count_spool_backlog(&dir);
        checks.push(if backlog == 0 {
            Check::ok(
                "hook-spool",
                format!("{} exists, no backlog (writability unchecked)", dir.display()),
            )
        } else {
            Check::warn(
                "hook-spool",
                format!(
                    "{backlog} spooled hook-event file(s) awaiting replay in {}",
                    dir.display()
                ),
                "run `totsuka run` to drain the spool (idempotent); inspect any *.corrupt files by hand",
            )
        });
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        checks.push(Check::fail(
            "hook-spool",
            format!("spool dir {} is not creatable: {e}", dir.display()),
            "check permissions on $XDG_STATE_HOME/totsuka/hooks",
        ));
        return;
    }
    // Writability probe: create and immediately remove a marker file.
    let probe = dir.join(".doctor-write-probe");
    if let Err(e) = std::fs::write(&probe, b"") {
        checks.push(Check::fail(
            "hook-spool",
            format!("spool dir {} is not writable: {e}", dir.display()),
            "check permissions on the spool directory",
        ));
        return;
    }
    let _ = std::fs::remove_file(&probe);

    let backlog = count_spool_backlog(&dir);
    if backlog == 0 {
        checks.push(Check::ok(
            "hook-spool",
            format!("{} is writable, no backlog", dir.display()),
        ));
    } else {
        checks.push(Check::warn(
            "hook-spool",
            format!(
                "{backlog} spooled hook-event file(s) awaiting replay in {}",
                dir.display()
            ),
            "run `totsuka run` to drain the spool (idempotent); inspect any *.corrupt files by hand",
        ));
    }
}

/// Operator-supplied worktree location templates must expand (F-22).
///
/// An unset `${VAR}` is a hard error in `expand_env`, and worktree creation
/// happens per dispatch — so a bad template does not surface at startup, it
/// fails every task at `fail_dispatch`. Catching it here mirrors `check_spool`.
///
/// Only explicit values are checked: the built-in default is pre-resolved from
/// [`Paths`](orchestrator_core::paths::Paths) and always expands. The rendered
/// value is discarded — `{repo_name}` / `{worktree_name}` are still unresolved at this
/// point, so there is no directory to probe for writability.
///
/// Several templates can be broken at once (the global one plus any per-repo
/// override), but they are reported as **one** `worktree-location` entry: the
/// rest of `doctor` keeps one check per name (a loop over many items varies the
/// name instead, as `plugin:{name}` does), and `--json` consumers look checks up
/// by name. Every offender is still named in the detail, so one `doctor` run is
/// enough to fix them all.
fn check_worktree_location(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) {
    let env_fn = |k: &str| env.get(k).cloned();
    let templates = std::iter::once((None, cfg.worktree.location.as_deref())).chain(
        cfg.repositories
            .iter()
            .map(|r| (Some(r.name.as_str()), r.worktree_location.as_deref())),
    );

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for (repo, template) in templates {
        let Some(template) = template else { continue };
        checked += 1;
        if let Err(e) = config::expand_path(template, &env_fn) {
            let referrer = match repo {
                Some(name) => format!("[[repositories]] `{name}`.worktree_location"),
                None => "[worktree].location".to_string(),
            };
            failures.push(format!("{referrer} does not expand: {e}"));
        }
    }

    if failures.is_empty() {
        checks.push(Check::ok(
            "worktree-location",
            match checked {
                0 => "using the built-in default location".to_string(),
                n => format!("{n} configured location template(s) expand"),
            },
        ));
    } else {
        checks.push(Check::fail(
            "worktree-location",
            failures.join("; "),
            "export the missing variable, or drop the key to fall back to the built-in default \
             ($XDG_STATE_HOME/totsuka/worktrees/..., or $HOME/.local/state/totsuka/worktrees/... \
             when XDG_STATE_HOME is unset)",
        ));
    }
}

/// Number of pending spool files (`*.jsonl`); quarantined `*.jsonl.corrupt`
/// files are excluded (they never replay automatically).
fn count_spool_backlog(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
                .count()
        })
        .unwrap_or(0)
}

/// Probe UDS connectivity by connecting to the receiver socket and POSTing a
/// synthetic event (E-04: it answers 200 immediately). `doctor` usually runs
/// while the orchestrator is *not* running, so an absent socket is expected and
/// reported ok — the probe only asserts health when a receiver is actually live.
///
/// "Actually live" is settled by connecting, not by the socket file existing:
/// the file outlives the listener, and the token gates below return before the
/// authenticated probe would have found that out.
fn check_hook_socket(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    secrets: SecretReadiness,
    checks: &mut Vec<Check>,
) {
    let socket_path = match crate::common::hook_socket_path(cx, cfg, env) {
        Ok(path) => path,
        Err(e) => {
            checks.push(Check::fail(
                "hook-socket",
                e.to_string(),
                "fix the ${{ENV}} reference in [hooks].socket_path",
            ));
            return;
        }
    };
    if !is_socket(&socket_path) {
        checks.push(Check::ok(
            "hook-socket",
            format!(
                "no live receiver at {} (expected unless `totsuka run` is active)",
                socket_path.display()
            ),
        ));
        return;
    }
    // `is_socket` reads the file *type* and nothing else, so a socket left
    // behind by a previous `totsuka run` passes it. Connect before any branch
    // below says a receiver is live, because the token gates return early and
    // would otherwise assert a liveness that nothing measured — a stale socket
    // plus an `op://` token reported "a receiver is live" while `lsof` showed
    // no holder at all. Connecting needs no token, so it is the one liveness
    // proof available on every path.
    if let Err(e) = can_connect(&socket_path) {
        checks.push(Check::warn(
            "hook-socket",
            format!(
                "socket {} exists but is not accepting connections: {e}",
                socket_path.display()
            ),
            STALE_SOCKET_ACTION,
        ));
        return;
    }
    // A receiver is live: prove connectivity + auth with a self-POST.
    //
    // This resolves `auth_token_ref` for real — the second `op://` door in
    // doctor, and one the `hook-token` check's "not resolved here" message
    // does not cover (#289). Probing without the token would be worse than
    // not probing: the receiver would answer 401 and the check would report a
    // token mismatch that does not exist.
    let token_ref = cfg.hooks.auth_token_ref.as_deref();
    if let Some(skip) = token_ref.and_then(|reference| secrets.skip_for(reference)) {
        checks.push(Check::skip(
            "hook-socket",
            format!(
                "a receiver is live at {} but {}",
                socket_path.display(),
                skip.detail
            ),
            skip.action("the receiver"),
        ));
        return;
    }
    // Resolution failures used to be swallowed by `.ok()`, which then probed
    // with no token at all: the receiver answered 401 and the check reported a
    // *token mismatch* that did not exist. A reference that cannot resolve is
    // its own finding, and saying so is strictly more informative than a 401
    // that says nothing about the receiver (Copilot review, #699).
    let token = match token_ref {
        None => None,
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(token) => Some(token),
            Err(e) => {
                checks.push(Check::fail(
                    "hook-socket",
                    format!(
                        "a receiver is live at {} but [hooks].auth_token_ref does not resolve: {e}",
                        socket_path.display()
                    ),
                    "fix the reference — probing without the token would report a 401 that \
                     says nothing about the receiver",
                ));
                return;
            }
        },
    };
    match self_post(&socket_path, token.as_ref().map(|t| t.expose())) {
        Ok(200) => checks.push(Check::ok(
            "hook-socket",
            format!("receiver at {} answered 200", socket_path.display()),
        )),
        Ok(401) => checks.push(Check::fail(
            "hook-socket",
            format!(
                "receiver at {} rejected the probe (401)",
                socket_path.display()
            ),
            "the running receiver's Bearer token differs from [hooks].auth_token_ref → restart `totsuka run` after aligning the token",
        )),
        Ok(status) => checks.push(Check::fail(
            "hook-socket",
            format!(
                "receiver at {} answered {status}",
                socket_path.display()
            ),
            "check the `totsuka run` logs for the hook receiver",
        )),
        // The connect above already passed, so reaching here means the
        // listener went away between the two calls, or answered nothing
        // parseable. Same advice either way, and still advisory: `doctor` must
        // pass when the orchestrator is *not* running.
        Err(e) => checks.push(Check::warn(
            "hook-socket",
            format!(
                "socket {} exists but is not accepting connections: {e}",
                socket_path.display()
            ),
            STALE_SOCKET_ACTION,
        )),
    }
}

/// Locate `bin` on `PATH` (executable regular file). No subprocess is spawned —
/// this mirrors `command -v` without side effects.
fn which(bin: &str, env: &HashMap<String, String>) -> Option<PathBuf> {
    let path = env.get("PATH")?;
    std::env::split_paths(path)
        .map(|dir| dir.join(bin))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// What to tell the operator when the socket file is there but nothing answers.
/// Shared by the pre-probe and the post-probe arm so the two cannot drift.
const STALE_SOCKET_ACTION: &str = "the receiver is not running, or this is a stale socket — ignore if `totsuka run` is not active, else remove the stale socket file and restart";

/// Whether a listener actually accepts a connection on `path`. `is_socket`
/// only proves the inode is a socket; this is what separates a live receiver
/// from a file left behind by a previous run. Deliberately connect-only — it
/// needs no token, so it can run before the `op://` / `cmd:` gates that skip
/// the authenticated probe.
#[cfg(unix)]
fn can_connect(path: &Path) -> io::Result<()> {
    std::os::unix::net::UnixStream::connect(path).map(|_| ())
}

#[cfg(not(unix))]
fn can_connect(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "UDS hook socket probe is only supported on Unix",
    ))
}

/// Whether `path` is an existing Unix domain socket (a live receiver's socket).
#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_socket(_path: &Path) -> bool {
    false
}

/// POST a synthetic doctor event to the receiver socket and return the HTTP
/// status. The `job-0-0` job id names no real task, so the receiver parks it
/// harmlessly (E-09) after answering — the probe never mutates task state.
#[cfg(unix)]
fn self_post(socket_path: &Path, token: Option<&str>) -> io::Result<u16> {
    use std::io::{Read, Write};
    use std::time::Duration;

    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;

    let body = format!(
        r#"{{"job_id":"{probe}","doctor_probe":true}}"#,
        probe = orchestrator_core::domain::signal::JobId::DOCTOR_PROBE
    );
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /agent-events HTTP/1.1\r\n\
         Host: localhost\r\n\
         {auth}\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_status(&response)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no HTTP status line in reply"))
}

#[cfg(not(unix))]
fn self_post(_socket_path: &Path, _token: Option<&str>) -> io::Result<u16> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "UDS hook socket probe is only supported on Unix",
    ))
}

/// Extract the numeric status from an HTTP/1.1 status line (`HTTP/1.1 200 OK`).
#[cfg(unix)]
fn parse_status(response: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(response).ok()?;
    let first = text.lines().next()?;
    first.split_whitespace().nth(1)?.parse().ok()
}

/// Installed + protocol-compatible + live-probe for every enabled plugin.
fn check_plugins(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    secrets: SecretReadiness,
    checks: &mut Vec<Check>,
) {
    let enabled: Vec<&String> = cfg
        .plugins
        .iter()
        .filter(|(_, p)| p.enabled)
        .map(|(name, _)| name)
        .collect();
    if enabled.is_empty() {
        checks.push(Check::ok("plugins", "no plugins enabled"));
        return;
    }
    let mut specs = Vec::new();
    // Plugins that never reach `validate_all`, and why. `validated.len()` only
    // counts what got into `specs`, so the projects check below cannot tell
    // "nothing was skipped" from "everything was skipped" without this — and
    // being skipped is the *normal* state for a `cmd:` token, not an edge case
    // (#542 review).
    let mut not_probed: Vec<(String, &'static str)> = Vec::new();
    for name in enabled {
        // `plugin_spec` resolves secrets, so a plugin whose references would
        // prompt (`op://` with no session) or run something (`cmd:`) cannot be
        // probed (#289, #444). Decided per plugin: one plugin's gated
        // reference must not silence the probes of plugins that need no secret
        // at all.
        if let Some(skip) = plugin_secret_skip(cx, cfg, name, secrets) {
            checks.push(Check::skip(
                &format!("plugin:{name}"),
                skip.detail,
                skip.action("this plugin"),
            ));
            not_probed.push((name.clone(), skip.summary));
            continue;
        }
        match plugin_spec(&cx.store(), cfg, name, env) {
            // `plugin_spec` already resolved the plugin's `[<name>]` table
            // into `init_config`; reuse it rather than re-reading and hitting
            // the Keychain a second time.
            Ok(spec) => {
                let init = spec.init_config.clone();
                specs.push((spec, init));
            }
            // Failure may be "not installed" or a `[<name>]` table
            // parse/secret-resolution error — point at both.
            Err(e) => {
                checks.push(Check::fail(
                    &format!("plugin:{name}"),
                    e.to_string(),
                    format!("install it (`totsuka plugin install <dir>`) or fix `[{name}]` in config.toml if it is already installed"),
                ));
                not_probed.push((name.clone(), "its launch spec could not be built"));
            }
        }
    }
    if specs.is_empty() {
        // Still report the projects check: with nothing probed it can only say
        // "cannot tell", and saying nothing at all reads as "no conflicts".
        check_project_claims(&[], &not_probed, checks);
        return;
    }
    // Live probe: launch, initialize, config/validate, shutdown (F-59).
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        checks.push(Check::fail(
            "plugins",
            "could not start an async runtime for plugin probes",
            "re-run; report if it persists",
        ));
        return;
    };
    let validated = runtime.block_on(plugin_host::validate_all(specs));
    for plugin_host::ValidatedPlugin { name, result, .. } in &validated {
        match result {
            // A plugin may be correctly configured and still know something
            // worth saying (protocol 0.7.3, #662) — "this queue has never
            // delivered anything" is true of a setup that is otherwise
            // perfect. Warnings never fail `doctor`, and a plugin that sends
            // none is reported exactly as before.
            Ok(v) if v.valid && v.warnings.is_empty() => {
                checks.push(Check::ok(
                    &format!("plugin:{name}"),
                    "launches and accepts its config",
                ));
            }
            Ok(v) if v.valid => push_warnings(name, &v.warnings, checks),
            Ok(v) => {
                checks.push(Check::fail(
                    &format!("plugin:{name}"),
                    v.errors.join("; "),
                    format!("fix `[{name}]` in config.toml"),
                ));
                // Still worth showing: a config can be refused for one reason
                // while a second, unrelated thing is also wrong.
                push_warnings(name, &v.warnings, checks);
            }
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "check the binary and protocol compatibility",
            )),
        }
    }
    check_project_claims(&validated, &not_probed, checks);
}

/// **One** advisory check carrying all of a plugin's warnings (protocol 0.7.3,
/// #662).
///
/// One per warning would repeat the `plugin:{name}` key, and `--json`
/// consumers look checks up by name — a second entry under a name already
/// present is a row nobody reads. So the warnings are joined, exactly as
/// `errors` already are for the failure case.
///
/// Warnings share the `errors` convention of "cause → next action", so the
/// arrow is where the two halves of a [`Check`] come from. A warning written
/// without one still reports — it becomes the cause, and the action says to
/// read it — because dropping the line entirely would be the one outcome
/// worse than an imperfectly split one.
fn push_warnings(name: &str, warnings: &[String], checks: &mut Vec<Check>) {
    if warnings.is_empty() {
        return;
    }
    let mut causes = Vec::with_capacity(warnings.len());
    let mut actions = Vec::with_capacity(warnings.len());
    for warning in warnings {
        match warning.split_once(" → ") {
            Some((cause, next)) => {
                causes.push(cause.to_string());
                actions.push(next.to_string());
            }
            None => {
                causes.push(warning.clone());
                actions.push(format!("reported by `{name}`; act on it or ignore it"));
            }
        }
    }
    // The `ok` line is *replaced*, not accompanied — one line per plugin
    // either way. So it has to keep saying the thing the `ok` line said,
    // or "did it even launch?" becomes unanswerable the moment a plugin
    // has anything to report.
    checks.push(Check::warn(
        &format!("plugin:{name}"),
        format!(
            "launches and accepts its config, but: {}",
            causes.join("; ")
        ),
        actions.join("; "),
    ));
}

/// How many repositories have a project to file into (#542, narrowed by #554).
///
/// **This reports, it no longer detects.** Until #554 it was the only place a
/// repository claimed by two sources became visible, because each plugin's
/// `config/validate` sees only its own list and the conflict existed only in
/// the union. That conflict is now unwritable — a repository names one
/// `[[projects]]` entry and the entry names one source — so what is left is a
/// count, and the check only ever produces `ok` or `skip`.
///
/// Reads the claims [`validate_all`](plugin_host::validate_all) already
/// gathered, so it inherits the launch gating exactly rather than restating it.
/// `not_probed` carries what that gating *excluded*, which the claims alone
/// cannot express: a plugin that was never launched reports no claims, and so
/// does a plugin that genuinely claims nothing.
///
/// **An incomplete picture degrades to a skip rather than an all-clear.**
/// "Every repository routes to exactly one project" is a statement about the
/// union, so it cannot be made from the plugins that happened to launch.
fn check_project_claims(
    validated: &[plugin_host::ValidatedPlugin],
    not_probed: &[(String, &'static str)],
    checks: &mut Vec<Check>,
) {
    // A plugin that failed to launch reports no claims, which is not the same
    // as claiming nothing: counting it as "claims nothing" would report an
    // all-clear built from one source's answer.
    let launched: Vec<&plugin_host::ValidatedPlugin> =
        validated.iter().filter(|v| v.result.is_ok()).collect();
    let registry = ClaimRegistry::from_sources(
        launched
            .iter()
            .map(|v| (v.name.as_str(), v.claimed_repos.as_slice())),
    );

    // Two sources claiming one repository used to be reported here (#542).
    // Since #554 a repository names one `[[projects]]` entry and the entry
    // names one source, so the state cannot be written — `config validate`
    // rejects a `project` that resolves to nothing, and there is no second
    // claimant to find.
    //
    // Everything the union is missing, in one list. `not_probed` is the common
    // case in real configs — a `cmd:` token (ADR-0044) means doctor never
    // launches that plugin — so treating it as an edge case would report an
    // all-clear from a single source's claims.
    let mut unseen: Vec<String> = not_probed
        .iter()
        .map(|(name, why)| format!("`{name}` ({why})"))
        .collect();
    unseen.extend(
        validated
            .iter()
            .filter(|v| v.result.is_err())
            .map(|v| format!("`{}` (it did not launch)", v.name)),
    );

    if !unseen.is_empty() {
        checks.push(Check::skip(
            "projects",
            format!("cannot tell: {} was not probed", unseen.join(", ")),
            "doctor stays non-interactive, so a plugin whose secrets need a prompt or a \
             command is never launched — check those configs by hand, or run `totsuka run` \
             which resolves them",
        ));
        return;
    }
    if registry.is_empty() {
        // Every source was probed and none claims anything: the normal state
        // for a config with no project set up. Not worth a line.
        return;
    }
    checks.push(Check::ok(
        "projects",
        format!(
            "{} repositories route to exactly one project",
            registry.len()
        ),
    ));
}

/// The LLM API key reference must resolve (no network call). With `online`,
/// a second check additionally proves the key is *accepted* (#267) — the
/// resolution alone never could, which is how a dead OpenRouter key stayed
/// invisible until the run log happened to be read.
fn check_llm_key(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    args: DoctorArgs,
    secrets: SecretReadiness,
    checks: &mut Vec<Check>,
) {
    let online = args.online;
    let Some(llm) = &cfg.llm else {
        checks.push(Check::ok(
            "llm",
            "no [llm] configured (repo selection falls back to hints/pending)",
        ));
        return;
    };
    match &llm.api_key_ref {
        None => checks.push(Check::ok("llm", "[llm] configured without api_key_ref")),
        // An `op://` reference is NOT resolved here: `op read` may pop a
        // biometric prompt (or hang unattended), and doctor must stay
        // non-interactive (ADR-0006). `--online` is the opt-in that accepts
        // the prompt in exchange for a real answer. Same for `cmd:`, which
        // would execute a command (#444).
        //
        // The wording matters (#289). This used to claim the reference was
        // "checked by the 1password probes", which those probes never did —
        // they check that `op` exists and a session is live, not that this
        // particular item resolves. Now that they also run *first* and gate
        // the checks that do resolve, the honest statement is the narrow one.
        Some(reference) => match secrets.deferred_note(reference, "api_key_ref") {
            Some(note) => checks.push(Check::ok("llm", note)),
            None => match secret_resolver(env).resolve(reference) {
                Ok(_) => checks.push(Check::ok("llm", "api_key_ref resolves")),
                Err(e) => {
                    checks.push(Check::fail(
                        "llm",
                        format!("api_key_ref does not resolve: {e}"),
                        "export the variable, store the key in the Keychain, or use an op:// reference",
                    ));
                    // No key to probe with; the online check would only
                    // restate this failure.
                    return;
                }
            },
        },
    }
    if online {
        check_llm_online(llm, env, checks);
    }
}

/// `--online` only: one live request proving the gateway accepts the key.
///
/// The only doctor check that makes a network call, and the only one that
/// still resolves `op://` unconditionally — so `--online` is also the opt-in
/// to a possible biometric prompt. That is now the *whole* of the exception:
/// #289 closed the paths that used to resolve behind the operator's back
/// (`check_plugins` via `plugin_spec`, `check_hook_socket`, `check_orphan_panes`),
/// which are gated on [`SecretReadiness`] and reported as skipped instead.
///
/// Only a 401/403 fails the check: a timeout or a 5xx says the provider is
/// unreachable or unwell, not that the key is wrong, so those stay advisory
/// rather than turning a flaky network into a red `doctor`.
fn check_llm_online(
    llm: &config::LlmConfig,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) {
    let api_key = match &llm.api_key_ref {
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(key) => key,
            Err(e) => {
                checks.push(Check::fail(
                    "llm-online",
                    format!("api_key_ref does not resolve: {e}"),
                    "check the reference (an op:// read needs an unlocked 1Password session)",
                ));
                return;
            }
        },
        // Matches what `run` sends for a keyless gateway.
        None => SecretString::new(""),
    };

    // `probe_auth` deliberately bypasses the retry loop — a probe answers now
    // or not at all, and retrying a 5xx would only make `doctor` hang on an
    // unwell provider — so `max_retries` is left at its default rather than
    // zeroed here: an assignment the probe never reads would only suggest it
    // is what disables retrying. Only `timeout` is honoured.
    let mut openai = OpenAiConfig::new(&llm.base_url, &llm.model);
    if let Some(secs) = llm.timeout_secs {
        openai.timeout = Duration::from_secs(secs);
    }
    let router = OpenAiRouter::new(openai, api_key);

    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        checks.push(Check::fail(
            "llm-online",
            "could not start a tokio runtime for the probe",
            "re-run doctor; if it persists, report it",
        ));
        return;
    };
    match runtime.block_on(router.probe_auth()) {
        Ok(()) => checks.push(Check::ok(
            "llm-online",
            format!("{} accepted the API key", llm.base_url),
        )),
        Err(e) if e.is_auth_failure() => checks.push(Check::fail(
            "llm-online",
            format!("the provider rejected the API key: {e}"),
            "reissue the key at the provider and update [llm].api_key_ref",
        )),
        Err(e) => checks.push(Check::warn(
            "llm-online",
            format!("could not verify the API key: {e}"),
            "the provider was unreachable or unwell — this does not mean the key is bad; re-run later",
        )),
    }
}

/// Detect orphan worktrees (F-24) and, interactively, offer to remove them.
fn check_orphans(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    db: Option<&orchestrator_core::adapters::StateDb>,
    args: DoctorArgs,
    checks: &mut Vec<Check>,
) -> Result<(), CliError> {
    let json = args.json;
    let Some(db) = db else {
        return Ok(());
    };
    let env_fn = |k: &str| env.get(k).cloned();
    let known: HashSet<PathBuf> = db
        .list_tasks()?
        .into_iter()
        .filter_map(|t| t.worktree_path.map(PathBuf::from))
        .collect();
    let manager = WorktreeManager::new(SystemGitRunner);

    let mut orphans: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    for repo in &cfg.repositories {
        let Ok(path) =
            orchestrator_core::config::expand_path(&repo.path.to_string_lossy(), &env_fn)
        else {
            continue;
        };
        if let Ok(found) = manager.detect_orphans(&path, &known) {
            for orphan in found {
                orphans.push((repo.name.clone(), path.clone(), orphan));
            }
        }
    }

    if orphans.is_empty() {
        checks.push(Check::ok("worktrees", "no orphan worktrees"));
        return Ok(());
    }

    let listing = orphans
        .iter()
        .map(|(repo, _, path)| format!("{repo}: {}", path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    // Interactive cleanup proposal (§5.1) — only on a TTY and never in --json.
    if !json && !args.no_repair && io::stdin().is_terminal() {
        for (repo_name, repo_path, orphan) in &orphans {
            // The path carries the branch built from the task title, and
            // `render_branch` only folds `Cc` — bidi overrides survive it
            // (#297). This is the line the operator answers y/N to, so it is
            // exactly the one that must not be able to lie.
            print!(
                "remove orphan worktree {} (repo {repo_name})? [y/N]: ",
                safe(&orphan.display().to_string())
            );
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                // Go through the GitRunner seam like the rest of the codebase
                // (testable, single place git is invoked).
                let out = SystemGitRunner.run(
                    repo_path,
                    &["worktree", "remove", &orphan.display().to_string()],
                )?;
                if out.success() {
                    println!("removed {}", safe(&orphan.display().to_string()));
                } else {
                    // git quotes the path back at us in its own message.
                    println!(
                        "could not remove (dirty?): {} → remove manually with `git worktree remove --force`",
                        safe(out.stderr.trim())
                    );
                }
            }
        }
        checks.push(Check::ok(
            "worktrees",
            format!("orphans handled: {listing}"),
        ));
    } else {
        checks.push(Check::fail(
            "worktrees",
            format!("orphan worktrees: {listing}"),
            if args.no_repair {
                "re-run `totsuka doctor` without --no-repair to clean them up interactively"
            } else {
                "run `totsuka doctor` in a terminal to clean them up interactively"
            },
        ));
    }
    Ok(())
}

/// One orphan-pane candidate (#211): a live, totsuka-labeled pane no task
/// should still be holding.
struct OrphanPane {
    /// The owning agent plugin (the one to send `session/release` to).
    plugin: String,
    /// The listed pane.
    session: plugin_protocol::methods::SessionInfo,
    /// Why it is a candidate (shown in the prompt / listing).
    reason: String,
}

/// Classify one plugin's `session/list` result against the task DB (#211).
///
/// The label carries the **source task id**: `totsuka {task.id}` where
/// `task.id` is the protocol `Task.id` = `TaskRecord.source_task_id` — the
/// source's own identifier (a Slack `"C1:1.0"`, a GitHub issue number), NOT
/// the DB row id. Correlation is therefore a string match on
/// `source_task_id`, which is only unique per source — so a pane is matched
/// against **every** task carrying that id and the conservative side wins.
///
/// A totsuka-labeled pane is an orphan candidate when:
/// - its label's id matches no task in the DB (a true orphan: crashed
///   dispatch, deleted DB row, pre-#210 leftovers), or
/// - every matching task is **terminal** and none still has a live worktree
///   (the #210 release linkage failed: manual `git worktree remove`, refused
///   release, crash) — `worktree_exists` reports whether a recorded path
///   still exists.
///
/// Deliberately NOT candidates: panes with any non-terminal matching task
/// (the pane is in use) and terminal tasks whose worktree is retained
/// (`keep_7d` etc. — the pane's lifetime tracks the worktree's, ADR-0010).
fn classify_orphan_panes(
    plugin: &str,
    sessions: Vec<plugin_protocol::methods::SessionInfo>,
    tasks: &[orchestrator_core::adapters::TaskRecord],
    worktree_exists: impl Fn(&str) -> bool,
) -> Vec<OrphanPane> {
    sessions
        .into_iter()
        .filter_map(|session| {
            // The plugin only lists panes with its `totsuka ` marker; the
            // source task id after the marker correlates the pane to tasks.
            let matches: Vec<_> = session
                .label
                .as_deref()
                .and_then(|l| l.strip_prefix("totsuka "))
                .map(|id| tasks.iter().filter(|t| t.source_task_id == id).collect())
                .unwrap_or_default();
            let reason = if matches.is_empty() {
                "no matching task in the DB".to_string()
            } else if matches.iter().any(|t| {
                // A live task, or a worktree still held by a retention
                // policy, keeps the pane.
                !t.state.is_terminal() || t.worktree_path.as_deref().is_some_and(&worktree_exists)
            }) {
                return None;
            } else {
                let task = matches[0];
                format!(
                    "task {} is {} and its worktree is gone",
                    task.id, task.state
                )
            };
            Some(OrphanPane {
                plugin: plugin.to_string(),
                session,
                reason,
            })
        })
        .collect()
}

/// Detect orphan agent panes (#211) and, interactively, offer to release
/// them. The counterpart of [`check_orphans`] for panes: enumerate via
/// `session/list` (protocol 0.2.2, `pane_control` agents only), diff against
/// the task DB, and release via `session/release` with the listed label as
/// the `expect_label` identity guard (the enumerate→confirm→release window
/// could see the position-based pane id reassigned).
fn check_orphan_panes(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    db: Option<&orchestrator_core::adapters::StateDb>,
    args: DoctorArgs,
    secrets: SecretReadiness,
    checks: &mut Vec<Check>,
) -> Result<(), CliError> {
    use plugin_protocol::manifest::PluginKind;
    let json = args.json;
    use plugin_protocol::methods::{
        NotReleased, SessionListParams, SessionListResult, SessionReleaseParams,
        SessionReleaseResult,
    };

    let Some(db) = db else {
        return Ok(());
    };
    let store = cx.store();
    // Only agents that can control panes are asked; a config with none (orca,
    // mock) gets no check at all rather than noise.
    let agents: Vec<String> = cfg
        .plugins
        .iter()
        .filter(|(_, p)| p.enabled)
        .filter(|(name, _)| {
            store
                .manifest_of(name)
                .ok()
                .flatten()
                .is_some_and(|m| m.kind == PluginKind::AgentIde && m.capabilities.pane_control)
        })
        .map(|(name, _)| name.clone())
        .collect();
    if agents.is_empty() {
        return Ok(());
    }

    let tasks = db.list_tasks()?;
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        checks.push(Check::fail(
            "panes",
            "could not start an async runtime for the pane probe",
            "re-run; report if it persists",
        ));
        return Ok(());
    };

    let mut orphans: Vec<OrphanPane> = Vec::new();
    let mut probed = 0usize;
    let mut skipped: Vec<(&str, SecretSkip)> = Vec::new();
    for name in &agents {
        // Same gate as `check_plugins` (#289, #444): launching the agent
        // resolves its secrets. Tracked separately so the check can say it
        // saw only part of the picture — silently probing fewer agents would
        // under-report orphans and read as "none found".
        if let Some(skip) = plugin_secret_skip(cx, cfg, name, secrets) {
            skipped.push((name.as_str(), skip));
            continue;
        }
        let spec = match plugin_spec(&store, cfg, name, env) {
            Ok(spec) => spec,
            // plugin_spec failures are already reported per-plugin by
            // check_plugins; don't fail the pane check on top.
            Err(_) => continue,
        };
        let listed = runtime.block_on(async {
            let plugin = plugin_host::Plugin::launch(spec).await?;
            let result: Result<SessionListResult, _> = plugin
                .call(plugin_protocol::method::SESSION_LIST, &SessionListParams {})
                .await;
            let _ = plugin.shutdown(std::time::Duration::from_secs(5)).await;
            result
        });
        match listed {
            Ok(result) => {
                probed += 1;
                orphans.extend(classify_orphan_panes(name, result.sessions, &tasks, |p| {
                    Path::new(p).exists()
                }));
            }
            // The plugin launched but the probe failed (herdr down, old
            // plugin): advisory only — plugin health is check_plugins' job.
            Err(e) => checks.push(Check::warn(
                "panes",
                format!("pane listing via `{name}` failed: {e}"),
                "check that the agent backend (herdr) is running",
            )),
        }
    }

    if !skipped.is_empty() {
        // Say so before any "no orphan panes" line below, so the two are read
        // together: the clean result only covers the agents we could reach.
        // Each agent carries its own reason: a config can mix an `op://`
        // plugin with a `cmd:` one, and a single blanket reason would misreport
        // the other (#444's lesson, applied to the message rather than the
        // gate).
        let reasons = skipped
            .iter()
            .map(|(name, skip)| format!("`{name}` ({})", skip.summary))
            .collect::<Vec<_>>()
            .join(", ");
        let reasons_only: Vec<SecretSkip> = skipped.iter().map(|(_, skip)| *skip).collect();
        checks.push(Check::skip(
            "panes",
            format!("did not list panes via {reasons}"),
            combined_skip_action(&reasons_only, "those agents"),
        ));
    }

    if orphans.is_empty() {
        if probed > 0 {
            checks.push(Check::ok(
                "panes",
                if skipped.is_empty() {
                    "no orphan panes".to_string()
                } else {
                    format!("no orphan panes among the {probed} agent(s) probed")
                },
            ));
        }
        return Ok(());
    }

    let listing = orphans
        .iter()
        .map(|o| {
            format!(
                "{}: {} ({})",
                o.plugin,
                o.session.label.as_deref().unwrap_or(&o.session.session_id),
                o.reason
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    // Interactive release proposal — only on a TTY and never in --json,
    // mirroring the orphan-worktree flow (doctor proposes, never auto-frees).
    if !json && !args.no_repair && io::stdin().is_terminal() {
        for orphan in &orphans {
            // The label is `totsuka {source_task_id}` (ADR-0013) — the id the
            // source chose, so external text on the prompt the operator is
            // about to answer y/N to (#297).
            let name = safe(orphan.session.label.as_deref().unwrap_or("(no label)"));
            print!(
                "release orphan pane {name} via {} — {}? [y/N]: ",
                safe(&orphan.plugin),
                safe(&orphan.reason)
            );
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                continue;
            }
            let Ok(spec) = plugin_spec(&store, cfg, &orphan.plugin, env) else {
                continue;
            };
            let released = runtime.block_on(async {
                let plugin = plugin_host::Plugin::launch(spec).await?;
                let result: Result<SessionReleaseResult, _> = plugin
                    .call(
                        plugin_protocol::method::SESSION_RELEASE,
                        &SessionReleaseParams {
                            session_id: orphan.session.session_id.clone(),
                            expect_cwd: None,
                            // The label we just enumerated is the identity
                            // guard against the pane id being reassigned
                            // between listing and this release.
                            expect_label: orphan.session.label.clone(),
                        },
                    )
                    .await;
                let _ = plugin.shutdown(std::time::Duration::from_secs(5)).await;
                result
            });
            match released {
                Ok(r) if r.released => println!("released {name}"),
                // Since protocol 0.4.2 the plugin says which it was (#485); an
                // older one says nothing, and "already gone or the pane
                // changed identity" is then the honest answer rather than a
                // guess.
                Ok(r) => match r.not_released {
                    // `Gone` also covers "could not tell" (this path sends no
                    // `expect_cwd`, so the plugin often has nothing to go on) —
                    // phrase it as the plugin's report, not as a fact.
                    Some(NotReleased::Gone) => {
                        println!("not released (the plugin found no pane left for this task)")
                    }
                    Some(NotReleased::Refused) => println!(
                        "not released: the plugin reports a live pane still belonging to \
                         this task — check it by hand before closing anything"
                    ),
                    Some(NotReleased::Unknown) | None => {
                        println!("not released (already gone, or the pane changed identity)")
                    }
                },
                Err(e) => println!("release failed: {}", safe(&e.to_string())),
            }
        }
        checks.push(Check::ok("panes", format!("orphans handled: {listing}")));
    } else {
        checks.push(Check::fail(
            "panes",
            format!("orphan panes: {listing}"),
            if args.no_repair {
                "re-run `totsuka doctor` without --no-repair to release them interactively"
            } else {
                "run `totsuka doctor` in a terminal to release them interactively"
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::adapters::TaskRecord;
    use orchestrator_core::domain::state::TaskState;
    use plugin_protocol::methods::SessionInfo;

    // --- `projects` (#542) -------------------------------------------------

    /// Warnings share `errors`' "cause → next action" shape, so the arrow is
    /// where a check's two halves come from (#662).
    #[test]
    fn a_plugin_warning_becomes_an_advisory_check_not_a_failure() {
        let mut checks = Vec::new();
        push_warnings(
            "slack",
            &["the queue has never delivered → check the Request URL".to_string()],
            &mut checks,
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "plugin:slack");
        // Advisory: `doctor` must not exit non-zero over it.
        assert!(checks[0].ok);
        assert!(checks[0].warning);
        // The line keeps saying what the `ok` line said — it replaces it.
        assert_eq!(
            checks[0].detail,
            "launches and accepts its config, but: the queue has never delivered"
        );
        assert_eq!(checks[0].action.as_deref(), Some("check the Request URL"));
    }

    /// **One check, however many warnings.** `--json` consumers look checks
    /// up by name, so a second entry under a name already present is a row
    /// nobody reads.
    #[test]
    fn several_warnings_from_one_plugin_share_a_single_check() {
        let mut checks = Vec::new();
        push_warnings(
            "slack",
            &[
                "the queue is silent → check the Request URL".to_string(),
                "a scope is missing → reinstall the app".to_string(),
            ],
            &mut checks,
        );
        assert_eq!(checks.len(), 1, "one check per plugin");
        assert!(
            checks[0]
                .detail
                .starts_with("launches and accepts its config, but: ")
        );
        assert!(checks[0].detail.contains("the queue is silent"));
        assert!(checks[0].detail.contains("a scope is missing"));
        let action = checks[0].action.as_deref().unwrap();
        assert!(action.contains("check the Request URL"), "{action}");
        assert!(action.contains("reinstall the app"), "{action}");
    }

    /// A warning written without the arrow still reports. Dropping the line
    /// would be the one outcome worse than splitting it imperfectly.
    #[test]
    fn a_warning_without_an_arrow_is_still_reported() {
        let mut checks = Vec::new();
        push_warnings("slack", &["something is odd".to_string()], &mut checks);
        assert!(
            checks[0].detail.ends_with("something is odd"),
            "{}",
            checks[0].detail
        );
        assert!(checks[0].action.is_some());
    }

    fn validated(name: &str, claims: &[(&str, &str)]) -> plugin_host::ValidatedPlugin {
        plugin_host::ValidatedPlugin {
            name: name.to_string(),
            result: Ok(plugin_protocol::methods::ConfigValidateResult {
                valid: true,
                errors: Vec::new(),
                warnings: Vec::new(),
            }),
            claimed_options: Vec::new(),
            claimed_repos: claims
                .iter()
                .map(
                    |(repo, destination)| plugin_protocol::methods::ClaimedRepo {
                        repo: (*repo).to_string(),
                        destination: (*destination).to_string(),
                    },
                )
                .collect(),
        }
    }

    fn project_checks(
        validated: &[plugin_host::ValidatedPlugin],
        not_probed: &[(String, &'static str)],
    ) -> Vec<Check> {
        let mut checks = Vec::new();
        check_project_claims(validated, not_probed, &mut checks);
        checks
    }

    #[test]
    fn projects_pass_when_every_source_was_probed_and_none_conflict() {
        let checks = project_checks(
            &[
                validated("github", &[("totsuka", "Project #7")]),
                validated("notion", &[("web-app", "Database DB2")]),
            ],
            &[],
        );
        assert_eq!(checks.len(), 1);
        assert!(checks[0].ok && !checks[0].skipped, "{:?}", checks[0]);
        assert!(checks[0].detail.contains('2'), "{:?}", checks[0]);
    }

    /// A plugin doctor never launched must not read as "claims nothing".
    ///
    /// This is the **common** case, not an edge one: a `cmd:` token (ADR-0044)
    /// means doctor skips that plugin on every run, so an all-clear here would
    /// be assembled from whatever single source happened to be probeable.
    #[test]
    fn projects_cannot_conclude_while_a_plugin_was_never_probed() {
        let checks = project_checks(
            &[validated("notion", &[("web-app", "Database DB2")])],
            &[(
                "github".to_string(),
                "resolving its cmd: reference would run a command",
            )],
        );
        assert_eq!(checks.len(), 1);
        assert!(checks[0].skipped, "{:?}", checks[0]);
        assert!(checks[0].detail.contains("github"), "{:?}", checks[0]);
        assert!(checks[0].detail.contains("cmd:"), "{:?}", checks[0]);
    }

    /// A plugin that launched and failed validation is "unknown", like one that
    /// was never probed — it answered `initialize` but its config is wrong, so
    /// its claim list is not something to conclude from.
    #[test]
    fn a_plugin_that_failed_to_launch_blocks_the_all_clear() {
        let mut failed = validated("github", &[]);
        failed.result = Err(plugin_host::HostError::Spawn {
            name: "github".to_string(),
            source: std::io::Error::other("boom"),
        });
        let checks = project_checks(&[validated("notion", &[("web-app", "DB2")]), failed], &[]);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].skipped, "{:?}", checks[0]);
        assert!(checks[0].detail.contains("github"), "{:?}", checks[0]);
    }

    /// Nothing probed at all (every plugin skipped) must not read as an
    /// all-clear either — the path `check_plugins` takes when `specs` is empty.
    #[test]
    fn projects_say_nothing_conclusive_when_nothing_was_probed() {
        let checks = project_checks(
            &[],
            &[("github".to_string(), "its op:// reference would prompt")],
        );
        assert_eq!(checks.len(), 1);
        assert!(checks[0].skipped, "{:?}", checks[0]);
    }

    /// Every source probed, none claims anything: a config with no project set
    /// up. Silent — a line saying "0 repositories route" is noise.
    #[test]
    fn projects_are_silent_when_no_source_claims_anything() {
        let checks = project_checks(&[validated("slack", &[])], &[]);
        assert!(checks.is_empty(), "{checks:?}");
    }

    fn worktree_location_checks(toml: &str, env: &[(&str, &str)]) -> Vec<Check> {
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let mut checks = Vec::new();
        check_worktree_location(&cfg, &env, &mut checks);
        checks
    }

    #[test]
    fn worktree_location_default_needs_no_env() {
        // No `[worktree]` at all — the default is pre-resolved from `Paths`,
        // so doctor has nothing to expand and must not fail on an empty env.
        let checks = worktree_location_checks("", &[]);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].ok);
        assert_eq!(checks[0].name, "worktree-location");
    }

    #[test]
    fn worktree_location_flags_an_unset_env_reference() {
        let checks = worktree_location_checks(
            r#"
[worktree]
location = "${TOTSUKA_DOCTOR_UNSET_VAR}/wt/{worktree_name}"
"#,
            &[],
        );
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].ok);
        assert!(
            checks[0].detail.contains("[worktree].location"),
            "message names the offending key: {}",
            checks[0].detail
        );
    }

    #[test]
    fn worktree_location_flags_a_per_repo_override() {
        // The per-repo override (`run/mod.rs` prefers it over the global one)
        // must be checked too, and reported by repository name.
        let checks = worktree_location_checks(
            r#"
[[repositories]]
name = "web"
path = "/repos/web"
worktree_location = "${TOTSUKA_DOCTOR_UNSET_VAR}/wt/{worktree_name}"
"#,
            &[],
        );
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].ok);
        assert!(
            checks[0].detail.contains("`web`"),
            "message names the repository: {}",
            checks[0].detail
        );
    }

    /// Several broken templates collapse into one check entry that names them
    /// all — `doctor --json` consumers look checks up by name, so a duplicated
    /// name would hide every offender but the first.
    #[test]
    fn worktree_location_reports_every_offender_in_one_check() {
        let checks = worktree_location_checks(
            r#"
[worktree]
location = "${TOTSUKA_DOCTOR_UNSET_A}/wt/{worktree_name}"

[[repositories]]
name = "web"
path = "/repos/web"
worktree_location = "${TOTSUKA_DOCTOR_UNSET_B}/wt/{worktree_name}"
"#,
            &[],
        );
        assert_eq!(checks.len(), 1, "one entry per check name");
        assert!(!checks[0].ok);
        assert!(
            checks[0].detail.contains("[worktree].location") && checks[0].detail.contains("`web`"),
            "both offenders named: {}",
            checks[0].detail
        );
    }

    #[test]
    fn worktree_location_accepts_a_resolvable_env_reference() {
        let checks = worktree_location_checks(
            r#"
[worktree]
location = "${MY_ROOT}/wt/{worktree_name}"
"#,
            &[("MY_ROOT", "/tmp/root")],
        );
        assert_eq!(checks.len(), 1);
        assert!(checks[0].ok, "{}", checks[0].detail);
    }

    /// A task whose **source task id** (what the pane label carries — e.g. a
    /// Slack thread key, never the DB row id) is `source_task_id`.
    fn task(source_task_id: &str, state: TaskState, worktree_path: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: 1000,
            source: "slack".into(),
            source_task_id: source_task_id.into(),
            workflow: "reply".into(),
            mode: "implement".into(),
            repo: Some("web".into()),
            worktree_path: worktree_path.map(str::to_string),
            branch: None,
            base_commit: None,
            state,
            priority: 0,
            title: format!("task {source_task_id}"),
            url: None,
            source_payload: None,
            finished_at: None,
            created_at: "2026-07-23T00:00:00Z".into(),
            updated_at: "2026-07-23T00:00:00Z".into(),
            last_signal_at: None,
        }
    }

    fn pane(label: &str) -> SessionInfo {
        SessionInfo {
            session_id: format!("w1:p1|{label}"),
            label: Some(label.to_string()),
            cwd: None,
        }
    }

    /// The trap that made the 1Password probes dead code for their whole life
    /// (#289): in toml 0.9 `FromStr for Value` parses a **single value**, not a
    /// document, so `"a = 1".parse::<Value>()` is an error. The detection
    /// helper used it and therefore always answered "no op:// anywhere" — with
    /// the probes gated on that answer, they never ran.
    #[test]
    fn a_toml_document_needs_the_table_parser_not_the_value_parser() {
        let doc = "token = \"op://Dev/Herdr/token\"\n";
        assert!(
            doc.parse::<toml::Value>().is_err(),
            "if Value ever parses a document, the comments explaining Table are stale"
        );
        let table = doc.parse::<toml::Table>().expect("Table parses a document");
        assert!(
            table
                .values()
                .any(|v| toml_mentions_scheme(v, SecretScheme::OnePassword))
        );
    }

    /// Only a real string value counts — the commented-out example `totsuka
    /// init` writes must not switch the 1Password probes on.
    #[test]
    fn only_a_live_op_reference_counts() {
        let commented = "# api_key_ref = \"op://Dev/Openrouter/api_key\"\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(
            !commented
                .values()
                .any(|v| toml_mentions_scheme(v, SecretScheme::OnePassword))
        );

        // Nested and inside an array, both of which `plugin_init_config`
        // would resolve.
        let nested = "[a.b]\nk = \"op://v/i/f\"\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(
            nested
                .values()
                .any(|v| toml_mentions_scheme(v, SecretScheme::OnePassword))
        );
        let array = "k = [\"plain\", \"op://v/i/f\"]\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(
            array
                .values()
                .any(|v| toml_mentions_scheme(v, SecretScheme::OnePassword))
        );
    }

    /// A skip is not a pass and not a failure: it must leave `doctor` green
    /// (exit 0 is decided by `ok`) while still being distinguishable.
    #[test]
    fn a_skipped_check_does_not_fail_doctor_but_is_marked() {
        let check = Check::skip("plugin:x", "would prompt", "run `op signin`");
        assert!(check.ok, "a skip must not turn doctor red");
        assert!(check.skipped);
        assert!(!check.warning, "skipped and warning are different states");
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["skipped"], true);
        // `warning` stays absent, so consumers written before #289 see the
        // same document shape they always did.
        assert!(json["warning"].is_null(), "{json}");

        let passed = Check::ok("plugin:x", "fine");
        assert!(
            serde_json::to_value(&passed).unwrap()["skipped"].is_null(),
            "a passing check must not grow a skipped field"
        );
    }

    /// A fake process result, for the pure probe helpers.
    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    /// A readiness where every measured backend reports `state`.
    fn readiness(state: BackendReadiness) -> SecretReadiness {
        SecretReadiness {
            onepassword: state,
            bitwarden: state,
        }
    }

    #[test]
    fn readiness_only_blocks_when_a_prompt_is_possible() {
        assert!(BackendReadiness::NotUsed.may_resolve());
        assert!(BackendReadiness::Ready.may_resolve());
        assert!(!BackendReadiness::WouldPrompt.may_resolve());
    }

    /// Classification goes through [`SecretRef`], so a scheme added in
    /// `orchestrator-core` cannot be silently missing here — the `match` in
    /// `SecretScheme::of` stops compiling instead of quietly answering
    /// "silent", which is what let `cmd:` reach 1 gate out of 3 (#444).
    #[test]
    fn schemes_are_classified_from_the_reference_type() {
        assert_eq!(SecretScheme::of("op://Dev/X/y"), SecretScheme::OnePassword);
        assert_eq!(SecretScheme::of("cmd:gh auth token"), SecretScheme::Command);
        assert_eq!(
            SecretScheme::of("bw:totsuka-slack/password"),
            SecretScheme::Bitwarden
        );
        assert_eq!(
            SecretScheme::of("keychain:totsuka/token"),
            SecretScheme::Silent
        );
        assert_eq!(SecretScheme::of("${TOTSUKA_TOKEN}"), SecretScheme::Silent);
        assert_eq!(SecretScheme::of("plain-value"), SecretScheme::Silent);
        // Malformed: the resolver rejects it before any CLI is spawned, so
        // there is nothing to gate.
        assert_eq!(SecretScheme::of("op://only-vault"), SecretScheme::Silent);
    }

    /// The `op://` gate follows the measured session; `cmd:` has nothing to
    /// measure and is therefore unconditional (#444).
    #[test]
    fn the_op_gate_follows_the_session_and_the_cmd_gate_never_opens() {
        let ready = readiness(BackendReadiness::Ready);
        let blocked = readiness(BackendReadiness::WouldPrompt);

        assert!(ready.skip_for("op://Dev/X/y").is_none());
        assert_eq!(
            blocked.skip_for("op://Dev/X/y"),
            Some(SecretSkip::ONEPASSWORD)
        );

        for readiness in [ready, blocked] {
            assert_eq!(
                readiness.skip_for("cmd:gh auth token"),
                Some(SecretSkip::COMMAND)
            );
            assert!(readiness.skip_for("keychain:totsuka/token").is_none());
            assert!(readiness.skip_for("${TOTSUKA_TOKEN}").is_none());
        }
    }

    /// The plugin gate walks **every string leaf**, because
    /// `plugin_init_config` resolves every string leaf.
    #[test]
    fn the_plugin_gate_finds_a_reference_at_any_depth() {
        let blocked = readiness(BackendReadiness::WouldPrompt);
        let nested = toml::Value::Table(
            "[a.b]\nk = [\"plain\", \"op://v/i/f\"]\n"
                .parse::<toml::Table>()
                .unwrap(),
        );
        assert_eq!(blocked.skip_in_toml(&nested), Some(SecretSkip::ONEPASSWORD));

        let silent = toml::Value::Table(
            "[a]\nk = \"keychain:totsuka/x\"\n"
                .parse::<toml::Table>()
                .unwrap(),
        );
        assert!(blocked.skip_in_toml(&silent).is_none());
    }

    /// The "left unresolved here" checks read like reporting, but they are
    /// gates: whatever `deferred_note` declines falls through to a real
    /// `resolve()`, which spawns the backend and can prompt on stdin.
    #[test]
    fn deferred_notes_cover_exactly_the_schemes_that_must_not_resolve() {
        let ready = readiness(BackendReadiness::Ready);
        let blocked = readiness(BackendReadiness::WouldPrompt);

        let note = ready
            .deferred_note("op://Dev/X/y", "api_key_ref")
            .expect("a note");
        assert!(note.contains("a 1Password session is active"), "{note}");
        let note = blocked
            .deferred_note("op://Dev/X/y", "api_key_ref")
            .expect("a note");
        assert!(note.contains("doctor stays non-interactive"), "{note}");

        let note = ready
            .deferred_note("cmd:gh auth token", "[hooks].auth_token_ref")
            .expect("a note");
        assert!(
            note.starts_with("[hooks].auth_token_ref is a cmd: reference"),
            "{note}"
        );

        // Silent schemes are resolved normally — no note, and no gate.
        assert!(
            ready
                .deferred_note("keychain:totsuka/token", "api_key_ref")
                .is_none()
        );
        assert!(
            ready
                .deferred_note("${TOTSUKA_TOKEN}", "api_key_ref")
                .is_none()
        );
    }

    /// `{target}` is what lets one wording serve gates that probe different
    /// things (a plugin, the hook receiver, a list of agents).
    #[test]
    fn skip_actions_name_what_would_have_been_probed() {
        assert_eq!(
            SecretSkip::ONEPASSWORD.action("the receiver"),
            "run `op signin`, then re-run `totsuka doctor` to probe the receiver"
        );
        assert!(
            SecretSkip::COMMAND
                .action("this plugin")
                .contains("test this plugin by hand"),
            "{}",
            SecretSkip::COMMAND.action("this plugin")
        );
    }

    /// Every probe outcome maps to a stated readiness and says something.
    ///
    /// This branching is what decides whether doctor resolves secrets at all,
    /// and it had **no test** while it was inlined in the spawn — which is how
    /// the 1Password probes stayed dead code for their whole life (#289).
    #[test]
    fn every_op_probe_outcome_maps_to_a_readiness() {
        for (probe, expected) in [
            (CliProbe::Missing, BackendReadiness::WouldPrompt),
            (
                CliProbe::VersionFailed { code: 127 },
                BackendReadiness::WouldPrompt,
            ),
            (
                CliProbe::Unrunnable {
                    error: "permission denied".to_string(),
                },
                BackendReadiness::WouldPrompt,
            ),
            (
                CliProbe::Probed {
                    version: "2.30.0".to_string(),
                    session: false,
                },
                BackendReadiness::WouldPrompt,
            ),
            (
                CliProbe::Probed {
                    version: "2.30.0".to_string(),
                    session: true,
                },
                BackendReadiness::Ready,
            ),
        ] {
            let mut checks = Vec::new();
            assert_eq!(
                backend_checks(&ONEPASSWORD_PROBE, probe.clone(), &mut checks),
                expected,
                "{probe:?}"
            );
            assert!(
                !checks.is_empty(),
                "every outcome must report something: {probe:?}"
            );
        }
    }

    /// A dead session is advisory, not a failure: doctor stays green and the
    /// gated checks report themselves as skipped instead.
    #[test]
    fn a_dead_op_session_warns_without_failing_doctor() {
        let mut checks = Vec::new();
        backend_checks(
            &ONEPASSWORD_PROBE,
            CliProbe::Probed {
                version: "2.30.0".to_string(),
                session: false,
            },
            &mut checks,
        );
        assert!(
            checks.iter().any(|c| c.detail.contains("2.30.0")),
            "the version is reported even when the session is dead: {checks:?}"
        );
        let session = checks
            .iter()
            .find(|c| c.name == "1password-session")
            .expect("a session check");
        assert!(session.warning, "{session:?}");
        assert!(session.ok, "a dead session must not turn doctor red");
    }

    /// A reference can reach doctor without ever appearing in `config.toml`:
    /// `Cx::load_config` applies the `TOTSUKA_*` env overrides **after**
    /// parsing, and two of them carry secret references.
    ///
    /// Measuring readiness from the file alone reported `NotUsed`, which opens
    /// the gate — and `check_hook_socket` / `check_plugins` then resolve for
    /// real, which is the unattended prompt the gate exists to prevent.
    #[test]
    fn an_override_supplied_reference_counts_as_in_use() {
        let cfg = RootConfig::from_toml_str(
            r#"
[hooks]
auth_token_ref = "op://Dev/totsuka/hook-token"
"#,
        )
        .unwrap();
        assert!(override_mentions_scheme(&cfg, SecretScheme::OnePassword));
        assert!(!override_mentions_scheme(&cfg, SecretScheme::Command));

        let cfg = RootConfig::from_toml_str(
            r#"
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4-5"
api_key_ref = "cmd:gh auth token"
"#,
        )
        .unwrap();
        assert!(override_mentions_scheme(&cfg, SecretScheme::Command));
        assert!(!override_mentions_scheme(&cfg, SecretScheme::OnePassword));

        // A config with neither field set must not switch any probe on.
        let cfg = RootConfig::from_toml_str("").unwrap();
        assert!(!override_mentions_scheme(&cfg, SecretScheme::OnePassword));
        assert!(!override_mentions_scheme(&cfg, SecretScheme::Command));

        // Silent schemes never gate anything, wherever they came from.
        let cfg = RootConfig::from_toml_str(
            r#"
[hooks]
auth_token_ref = "keychain:totsuka/hook-token"
"#,
        )
        .unwrap();
        assert!(!override_mentions_scheme(&cfg, SecretScheme::OnePassword));
        assert!(!override_mentions_scheme(&cfg, SecretScheme::Command));
    }

    /// A config can mix an `op://` plugin with a `cmd:` one, and the two
    /// recoveries do not substitute for each other: `op signin` does nothing
    /// for a `cmd:` agent, and "test it by hand" omits the sign-in.
    #[test]
    fn a_mixed_skip_list_names_every_recovery() {
        let action = combined_skip_action(
            &[SecretSkip::ONEPASSWORD, SecretSkip::COMMAND],
            "those agents",
        );
        assert!(action.contains("op signin"), "{action}");
        assert!(
            action.contains("`totsuka run` resolves the config"),
            "{action}"
        );
        assert!(action.contains("those agents"), "{action}");
    }

    /// Repeats collapse, so five `op://` agents still read as one instruction.
    #[test]
    fn repeated_skip_reasons_are_stated_once() {
        let action = combined_skip_action(
            &[
                SecretSkip::ONEPASSWORD,
                SecretSkip::ONEPASSWORD,
                SecretSkip::ONEPASSWORD,
            ],
            "those agents",
        );
        assert_eq!(action, SecretSkip::ONEPASSWORD.action("those agents"));
    }

    /// The `bw:` gate follows the vault state, the same way the `op://` gate
    /// follows the session — a Bitwarden user must not get a weaker `doctor`
    /// than a 1Password user just because the scheme is newer.
    #[test]
    fn the_bw_gate_follows_the_vault_state() {
        let unlocked = SecretReadiness {
            onepassword: BackendReadiness::WouldPrompt,
            bitwarden: BackendReadiness::Ready,
        };
        let locked = SecretReadiness {
            onepassword: BackendReadiness::Ready,
            bitwarden: BackendReadiness::WouldPrompt,
        };
        assert!(unlocked.skip_for("bw:x/password").is_none());
        assert_eq!(
            locked.skip_for("bw:x/password"),
            Some(SecretSkip::BITWARDEN)
        );
        // Each backend is gated on its own state, not on a shared verdict.
        assert!(locked.skip_for("op://Dev/X/y").is_none());
        assert_eq!(
            unlocked.skip_for("op://Dev/X/y"),
            Some(SecretSkip::ONEPASSWORD)
        );
    }

    /// The `bw:` skip has to name the stdin prompt: unlike 1Password's
    /// biometric dialog, an unattended `totsuka run` that hits it just stops
    /// with nothing on screen.
    #[test]
    fn the_bw_skip_names_the_stdin_prompt_and_the_recovery() {
        assert!(
            SecretSkip::BITWARDEN.detail.contains("stdin"),
            "{}",
            SecretSkip::BITWARDEN.detail
        );
        let action = SecretSkip::BITWARDEN.action("this plugin");
        assert!(action.contains("bw unlock"), "{action}");
        assert!(action.contains("BW_SESSION"), "{action}");
        assert!(action.contains("this plugin"), "{action}");
    }

    #[test]
    fn a_bw_reference_gets_a_deferred_note_on_both_vault_states() {
        let unlocked = SecretReadiness {
            onepassword: BackendReadiness::NotUsed,
            bitwarden: BackendReadiness::Ready,
        };
        let locked = SecretReadiness {
            onepassword: BackendReadiness::NotUsed,
            bitwarden: BackendReadiness::WouldPrompt,
        };
        let note = unlocked
            .deferred_note("bw:x/password", "api_key_ref")
            .expect("a note");
        assert!(note.contains("the Bitwarden vault is unlocked"), "{note}");
        let note = locked
            .deferred_note("bw:x/password", "api_key_ref")
            .expect("a note");
        assert!(note.contains("see the bitwarden checks above"), "{note}");
    }

    /// `bw status` exits 0 in every state, so the exit code is not the answer
    /// — the verdict is the `status` field. Anything unreadable must count as
    /// locked: guessing "unlocked" sends the gated probes into the prompt.
    #[test]
    fn only_an_unlocked_bw_status_counts_as_ready() {
        let ok = |body: &str| bw_vault_unlocked(&output(0, body.as_bytes(), b""));
        assert!(ok(r#"{"status":"unlocked","userEmail":"a@example.com"}"#));
        assert!(!ok(r#"{"status":"locked"}"#));
        assert!(!ok(r#"{"status":"unauthenticated"}"#));
        // Unreadable shapes fail closed.
        assert!(!ok("not json at all"));
        assert!(!ok("{}"));
        assert!(!ok(r#"{"status":42}"#));
        // A non-zero exit is not ready regardless of what it printed.
        assert!(!bw_vault_unlocked(&output(
            1,
            br#"{"status":"unlocked"}"#,
            b""
        )));
    }

    /// Each backend's probe must name **its own** CLI. The bug #699 fixed was
    /// a shared message telling every operator to install 1Password's.
    #[test]
    fn each_backend_probe_names_its_own_cli() {
        let mut checks = Vec::new();
        backend_checks(&BITWARDEN_PROBE, CliProbe::Missing, &mut checks);
        let check = checks.first().expect("a check");
        assert!(!check.ok, "{check:?}");
        assert!(check.detail.contains("bw:"), "{check:?}");
        let action = check.action.as_deref().unwrap_or_default();
        assert!(action.contains("brew install bitwarden-cli"), "{action}");
        assert!(!action.contains("1password"), "{action}");
    }

    /// Both backends must report the same four outcomes, so neither gets a
    /// quieter `doctor` than the other.
    #[test]
    fn both_backends_report_every_probe_outcome_symmetrically() {
        for spec in [&ONEPASSWORD_PROBE, &BITWARDEN_PROBE] {
            for (probe, expected) in [
                (CliProbe::Missing, BackendReadiness::WouldPrompt),
                (
                    CliProbe::VersionFailed { code: 127 },
                    BackendReadiness::WouldPrompt,
                ),
                (
                    CliProbe::Unrunnable {
                        error: "permission denied".to_string(),
                    },
                    BackendReadiness::WouldPrompt,
                ),
                (
                    CliProbe::Probed {
                        version: "1.2.3".to_string(),
                        session: false,
                    },
                    BackendReadiness::WouldPrompt,
                ),
                (
                    CliProbe::Probed {
                        version: "1.2.3".to_string(),
                        session: true,
                    },
                    BackendReadiness::Ready,
                ),
            ] {
                let mut checks = Vec::new();
                assert_eq!(
                    backend_checks(spec, probe.clone(), &mut checks),
                    expected,
                    "{} {probe:?}",
                    spec.name
                );
                assert!(!checks.is_empty(), "{} {probe:?}", spec.name);
            }
        }
    }

    /// A missing binary fails with the install next-action (§7).
    #[test]
    fn a_missing_op_binary_fails_with_an_install_action() {
        let mut checks = Vec::new();
        backend_checks(&ONEPASSWORD_PROBE, CliProbe::Missing, &mut checks);
        let check = checks.first().expect("a check");
        assert!(!check.ok, "{check:?}");
        assert!(
            check
                .action
                .as_deref()
                .unwrap_or_default()
                .contains("brew install 1password-cli"),
            "{check:?}"
        );
    }

    #[test]
    fn unknown_task_id_is_an_orphan() {
        // The DB knows no task with source id "gone-9": crashed dispatch,
        // pre-#210 leftovers, or a deleted row — a true orphan.
        let tasks = vec![task("C1:1.0", TaskState::Running, Some("/wt/1"))];
        let orphans =
            classify_orphan_panes("herdr", vec![pane("totsuka gone-9")], &tasks, |_| true);
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].reason.contains("no matching task"));
    }

    #[test]
    fn non_terminal_task_pane_is_not_an_orphan() {
        // The pane is in use — never a candidate, even with the worktree
        // gone. The label carries the source task id, which for Slack is a
        // non-numeric thread key: correlation must be a string match on
        // source_task_id, never a parse against the DB row id.
        for state in [
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Verifying,
            TaskState::Escalated,
        ] {
            let tasks = vec![task("C1:1.0", state, None)];
            let orphans =
                classify_orphan_panes("herdr", vec![pane("totsuka C1:1.0")], &tasks, |_| false);
            assert!(orphans.is_empty(), "state {state} must be kept");
        }
    }

    #[test]
    fn terminal_task_with_missing_worktree_is_an_orphan() {
        // The #210 linkage failed (manual `git worktree remove`, refused
        // release, crash): terminal + worktree gone ⇒ candidate.
        let tasks = vec![task("42", TaskState::Done, Some("/wt/7"))];
        let orphans = classify_orphan_panes("herdr", vec![pane("totsuka 42")], &tasks, |_| false);
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].reason.contains("worktree is gone"));

        // A terminal task that never had a worktree recorded counts too.
        let tasks = vec![task("C2:9.9", TaskState::Cancelled, None)];
        let orphans =
            classify_orphan_panes("herdr", vec![pane("totsuka C2:9.9")], &tasks, |_| true);
        assert_eq!(orphans.len(), 1);
    }

    #[test]
    fn terminal_task_with_retained_worktree_is_kept() {
        // Retention policies (keep_7d etc.) hold the worktree on purpose; the
        // pane's lifetime tracks the worktree's (ADR-0010).
        let tasks = vec![task("42", TaskState::Done, Some("/wt/7"))];
        let orphans = classify_orphan_panes("herdr", vec![pane("totsuka 42")], &tasks, |_| true);
        assert!(orphans.is_empty());
    }

    #[test]
    fn any_non_terminal_match_wins_when_source_ids_collide() {
        // source_task_id is only unique per source: with several matching
        // tasks (retry rows, cross-source collision) the conservative side
        // wins — one live task keeps the pane.
        let tasks = vec![
            task("42", TaskState::Done, None),
            task("42", TaskState::Running, None),
        ];
        let orphans = classify_orphan_panes("herdr", vec![pane("totsuka 42")], &tasks, |_| false);
        assert!(orphans.is_empty(), "the running match must keep the pane");
    }
}
