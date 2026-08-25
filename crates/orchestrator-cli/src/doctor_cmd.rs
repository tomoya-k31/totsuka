//! `totsuka doctor` — environment diagnosis (§5.1, F-24): git, config, state
//! DB, installed plugins (with a live probe), LLM key resolution, and orphan
//! worktrees (with an interactive cleanup proposal).

use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
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
use orchestrator_core::ports::SecretString;
use orchestrator_core::ports::git::GitRunner;
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

/// Whether resolving an `op://` reference can happen without a prompt (#289).
///
/// [ADR-0006](../../ai-docs/decisions/adr-0006-onepassword-secret-backend.md)
/// requires `doctor` to stay non-interactive, but `op read` only prompts when
/// no session is established. So rather than approximating with "is there a
/// TTY", doctor asks the question directly — `op whoami` answers it and never
/// prompts itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpReadiness {
    /// No `op://` reference anywhere in config: nothing can prompt, so no
    /// check needs gating.
    NotUsed,
    /// A session is established — `op read` answers from it. Probes that need
    /// a secret run exactly as before.
    Ready,
    /// `op` is missing, broken, or has no session. Resolving would pop a
    /// biometric prompt, or hang forever when nobody is watching.
    WouldPrompt,
}

impl OpReadiness {
    /// Whether a probe that must resolve an `op://` reference may proceed.
    fn may_resolve(self) -> bool {
        !matches!(self, Self::WouldPrompt)
    }

    /// The reason to put on a check skipped because of this state.
    fn skip_reason(self) -> &'static str {
        "resolving its op:// reference would prompt for 1Password unlock \
         (doctor stays non-interactive)"
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
        // 1Password goes **first** (#289). Several checks below resolve
        // secrets, and `op read` prompts (or hangs unattended) without a
        // session — so the answer to "may we resolve?" has to exist before
        // anything acts on it. Running this last, as it used to, meant
        // `check_plugins` had already resolved the very references the `llm`
        // and `hook-token` checks claimed the probes would cover.
        let op = check_onepassword(cx, &env, &mut checks);
        check_worktree_location(cfg, &env, &mut checks);
        check_hooks(cx, cfg, config_ok, &env, op, args, &mut checks);
        check_plugins(cx, cfg, &env, op, &mut checks);
        check_llm_key(cfg, &env, args, op, &mut checks);
        check_orphans(cfg, &env, db.as_ref(), args, &mut checks)?;
        check_orphan_panes(cx, cfg, &env, db.as_ref(), args, op, &mut checks)?;
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

/// 1Password backend probes (#156), fired **only when** `config.toml` or a
/// `config.toml` actually contains an `op://` reference: `op --version`
/// (CLI present) and `op whoami` (session established — unlike `op read`, it
/// never triggers a biometric prompt). No `op://` in config ⇒ no checks.
/// Returns whether the rest of `doctor` may resolve `op://` references
/// without prompting (#289).
fn check_onepassword(
    cx: &Cx,
    env: &HashMap<String, String>,
    checks: &mut Vec<Check>,
) -> OpReadiness {
    if !config_mentions_onepassword(cx) {
        return OpReadiness::NotUsed;
    }
    let Some(op) = which("op", env) else {
        checks.push(Check::fail(
            "1password",
            "config references op:// secrets but the 1Password CLI (op) is not on PATH",
            "install it (macOS: `brew install 1password-cli`, other platforms: \
             https://developer.1password.com/docs/cli) or switch the references to \
             `keychain:` / `${ENV}`",
        ));
        // No `op` binary: every resolution would fail anyway, and the probes
        // that need one must not pretend otherwise.
        return OpReadiness::WouldPrompt;
    };
    match std::process::Command::new(&op).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            checks.push(Check::ok("1password", format!("op {version} on PATH")));
        }
        Ok(out) => {
            checks.push(Check::fail(
                "1password",
                format!(
                    "`op --version` exited with {}",
                    out.status.code().unwrap_or(-1)
                ),
                "reinstall the 1Password CLI (macOS: `brew reinstall 1password-cli`)",
            ));
            return OpReadiness::WouldPrompt;
        }
        Err(e) => {
            checks.push(Check::fail(
                "1password",
                format!("cannot run `op`: {e}"),
                "install the 1Password CLI (macOS: `brew install 1password-cli`, \
                 other platforms: https://developer.1password.com/docs/cli)",
            ));
            return OpReadiness::WouldPrompt;
        }
    }
    // Session check: `op whoami` fails when not signed in, without prompting.
    // This is also the answer to "may the checks below resolve?" — `op read`
    // prompts only when there is no session, so asking `whoami` measures the
    // real condition instead of approximating it with a TTY test (#289).
    match std::process::Command::new(&op).arg("whoami").output() {
        Ok(out) if out.status.success() => {
            checks.push(Check::ok("1password-session", "op session is active"));
            OpReadiness::Ready
        }
        _ => {
            checks.push(Check::warn(
                "1password-session",
                "no active 1Password session — probes that need an op:// secret are skipped",
                "run `op signin`, then re-run `totsuka doctor` for the full picture",
            ));
            OpReadiness::WouldPrompt
        }
    }
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
[[workflows]]
name = "w"
source = "github"
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

/// Whether `[llm].api_key_ref` is an `op://` reference — the one secret
/// `plugin_spec` resolves for a task-source plugin that does *not* live in
/// that plugin's own config file.
fn llm_key_is_onepassword(cfg: &RootConfig) -> bool {
    cfg.llm
        .as_ref()
        .and_then(|llm| llm.api_key_ref.as_deref())
        .is_some_and(|reference| reference.starts_with("op://"))
}

/// Whether the plugin's `[<name>]` table holds an `op://` reference in a real
/// string value. Per-plugin counterpart of [`config_mentions_onepassword`], so
/// one plugin's 1Password usage does not gate the probes of every other
/// plugin.
fn plugin_config_mentions_onepassword(cfg: &RootConfig, name: &str) -> bool {
    cfg.plugin_settings(name).is_some_and(toml_has_op_reference)
}

/// Whether launching `name` would make `plugin_spec` resolve an `op://`
/// reference (#289).
///
/// Two independent doors, both inside `plugin_spec`: `plugin_init_config`
/// resolves **every string leaf** of the plugin's `[<name>]` table, and
/// `llm_info` resolves `[llm].api_key_ref` — but only for a task source.
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
fn plugin_needs_onepassword(cx: &Cx, cfg: &RootConfig, name: &str) -> bool {
    let declared_task_source = cfg
        .plugin(name)
        .is_some_and(|p| p.kind == ConfigPluginKind::TaskSource);
    let manifest_task_source = cx
        .store()
        .manifest_of(name)
        .ok()
        .flatten()
        .is_some_and(|m| m.kind == plugin_protocol::manifest::PluginKind::TaskSource);
    let is_task_source = declared_task_source || manifest_task_source;
    plugin_config_mentions_onepassword(cfg, name) || (is_task_source && llm_key_is_onepassword(cfg))
}

/// Whether `config.toml` contains an `op://` secret reference in an **actual
/// string value** (resolution stays lazy, this only gates doctor). The file is
/// TOML-parsed and its string leaves walked, so a commented-out example — like
/// the one `totsuka init` generates — never triggers the 1Password checks.
///
/// One file since #554: plugin settings live in the same document, so the
/// separate `plugins/*.toml` sweep this used to do is now the same walk.
fn config_mentions_onepassword(cx: &Cx) -> bool {
    let sources: Vec<PathBuf> = vec![cx.config_path.clone()];
    sources.iter().any(|path| {
        std::fs::read_to_string(path).is_ok_and(|content| {
            // `toml::Table`, not `toml::Value`: in toml 0.9 `FromStr for Value`
            // parses a *single value*, so `"a = 1".parse::<Value>()` is an
            // error ("unexpected content, expected nothing") for every real
            // config file. This helper silently answered "no op:// anywhere"
            // for its whole life, which meant the 1Password checks below never
            // ran at all (#289). `Table` is the document parser.
            content
                .parse::<toml::Table>()
                .is_ok_and(|table| table.values().any(toml_has_op_reference))
        })
    })
}

/// Whether any string leaf of a TOML value starts with `op://`.
fn toml_has_op_reference(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(s) => s.starts_with("op://"),
        toml::Value::Array(items) => items.iter().any(toml_has_op_reference),
        toml::Value::Table(table) => table.values().any(toml_has_op_reference),
        _ => false,
    }
}

/// Whether any string leaf of a TOML value starts with `cmd:` (#444).
fn toml_has_cmd_reference(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(s) => s.starts_with("cmd:"),
        toml::Value::Array(items) => items.iter().any(toml_has_cmd_reference),
        toml::Value::Table(table) => table.values().any(toml_has_cmd_reference),
        _ => false,
    }
}

/// Whether launching `name` would make `plugin_spec` run a `cmd:` reference's
/// command (#444). Same two doors as [`plugin_needs_onepassword`]: the
/// plugin's own `[<name>]` table, and `[llm].api_key_ref` for a task source.
///
/// Unlike `op://` there is no session to measure — doctor cannot know whether
/// the command is prompt-free (`cmd:op read …` is a real spelling), so a
/// plugin that mentions one is always skipped rather than probed (#289's
/// non-interactive principle).
fn plugin_needs_command_exec(cx: &Cx, cfg: &RootConfig, name: &str) -> bool {
    let declared_task_source = cfg
        .plugin(name)
        .is_some_and(|p| p.kind == ConfigPluginKind::TaskSource);
    let manifest_task_source = cx
        .store()
        .manifest_of(name)
        .ok()
        .flatten()
        .is_some_and(|m| m.kind == plugin_protocol::manifest::PluginKind::TaskSource);
    let is_task_source = declared_task_source || manifest_task_source;
    let llm_key_is_command = cfg
        .llm
        .as_ref()
        .and_then(|llm| llm.api_key_ref.as_deref())
        .is_some_and(|reference| reference.starts_with("cmd:"));
    let plugin_mentions_cmd = cfg
        .plugin_settings(name)
        .is_some_and(toml_has_cmd_reference);
    plugin_mentions_cmd || (is_task_source && llm_key_is_command)
}

/// All Claude Code hook-mechanism probes (#141): assets, script dependencies,
/// the Bearer token, the spool backlog, and (when a receiver is live) UDS
/// connectivity. Extends the single asset check that shipped with #137.
fn check_hooks(
    cx: &Cx,
    cfg: &RootConfig,
    config_ok: bool,
    env: &HashMap<String, String>,
    op: OpReadiness,
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
    check_hook_token(cfg, env, &hook_workflows, &unknown_workflows, checks);
    check_spool(cx, cfg, env, args, checks);
    check_hook_socket(cx, cfg, env, op, checks);
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
        // `op://` is deliberately not resolved here (a real `op read` can
        // prompt for biometrics / hang unattended); the 1password probes
        // check presence + session without prompting (ADR-0006).
        Some(reference) if reference.starts_with("op://") => checks.push(Check::ok(
            "hook-token",
            "[hooks].auth_token_ref is an op:// reference, left unresolved here \
             (doctor stays non-interactive; see the 1password checks above)",
        )),
        // Same for `cmd:` — resolving would execute the command (#444).
        Some(reference) if reference.starts_with("cmd:") => checks.push(Check::ok(
            "hook-token",
            "[hooks].auth_token_ref is a cmd: reference, left unresolved here \
             (doctor stays non-interactive; the command runs when `totsuka run` \
             resolves the config)",
        )),
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(_) => checks.push(Check::ok("hook-token", "[hooks].auth_token_ref resolves")),
            Err(e) => checks.push(Check::fail(
                "hook-token",
                format!("[hooks].auth_token_ref does not resolve: {e}"),
                "export the referenced env var, store the token in the Keychain, or use an op:// reference",
            )),
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
fn check_hook_socket(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    op: OpReadiness,
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
    // A receiver is live: prove connectivity + auth with a self-POST.
    //
    // This resolves `auth_token_ref` for real — the second `op://` door in
    // doctor, and one the `hook-token` check's "not resolved here" message
    // does not cover (#289). Probing without the token would be worse than
    // not probing: the receiver would answer 401 and the check would report a
    // token mismatch that does not exist.
    let token_ref = cfg.hooks.auth_token_ref.as_deref();
    if !op.may_resolve() && token_ref.is_some_and(|r| r.starts_with("op://")) {
        checks.push(Check::skip(
            "hook-socket",
            format!(
                "a receiver is live at {} but {}",
                socket_path.display(),
                op.skip_reason()
            ),
            "run `op signin`, then re-run `totsuka doctor` to probe the receiver",
        ));
        return;
    }
    // A `cmd:` token has no session to measure — resolving would execute the
    // command, which doctor never does (#444, #289). Unconditional, unlike
    // the op gate above.
    if token_ref.is_some_and(|r| r.starts_with("cmd:")) {
        checks.push(Check::skip(
            "hook-socket",
            format!(
                "a receiver is live at {} but resolving the cmd: token would \
                 execute a command (doctor stays non-interactive)",
                socket_path.display()
            ),
            "the command runs when `totsuka run` resolves the config; \
             test the receiver by hand if unsure",
        ));
        return;
    }
    let token = token_ref.and_then(|reference| secret_resolver(env).resolve(reference).ok());
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
        // A socket file that exists but refuses/drops the connection is almost
        // always a stale socket from a prior `totsuka run` (the file lingers on
        // Linux after the listener exits). Since `doctor` must pass when the
        // orchestrator is *not* running, this is advisory, not a failure.
        Err(e) => checks.push(Check::warn(
            "hook-socket",
            format!(
                "socket {} exists but is not accepting connections: {e}",
                socket_path.display()
            ),
            "the receiver is not running, or this is a stale socket — ignore if `totsuka run` is not active, else remove the stale socket file and restart",
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
    op: OpReadiness,
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
    // counts what got into `specs`, so the tracker check below cannot tell
    // "nothing was skipped" from "everything was skipped" without this — and
    // being skipped is the *normal* state for a `cmd:` token, not an edge case
    // (#542 review).
    let mut not_probed: Vec<(String, &'static str)> = Vec::new();
    for name in enabled {
        // `plugin_spec` resolves secrets, so a plugin that needs 1Password
        // cannot be probed while `op read` would prompt (#289). Decided per
        // plugin: one plugin's op:// reference must not silence the probes of
        // plugins that need no secret at all.
        if !op.may_resolve() && plugin_needs_onepassword(cx, cfg, name) {
            checks.push(Check::skip(
                &format!("plugin:{name}"),
                op.skip_reason(),
                "run `op signin`, then re-run `totsuka doctor` to probe this plugin",
            ));
            not_probed.push((name.clone(), "its op:// reference would prompt"));
            continue;
        }
        // A `cmd:` reference has no session to measure: doctor cannot know
        // the command is prompt-free (`cmd:op read …` is a real spelling), so
        // the probe is always skipped rather than executed (#444, #289).
        if plugin_needs_command_exec(cx, cfg, name) {
            checks.push(Check::skip(
                &format!("plugin:{name}"),
                "resolving its cmd: reference would execute a command \
                 (doctor stays non-interactive)",
                "the command runs when `totsuka run` resolves the config; \
                 test it by hand if unsure",
            ));
            not_probed.push((
                name.clone(),
                "resolving its cmd: reference would run a command",
            ));
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
        // Still report the tracker check: with nothing probed it can only say
        // "cannot tell", and saying nothing at all reads as "no conflicts".
        check_tracker_claims(&[], &not_probed, checks);
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
            Ok(v) if v.valid => {
                checks.push(Check::ok(
                    &format!("plugin:{name}"),
                    "launches and accepts its config",
                ));
            }
            Ok(v) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                v.errors.join("; "),
                format!("fix `[{name}]` in config.toml"),
            )),
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "check the binary and protocol compatibility",
            )),
        }
    }
    check_tracker_claims(&validated, &not_probed, checks);
}

/// Repositories claimed as a tracker target by more than one source (#542).
///
/// **The only place this is visible.** Each plugin's own `config/validate`
/// checks its own list, so the configs are individually valid and the conflict
/// exists only in the union — which nothing but the Orchestrator assembles.
///
/// Reads the claims [`validate_all`](plugin_host::validate_all) already
/// gathered, so it inherits the launch gating exactly rather than restating it.
/// `not_probed` carries what that gating *excluded*, which the claims alone
/// cannot express: a plugin that was never launched reports no claims, and so
/// does a plugin that genuinely claims nothing.
///
/// **A conflict is always reported, even when the picture is incomplete.** Only
/// the all-clear verdict degrades to a skip: "these two claim the same
/// repository" stays true whatever the un-probed plugins would have said, while
/// "every repository routes to exactly one tracker" does not.
fn check_tracker_claims(
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
            "trackers",
            format!("cannot tell: {} was not probed", unseen.join(", ")),
            "doctor stays non-interactive, so a plugin whose secrets need a prompt or a \
             command is never launched — check those configs by hand, or run `totsuka run` \
             which resolves them",
        ));
        return;
    }
    if registry.is_empty() {
        // Every source was probed and none claims anything: the normal state
        // for a config with no tracker set up. Not worth a line.
        return;
    }
    checks.push(Check::ok(
        "trackers",
        format!(
            "{} repositories route to exactly one tracker",
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
    op: OpReadiness,
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
        // the prompt in exchange for a real answer.
        //
        // The wording matters (#289). This used to claim the reference was
        // "checked by the 1password probes", which those probes never did —
        // they check that `op` exists and a session is live, not that this
        // particular item resolves. Now that they also run *first* and gate
        // the checks that do resolve, the honest statement is the narrow one.
        Some(reference) if reference.starts_with("op://") => checks.push(Check::ok(
            "llm",
            match op {
                OpReadiness::Ready => {
                    "api_key_ref is an op:// reference, left unresolved here \
                     (a 1Password session is active, so `totsuka run` will resolve it)"
                }
                _ => {
                    "api_key_ref is an op:// reference, left unresolved here \
                     (doctor stays non-interactive; see the 1password checks above)"
                }
            },
        )),
        // Same for `cmd:` — resolving would execute the command (#444).
        Some(reference) if reference.starts_with("cmd:") => checks.push(Check::ok(
            "llm",
            "api_key_ref is a cmd: reference, left unresolved here (doctor stays \
             non-interactive; the command runs when `totsuka run` resolves the config)",
        )),
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(_) => checks.push(Check::ok("llm", "api_key_ref resolves")),
            Err(e) => {
                checks.push(Check::fail(
                    "llm",
                    format!("api_key_ref does not resolve: {e}"),
                    "export the variable, store the key in the Keychain, or use an op:// reference",
                ));
                // No key to probe with; the online check would only restate
                // this failure.
                return;
            }
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
/// which are gated on [`OpReadiness`] and reported as skipped instead.
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
    op: OpReadiness,
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
    let mut skipped: Vec<&str> = Vec::new();
    for name in &agents {
        // Same gate as `check_plugins` (#289, #444): launching the agent
        // resolves its secrets. Tracked separately so the check can say it
        // saw only part of the picture — silently probing fewer agents would
        // under-report orphans and read as "none found".
        if (!op.may_resolve() && plugin_needs_onepassword(cx, cfg, name))
            || plugin_needs_command_exec(cx, cfg, name)
        {
            skipped.push(name.as_str());
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
        checks.push(Check::skip(
            "panes",
            format!(
                "did not list panes via {} — {}",
                skipped.join(", "),
                op.skip_reason()
            ),
            "run `op signin`, then re-run `totsuka doctor` to see orphan panes for those agents",
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

    // --- `trackers` (#542) -------------------------------------------------

    fn validated(name: &str, claims: &[(&str, &str)]) -> plugin_host::ValidatedPlugin {
        plugin_host::ValidatedPlugin {
            name: name.to_string(),
            result: Ok(plugin_protocol::methods::ConfigValidateResult {
                valid: true,
                errors: Vec::new(),
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

    fn tracker_checks(
        validated: &[plugin_host::ValidatedPlugin],
        not_probed: &[(String, &'static str)],
    ) -> Vec<Check> {
        let mut checks = Vec::new();
        check_tracker_claims(validated, not_probed, &mut checks);
        checks
    }

    #[test]
    fn trackers_pass_when_every_source_was_probed_and_none_conflict() {
        let checks = tracker_checks(
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
    fn trackers_cannot_conclude_while_a_plugin_was_never_probed() {
        let checks = tracker_checks(
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
        let checks = tracker_checks(&[validated("notion", &[("web-app", "DB2")]), failed], &[]);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].skipped, "{:?}", checks[0]);
        assert!(checks[0].detail.contains("github"), "{:?}", checks[0]);
    }

    /// Nothing probed at all (every plugin skipped) must not read as an
    /// all-clear either — the path `check_plugins` takes when `specs` is empty.
    #[test]
    fn trackers_say_nothing_conclusive_when_nothing_was_probed() {
        let checks = tracker_checks(
            &[],
            &[("github".to_string(), "its op:// reference would prompt")],
        );
        assert_eq!(checks.len(), 1);
        assert!(checks[0].skipped, "{:?}", checks[0]);
    }

    /// Every source probed, none claims anything: a config with no tracker set
    /// up. Silent — a line saying "0 repositories route" is noise.
    #[test]
    fn trackers_are_silent_when_no_source_claims_anything() {
        let checks = tracker_checks(&[validated("slack", &[])], &[]);
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
        assert!(table.values().any(toml_has_op_reference));
    }

    /// Only a real string value counts — the commented-out example `totsuka
    /// init` writes must not switch the 1Password probes on.
    #[test]
    fn only_a_live_op_reference_counts() {
        let commented = "# api_key_ref = \"op://Dev/Openrouter/api_key\"\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(!commented.values().any(toml_has_op_reference));

        // Nested and inside an array, both of which `plugin_init_config`
        // would resolve.
        let nested = "[a.b]\nk = \"op://v/i/f\"\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(nested.values().any(toml_has_op_reference));
        let array = "k = [\"plain\", \"op://v/i/f\"]\n"
            .parse::<toml::Table>()
            .unwrap();
        assert!(array.values().any(toml_has_op_reference));
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

    #[test]
    fn readiness_only_blocks_when_a_prompt_is_possible() {
        assert!(OpReadiness::NotUsed.may_resolve());
        assert!(OpReadiness::Ready.may_resolve());
        assert!(!OpReadiness::WouldPrompt.may_resolve());
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
