//! `totsuka doctor` — environment diagnosis (§5.1, F-24): git, config, state
//! DB, installed plugins (with a live probe), LLM key resolution, and orphan
//! worktrees (with an interactive cleanup proposal).

use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::plugin_host;
use orchestrator_core::config::{self, RootConfig};
use orchestrator_core::ports::git::GitRunner;
use orchestrator_core::worktree::WorktreeManager;
use serde::Serialize;

use crate::common::{CliError, Cx, plugin_spec, secret_resolver};
use crate::init_cmd::git_version;

/// `serde` `skip_serializing_if` predicate: omit a `false` flag from the JSON.
fn is_false(b: &bool) -> bool {
    !*b
}

/// One diagnostic result. `action` follows the "cause + next action" rule (§7).
///
/// Three severities: an `ok` check passes silently; a `warning` is advisory
/// (`ok` stays true, so it never fails `doctor`) yet still carries an action; a
/// failure (`ok = false`) is what makes `doctor` exit non-zero.
#[derive(Debug, Serialize)]
struct Check {
    name: String,
    ok: bool,
    /// Advisory finding: reported with its action but does not fail `doctor`.
    #[serde(skip_serializing_if = "is_false")]
    warning: bool,
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
            detail: detail.into(),
            action: None,
        }
    }
    fn fail(name: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: false,
            warning: false,
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
            detail: detail.into(),
            action: Some(action.into()),
        }
    }
}

/// Execute `totsuka doctor`.
pub fn run(cx: &Cx, json: bool) -> Result<(), CliError> {
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

    // Config presence + full offline validation.
    let cfg = match cx.load_config() {
        Ok(cfg) => {
            let env_fn = |k: &str| env.get(k).cloned();
            let store = cx.store();
            // Hook capability is not yet declared in plugin manifests
            // (protocol 0.1.3, #132); `None` = unknown skips the
            // `[hooks].auth_token_ref` advisory until manifests declare it.
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
            checks.push(Check::ok(
                "state-db",
                format!("{} opens", cx.state_db_path().display()),
            ));
            Some(db)
        }
        Err(e) => {
            checks.push(Check::fail(
                "state-db",
                e.to_string(),
                "run `totsuka run` once to create it",
            ));
            None
        }
    };

    if let Some(cfg) = &cfg {
        check_hooks(cx, cfg, &env, &mut checks);
        check_plugins(cx, cfg, &env, &mut checks);
        check_llm_key(cfg, &env, &mut checks);
        check_onepassword(cx, &env, &mut checks);
        check_orphans(cfg, &env, db.as_ref(), json, &mut checks)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in &checks {
            if !check.ok {
                println!(
                    "FAIL: {} — {} → {}",
                    check.name,
                    check.detail,
                    check.action.as_deref().unwrap_or("see docs")
                );
            } else if check.warning {
                println!(
                    "warn: {} — {} → {}",
                    check.name,
                    check.detail,
                    check.action.as_deref().unwrap_or("see docs")
                );
            } else {
                println!("ok:   {} — {}", check.name, check.detail);
            }
        }
    }
    if checks.iter().any(|c| !c.ok) {
        return Err("doctor found problems → follow the actions above".into());
    }
    Ok(())
}

/// 1Password backend probes (#156), fired **only when** `config.toml` or a
/// `plugins/*.toml` actually contains an `op://` reference: `op --version`
/// (CLI present) and `op whoami` (session established — unlike `op read`, it
/// never triggers a biometric prompt). No `op://` in config ⇒ no checks.
fn check_onepassword(cx: &Cx, env: &HashMap<String, String>, checks: &mut Vec<Check>) {
    if !config_mentions_onepassword(cx) {
        return;
    }
    let Some(op) = which("op", env) else {
        checks.push(Check::fail(
            "1password",
            "config references op:// secrets but the 1Password CLI (op) is not on PATH",
            "install it (`brew install 1password-cli`) or switch the references to keychain:/${ENV}",
        ));
        return;
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
                "reinstall the 1Password CLI (`brew reinstall 1password-cli`)",
            ));
            return;
        }
        Err(e) => {
            checks.push(Check::fail(
                "1password",
                format!("cannot run `op`: {e}"),
                "install the 1Password CLI (`brew install 1password-cli`)",
            ));
            return;
        }
    }
    // Session check: `op whoami` fails when not signed in, without prompting.
    match std::process::Command::new(&op).arg("whoami").output() {
        Ok(out) if out.status.success() => {
            checks.push(Check::ok("1password-session", "op session is active"));
        }
        _ => {
            checks.push(Check::warn(
                "1password-session",
                "no active 1Password session",
                "run `op signin` before `totsuka run` so op:// references resolve",
            ));
        }
    }
}

/// Whether `config.toml` or any `plugins/*.toml` contains an `op://` secret
/// reference (textual scan — resolution stays lazy, this only gates doctor).
fn config_mentions_onepassword(cx: &Cx) -> bool {
    let mut sources: Vec<PathBuf> = vec![cx.config_path.clone()];
    if let Ok(entries) = std::fs::read_dir(cx.plugin_config_dir()) {
        sources.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "toml")),
        );
    }
    sources
        .iter()
        .any(|path| std::fs::read_to_string(path).is_ok_and(|content| content.contains("op://")))
}

/// All Claude Code hook-mechanism probes (#141): assets, script dependencies,
/// the Bearer token, the spool backlog, and (when a receiver is live) UDS
/// connectivity. Extends the single asset check that shipped with #137.
fn check_hooks(cx: &Cx, cfg: &RootConfig, env: &HashMap<String, String>, checks: &mut Vec<Check>) {
    check_hook_assets(cx, cfg, checks);
    check_hook_deps(env, checks);
    check_hook_token(cfg, env, checks);
    check_spool(cx, cfg, env, checks);
    check_hook_socket(cx, cfg, env, checks);
}

/// Refresh the static hook scripts + per-workflow settings (idempotent, same
/// writeout as `totsuka run`, so `doctor` doubles as "materialize the hooks"),
/// then verify every asset exists with the embedded content and the expected
/// mode (0700 scripts / 0600 settings, N-02 tamper resistance).
fn check_hook_assets(cx: &Cx, cfg: &RootConfig, checks: &mut Vec<Check>) {
    if let Err(e) = crate::hooks::install(&cx.paths, cfg) {
        checks.push(Check::fail(
            "hooks",
            format!("could not write hook scripts/settings: {e}"),
            "check permissions on $XDG_DATA_HOME/totsuka/hooks",
        ));
        return;
    }
    let issues = crate::hooks::verify_assets(&cx.paths, cfg);
    if issues.is_empty() {
        let dir = crate::hooks::hooks_dir(&cx.paths);
        checks.push(Check::ok(
            "hooks",
            format!(
                "{} scripts (0700) + {} workflow settings (0600) under {}",
                crate::hooks::script_count(),
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
        checks.push(Check::fail(
            "hooks",
            format!("hook assets are inconsistent after a repair attempt: {detail}"),
            "a persistent mismatch on a writable dir means the asset is being tampered with (N-02) → investigate",
        ));
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

/// The Bearer token that authenticates hook POSTs (E-03) must resolve. An unset
/// `auth_token_ref` is advisory (the 0600 socket is still a barrier), not a hard
/// failure, since a config with no hook-capable agent never needs it.
fn check_hook_token(cfg: &RootConfig, env: &HashMap<String, String>, checks: &mut Vec<Check>) {
    match &cfg.hooks.auth_token_ref {
        None => checks.push(Check::warn(
            "hook-token",
            "[hooks].auth_token_ref is unset → hook POSTs are accepted on the 0600 socket without a Bearer token",
            "set [hooks].auth_token_ref (e.g. keychain:totsuka/hook-token) before using a hook-capable agent",
        )),
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(_) => checks.push(Check::ok("hook-token", "[hooks].auth_token_ref resolves")),
            Err(e) => checks.push(Check::fail(
                "hook-token",
                format!("[hooks].auth_token_ref does not resolve: {e}"),
                "export the referenced env var or store the token in the Keychain",
            )),
        },
    }
}

/// The spool directory (E-07 at-least-once fallback) must be writable, and a
/// non-empty backlog is surfaced as an advisory — spooled events replay
/// automatically on the next `totsuka run`, but a growing backlog signals the
/// receiver has been unreachable.
fn check_spool(cx: &Cx, cfg: &RootConfig, env: &HashMap<String, String>, checks: &mut Vec<Check>) {
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
    let token = cfg
        .hooks
        .auth_token_ref
        .as_ref()
        .and_then(|reference| secret_resolver(env).resolve(reference).ok());
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

    let body = r#"{"job_id":"job-0-0","doctor_probe":true}"#;
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /claude-events HTTP/1.1\r\n\
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
    for name in enabled {
        match plugin_spec(cx, cfg, name, env) {
            // `plugin_spec` already resolved plugins/{name}.toml (with secrets)
            // into `init_config`; reuse it rather than re-reading and hitting
            // the Keychain a second time.
            Ok(spec) => {
                let init = spec.init_config.clone();
                specs.push((spec, init));
            }
            // Failure may be "not installed" or a plugins/{name}.toml
            // parse/secret-resolution error — point at both.
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "install it (`totsuka plugin install <dir>`) or fix plugins/{name}.toml if it is already installed",
            )),
        }
    }
    if specs.is_empty() {
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
    for (name, result) in runtime.block_on(plugin_host::validate_all(specs)) {
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
                format!("fix plugins/{name}.toml"),
            )),
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "check the binary and protocol compatibility",
            )),
        }
    }
}

/// The LLM API key reference must resolve (no network call).
fn check_llm_key(cfg: &RootConfig, env: &HashMap<String, String>, checks: &mut Vec<Check>) {
    let Some(llm) = &cfg.llm else {
        checks.push(Check::ok(
            "llm",
            "no [llm] configured (repo selection falls back to hints/pending)",
        ));
        return;
    };
    let Some(reference) = &llm.api_key_ref else {
        checks.push(Check::ok("llm", "[llm] configured without api_key_ref"));
        return;
    };
    match secret_resolver(env).resolve(reference) {
        Ok(_) => checks.push(Check::ok("llm", "api_key_ref resolves")),
        Err(e) => checks.push(Check::fail(
            "llm",
            format!("api_key_ref does not resolve: {e}"),
            "export the variable or store the key in the Keychain",
        )),
    }
}

/// Detect orphan worktrees (F-24) and, interactively, offer to remove them.
fn check_orphans(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    db: Option<&orchestrator_core::adapters::StateDb>,
    json: bool,
    checks: &mut Vec<Check>,
) -> Result<(), CliError> {
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
    if !json && io::stdin().is_terminal() {
        for (repo_name, repo_path, orphan) in &orphans {
            print!(
                "remove orphan worktree {} (repo {repo_name})? [y/N]: ",
                orphan.display()
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
                    println!("removed {}", orphan.display());
                } else {
                    println!(
                        "could not remove (dirty?): {} → remove manually with `git worktree remove --force`",
                        out.stderr.trim()
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
            "run `totsuka doctor` in a terminal to clean them up interactively",
        ));
    }
    Ok(())
}
