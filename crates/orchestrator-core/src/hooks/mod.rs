//! Static hook scripts + per-workflow `orchestrator-<workflow>.json` rendering
//! (#131 H-01/H-03, #137).
//!
//! The six hook scripts are baked into the binary with [`include_str!`] and
//! written to `$XDG_DATA_HOME/totsuka/hooks/` at `totsuka run` / `totsuka
//! doctor` startup (0700, idempotent by content hash so a version bump refreshes
//! them but an unchanged run touches nothing). Per-workflow settings are
//! rendered next to them (0600), with the `prompt`-type Stop hook added only for
//! `verification = "llm"` workflows.
//!
//! Job-specific values (job_id / endpoint / token / spool dir / prompt
//! context) are deliberately kept **out** of these files: `agent-ide-herdr`
//! injects them as env (`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` /
//! `TOTSUKA_HOOK_TOKEN` / `TOTSUKA_HOOK_SPOOL_DIR` /
//! `TOTSUKA_PROMPT_CONTEXT`, #132 `HookLaunchSpec`), so a single rendered
//! `--settings` path is reusable across `claude --resume` (H-03).

pub mod codex;
pub mod opencode;

use std::io;
use std::path::{Path, PathBuf};

use crate::config::{RootConfig, VerificationMode, WorkflowConfig};
use crate::domain::signal::{MARKER_COMPLETED, MARKER_FAILED, MARKER_NEEDS_INPUT};
use crate::paths::Paths;
use crate::tool::{ToolKind, ToolProfile};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Whether the config can resolve any task to a tool of `kind`: the global
/// default, a repository default, or a workflow pin. Shared by the per-tool
/// asset installers ([`codex`], [`opencode`]) to decide whether they may touch
/// anything outside totsuka's own dirs.
pub(crate) fn config_references_kind(cfg: &RootConfig, kind: ToolKind) -> bool {
    let kind_of = |name: &str| {
        cfg.tool(name)
            .map(|t| t.kind)
            .or_else(|| ToolProfile::builtin(name).map(|p| p.kind))
    };
    cfg.default_tool
        .as_deref()
        .into_iter()
        .chain(cfg.repositories.iter().filter_map(|r| r.tool.as_deref()))
        .chain(cfg.workflows.iter().filter_map(|w| w.tool.as_deref()))
        .any(|name| kind_of(name) == Some(kind))
}

/// The static hook scripts, embedded in file order (`hook-common.sh` first so a
/// reader sees the shared helpers before the entry points that source them).
const HOOK_SCRIPTS: &[(&str, &str)] = &[
    ("hook-common.sh", include_str!("hook-common.sh")),
    ("on-stop.sh", include_str!("on-stop.sh")),
    ("on-notification.sh", include_str!("on-notification.sh")),
    ("on-session-start.sh", include_str!("on-session-start.sh")),
    ("on-session-end.sh", include_str!("on-session-end.sh")),
    (
        "on-user-prompt-submit.sh",
        include_str!("on-user-prompt-submit.sh"),
    ),
];

/// Rubric embedded into the `prompt`-type Stop hook when a `verification =
/// "llm"` workflow sets no `rubric` of its own.
pub const DEFAULT_RUBRIC: &str = "作業が指示された要件を実際に満たしているかを、対象リポジトリの現在のコードと状態に基づいて検証してください。表面的な自己申告ではなく、変更が意図どおり機能し破綻や取りこぼしがないことを確認してください。";

/// Intermediate-Stop exemption for the `prompt`-type hook, mirroring
/// `on-stop.sh`'s R-02/D-12 rule: a Stop with background tasks still running is
/// a heartbeat, not a completion claim. Without this the judge blocks every
/// such Stop — the pane shows a spurious "Stop hook error" and the session is
/// forced to busy-wait in-turn instead of yielding to the task-notification
/// re-invoke (real-machine finding on the `slack-reply` workflow).
const BACKGROUND_EXEMPTION: &str = "ただし、バックグラウンドタスク（サブエージェント等）が実行中のままターンを終える中間停止は完了申告ではありません。その場合は検証もブロックも行わず停止を許可してください。完了判定はバックグラウンドタスクが残っていない停止に対してのみ行います。";

/// Appended to the rubric so the verifying model re-emits the status marker the
/// `on-stop.sh` command hook parses (D-12). Built from the shared marker
/// constants in [`crate::domain::signal`] so the rendered convention cannot
/// drift from the receiver's
/// [`MARKER_SELF_REPORT_INSTRUCTION`](crate::run::hooks) counterpart.
fn marker_convention() -> String {
    format!(
        "検証結果を踏まえ、応答の最終行に必ず次のいずれかのマーカーを付けてください: {MARKER_COMPLETED} / {MARKER_NEEDS_INPUT} / {MARKER_FAILED}"
    )
}

/// Directory holding the scripts and rendered settings.
pub fn hooks_dir(paths: &Paths) -> PathBuf {
    paths.data_dir().join("hooks")
}

/// Number of static hook scripts written under the hooks dir (`doctor` reports
/// it in the asset check).
pub fn script_count() -> usize {
    HOOK_SCRIPTS.len()
}

/// One hook asset that is missing, mis-permissioned, or content-drifted, found
/// by [`verify_assets`]. `doctor`'s `check_hook_assets` turns these into
/// actionable findings without rewriting anything.
pub struct AssetIssue {
    /// The offending file.
    pub path: PathBuf,
    /// What is wrong (missing / wrong mode / content mismatch).
    pub problem: String,
}

/// Verify every hook asset exists with the embedded content and the expected
/// mode (0700 scripts, 0600 settings) **without** writing anything. Returns the
/// issues found (empty ⇒ all assets are correct). This is the read-only
/// counterpart to [`install`]: `doctor` calls `install` first to materialize
/// and self-heal, then `verify_assets` to surface anything that is still wrong
/// (e.g. active tampering, or a dir the repair could not write to).
pub fn verify_assets(paths: &Paths, cfg: &RootConfig) -> Vec<AssetIssue> {
    let dir = hooks_dir(paths);
    let mut issues = Vec::new();
    for (name, content) in HOOK_SCRIPTS {
        verify_one(&dir.join(name), content.as_bytes(), 0o700, &mut issues);
    }
    for wf in &cfg.workflows {
        let rendered = render_settings(&dir, wf);
        verify_one(
            &settings_path(paths, &wf.name),
            rendered.as_bytes(),
            0o600,
            &mut issues,
        );
    }
    issues
}

/// Check one asset's existence, content hash, and mode, pushing any problem
/// onto `issues`.
pub(crate) fn verify_one(path: &Path, expected: &[u8], mode: u32, issues: &mut Vec<AssetIssue>) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            issues.push(AssetIssue {
                path: path.to_path_buf(),
                problem: "missing".to_string(),
            });
            return;
        }
        Err(e) => {
            issues.push(AssetIssue {
                path: path.to_path_buf(),
                problem: format!("unreadable: {e}"),
            });
            return;
        }
    };
    if Sha256::digest(&bytes) != Sha256::digest(expected) {
        issues.push(AssetIssue {
            path: path.to_path_buf(),
            problem: "content does not match the embedded asset".to_string(),
        });
    }
    if let Some(actual) = file_mode(path)
        && actual != mode
    {
        issues.push(AssetIssue {
            path: path.to_path_buf(),
            problem: format!("mode {actual:04o}, expected {mode:04o}"),
        });
    }
}

/// The permission bits of `path` (`0o777`-masked), or `None` on non-Unix / stat
/// failure.
#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// Absolute path of a workflow's rendered settings file (the value wired into
/// `--settings` via `HookLaunchSpec.settings_path`).
pub fn settings_path(paths: &Paths, workflow: &str) -> PathBuf {
    hooks_dir(paths).join(format!("orchestrator-{workflow}.json"))
}

/// Write the static scripts (0700) and render every configured workflow's
/// settings (0600) under the hooks dir. Idempotent: a file whose bytes already
/// match is left untouched (no rewrite), so second startups don't churn and the
/// `--settings` path a live session was launched with stays byte-stable.
pub fn install(paths: &Paths, cfg: &RootConfig) -> io::Result<()> {
    let dir = hooks_dir(paths);
    std::fs::create_dir_all(&dir)?;
    for (name, content) in HOOK_SCRIPTS {
        write_if_changed(&dir.join(name), content.as_bytes(), 0o700)?;
    }
    for wf in &cfg.workflows {
        let rendered = render_settings(&dir, wf);
        write_if_changed(&settings_path(paths, &wf.name), rendered.as_bytes(), 0o600)?;
    }
    Ok(())
}

/// Render a workflow's `orchestrator-<workflow>.json`. The `Stop` array always
/// carries the `on-stop.sh` command hook; `verification = "llm"` workflows also
/// get a `prompt`-type hook running the rubric in-session (D-01).
pub fn render_settings(dir: &Path, wf: &WorkflowConfig) -> String {
    let script = |name: &str| dir.join(name).to_string_lossy().into_owned();

    let mut stop = vec![json!({
        "hooks": [{ "type": "command", "command": script("on-stop.sh"), "timeout": 30 }]
    })];
    if wf.verification == VerificationMode::Llm {
        let rubric = wf.rubric.as_deref().unwrap_or(DEFAULT_RUBRIC);
        let convention = marker_convention();
        let prompt = format!("{rubric}\n\n{BACKGROUND_EXEMPTION}\n\n{convention}");
        stop.push(json!({
            "hooks": [{ "type": "prompt", "prompt": prompt, "timeout": 60 }]
        }));
    }

    let settings = json!({
        "hooks": {
            "Stop": stop,
            "Notification": [{
                "matcher": "permission_prompt|agent_needs_input|idle_prompt",
                "hooks": [{ "type": "command", "command": script("on-notification.sh"), "timeout": 10 }]
            }],
            "SessionStart": [{
                "hooks": [{ "type": "command", "command": script("on-session-start.sh"), "timeout": 10 }]
            }],
            "SessionEnd": [{
                "hooks": [{ "type": "command", "command": script("on-session-end.sh"), "timeout": 10 }]
            }],
            // Invisible prompt-context injection: rendered for every workflow;
            // the script no-ops when TOTSUKA_PROMPT_CONTEXT is unset.
            "UserPromptSubmit": [{
                "hooks": [{ "type": "command", "command": script("on-user-prompt-submit.sh"), "timeout": 10 }]
            }]
        }
    });
    serde_json::to_string_pretty(&settings).expect("settings JSON is always serializable")
}

/// Write `content` only when the on-disk bytes differ (content-hash compare),
/// then apply `mode`. Returns whether a write happened (drives the idempotency
/// tests).
pub(crate) fn write_if_changed(path: &Path, content: &[u8], mode: u32) -> io::Result<bool> {
    if let Ok(existing) = std::fs::read(path)
        && Sha256::digest(&existing) == Sha256::digest(content)
    {
        // Content is unchanged, but still re-apply the mode so a drifted
        // permission (e.g. a hook script that lost its exec bit) is repaired.
        set_mode(path, mode)?;
        return Ok(false);
    }
    std::fs::write(path, content)?;
    set_mode(path, mode)?;
    Ok(true)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// These tests exercise Unix-only behaviour (file mode bits 0700/0600, symlinked
// tool shims, shell-script execution), matching `set_mode`'s `#[cfg(not(unix))]`
// no-op: on non-Unix the hooks feature has no permissions to assert.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh, unique temp directory for one test (no external tempfile dep).
    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("totsuka-hooks-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Directory of the committed source scripts (what the tests exercise).
    fn script_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hooks")
    }

    /// Absolute path of a tool, or panic (tests need real coreutils/jq/curl).
    fn tool(name: &str) -> PathBuf {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "tool `{name}` not found on PATH");
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
    }

    /// Run `on-stop.sh` with `input` on stdin. `endpoint`/`token` are left unset
    /// so a successful branch spools its payload (no live UDS in a unit test);
    /// the spooled NDJSON is what we assert on. When `restricted_path` is set,
    /// the child runs with a PATH that excludes `jq`/`curl` (branch ⑤).
    fn run_stop(input: &str, spool: &Path, restricted_path: Option<&Path>) -> std::process::Output {
        let mut cmd = Command::new(tool("bash"));
        cmd.arg(script_dir().join("on-stop.sh"))
            .env("TOTSUKA_JOB_ID", "job-test")
            .env("TOTSUKA_HOOK_SPOOL_DIR", spool)
            .env_remove("TOTSUKA_HOOK_ENDPOINT")
            .env_remove("TOTSUKA_HOOK_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = restricted_path {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// All spooled NDJSON lines across every spool file in `dir`.
    fn spooled_lines(dir: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                lines.extend(text.lines().map(str::to_string));
            }
        }
        lines
    }

    fn spooled_json(dir: &Path) -> Vec<serde_json::Value> {
        spooled_lines(dir)
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    // --- Script branch tests (受け入れ条件: ①〜⑤) ---

    #[test]
    fn stop_without_job_id_is_inert() {
        // Codex hooks are registered globally and fire for personal sessions
        // too; without TOTSUKA_JOB_ID the script must do nothing — no block
        // JSON, no POST, no spool (#196 Phase 2 env gate).
        let spool = unique_dir("nojob");
        let input = r#"{"session_id":"s1","turn_id":"t1","stop_hook_active":false,"last_assistant_message":"no marker here"}"#;
        let mut cmd = Command::new(tool("bash"));
        cmd.arg(script_dir().join("on-stop.sh"))
            .env_remove("TOTSUKA_JOB_ID")
            .env("TOTSUKA_HOOK_SPOOL_DIR", &spool)
            .env_remove("TOTSUKA_HOOK_ENDPOINT")
            .env_remove("TOTSUKA_HOOK_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "a personal session is never blocked");
        assert!(
            spooled_lines(&spool).is_empty(),
            "nothing is posted/spooled"
        );
    }

    #[test]
    fn stop_codex_turn_id_rides_as_prompt_id() {
        // Codex names the turn key `turn_id` and has no `background_tasks`;
        // the payload must still carry it as `prompt_id` (idempotency key).
        let spool = unique_dir("turnid");
        let input = r#"{"session_id":"s1","turn_id":"turn-42","stop_hook_active":false,"last_assistant_message":"done <<STATUS:COMPLETED>>"}"#;
        let out = run_stop(input, &spool, None);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["prompt_id"], "turn-42");
        assert_eq!(events[0]["status"], "COMPLETED");
        assert_eq!(events[0]["background_tasks"], serde_json::json!([]));
    }

    #[test]
    fn notification_synthesizes_message_for_codex_permission_request() {
        // on-notification.sh doubles as the codex PermissionRequest hook: no
        // `message` field, so one is synthesized from `tool_name` — and stdout
        // must stay empty (any output would decide the approval).
        let spool = unique_dir("permreq");
        let input = r#"{"session_id":"s1","turn_id":"t1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_use_id":"tu1","tool_input":{"command":"rm -rf /tmp/x"}}"#;
        let mut cmd = Command::new(tool("bash"));
        cmd.arg(script_dir().join("on-notification.sh"))
            .env("TOTSUKA_JOB_ID", "job-test")
            .env("TOTSUKA_HOOK_SPOOL_DIR", &spool)
            .env_remove("TOTSUKA_HOOK_ENDPOINT")
            .env_remove("TOTSUKA_HOOK_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "stdout would decide the approval");
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["hook_event_name"], "Notification");
        assert_eq!(events[0]["message"], "permission_prompt: Bash");
    }

    #[test]
    fn stop_marker_present_posts_stop_status_and_does_not_block() {
        let spool = unique_dir("marker");
        // Two markers: the LAST one wins (D-12), reason attribute parsed.
        let input = r#"{"session_id":"s1","transcript_path":"/t.jsonl","prompt_id":"p1","stop_hook_active":false,"last_assistant_message":"first <<STATUS:COMPLETED>>\nfinal <<STATUS:NEEDS_INPUT reason=\"which branch?\">>","background_tasks":[]}"#;
        let out = run_stop(input, &spool, None);
        assert!(out.status.success());
        assert!(
            out.stdout.is_empty(),
            "no block JSON when a marker is present"
        );
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        // Canonical wire contract: the event kind rides `hook_event_name`.
        assert_eq!(events[0]["hook_event_name"], "Stop");
        assert_eq!(events[0]["status"], "NEEDS_INPUT");
        assert_eq!(events[0]["reason"], "which branch?");
        assert_eq!(events[0]["job_id"], "job-test");
        assert_eq!(events[0]["prompt_id"], "p1");
        assert_eq!(events[0]["transcript_path"], "/t.jsonl");
        // background_tasks is carried so the receiver can distinguish an
        // intermediate (heartbeat) Stop from a final one.
        assert_eq!(events[0]["background_tasks"], serde_json::json!([]));
    }

    #[test]
    fn stop_single_bracket_marker_is_accepted() {
        // Real agents (observed live) normalise the doubled brackets and emit a
        // single pair `<STATUS:COMPLETED>`. It must still be read as a completion,
        // not stranded as a missing marker. Also covers a mixed `<<...>` pair.
        for (msg, want) in [
            ("調べて直しました\n<STATUS:COMPLETED>", "COMPLETED"),
            ("done\n<<STATUS:COMPLETED>", "COMPLETED"),
            (
                "続けます\n<STATUS:NEEDS_INPUT reason=\"どのブランチ?\">",
                "NEEDS_INPUT",
            ),
        ] {
            let spool = unique_dir("single-bracket");
            let input = format!(
                r#"{{"session_id":"s1","prompt_id":"p1","stop_hook_active":false,"last_assistant_message":{},"background_tasks":[]}}"#,
                serde_json::Value::from(msg)
            );
            let out = run_stop(&input, &spool, None);
            assert!(out.status.success());
            assert!(
                out.stdout.is_empty(),
                "a single/mixed-bracket marker must not block: {msg}"
            );
            let events = spooled_json(&spool);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["hook_event_name"], "Stop");
            assert_eq!(events[0]["status"], want, "parsed status for: {msg}");
        }
    }

    #[test]
    fn stop_marker_absent_first_time_blocks_and_posts_unknown() {
        let spool = unique_dir("absent");
        let input = r#"{"session_id":"s1","prompt_id":"p1","stop_hook_active":false,"last_assistant_message":"no marker here","background_tasks":[]}"#;
        let out = run_stop(input, &spool, None);
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        let block: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(block["decision"], "block");
        // R-03: the corrective instruction must name the markers.
        assert!(
            block["reason"]
                .as_str()
                .unwrap()
                .contains("<<STATUS:COMPLETED>>")
        );
        // UNKNOWN is still posted for audit/counting.
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["status"], "UNKNOWN");
    }

    #[test]
    fn stop_marker_absent_reentrant_posts_unknown_without_blocking() {
        let spool = unique_dir("reentrant");
        let input = r#"{"session_id":"s1","prompt_id":"p1","stop_hook_active":true,"last_assistant_message":"still nothing","background_tasks":[]}"#;
        let out = run_stop(input, &spool, None);
        assert!(out.status.success());
        assert!(
            out.stdout.is_empty(),
            "no re-block when stop_hook_active=true"
        );
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["status"], "UNKNOWN");
    }

    #[test]
    fn stop_background_tasks_nonempty_sends_heartbeat_only() {
        let spool = unique_dir("bg");
        // A COMPLETED marker is present but must be ignored: this is an
        // intermediate Stop (R-02).
        let input = r#"{"session_id":"s1","prompt_id":"p1","stop_hook_active":false,"last_assistant_message":"working <<STATUS:COMPLETED>>","background_tasks":[{"id":"bg1"}]}"#;
        let out = run_stop(input, &spool, None);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        let events = spooled_json(&spool);
        assert_eq!(events.len(), 1);
        // An intermediate Stop stays `hook_event_name: "Stop"`; the non-empty
        // background_tasks is what makes the receiver treat it as a heartbeat.
        assert_eq!(events[0]["hook_event_name"], "Stop");
        assert!(
            !events[0]["background_tasks"].as_array().unwrap().is_empty(),
            "heartbeat carries the non-empty background_tasks: {}",
            events[0]
        );
    }

    #[test]
    fn stop_without_jq_spools_raw_input_and_exits_zero() {
        let spool = unique_dir("nojq");
        // PATH with the coreutils the script needs but NOT jq/curl.
        let bin = unique_dir("nojq-bin");
        for name in ["cat", "dirname", "date", "mkdir", "grep", "tail"] {
            std::os::unix::fs::symlink(tool(name), bin.join(name)).unwrap();
        }
        let input = r#"{"session_id":"s1","last_assistant_message":"x <<STATUS:COMPLETED>>","background_tasks":[]}"#;
        let out = run_stop(input, &spool, Some(&bin));
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        // The raw stdin is spooled verbatim (one NDJSON line).
        let lines = spooled_lines(&spool);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], input);
    }

    // --- on-user-prompt-submit.sh branch tests ---

    /// Run `on-user-prompt-submit.sh` with `context` as TOTSUKA_PROMPT_CONTEXT
    /// (`None` ⇒ unset). When `restricted_path` is set, the child runs with a
    /// PATH that excludes `jq` (fail-open branch).
    fn run_prompt_submit(
        context: Option<&str>,
        restricted_path: Option<&Path>,
    ) -> std::process::Output {
        let mut cmd = Command::new(tool("bash"));
        cmd.arg(script_dir().join("on-user-prompt-submit.sh"))
            .env_remove("TOTSUKA_PROMPT_CONTEXT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(ctx) = context {
            cmd.env("TOTSUKA_PROMPT_CONTEXT", ctx);
        }
        if let Some(path) = restricted_path {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s1","prompt":"user prompt"}"#)
            .unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn prompt_submit_emits_one_additional_context_line() {
        // A multi-line context with quotes must round-trip through the single
        // emitted JSON line (jq handles all escaping).
        let ctx = "返信スタイル: \"簡潔\"\n\n[orchestrator] end with <<STATUS:COMPLETED>>";
        let out = run_prompt_submit(Some(ctx), None);
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(stdout.trim_end().lines().count(), 1, "exactly one line");
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], ctx);
    }

    #[test]
    fn prompt_submit_without_context_env_is_silent() {
        for ctx in [None, Some("")] {
            let out = run_prompt_submit(ctx, None);
            assert!(out.status.success());
            assert!(out.stdout.is_empty(), "no output without a context");
        }
    }

    #[test]
    fn prompt_submit_without_jq_is_silent_and_exits_zero() {
        // PATH with the coreutils the script needs but NOT jq: fail-open, the
        // prompt submits without the injected context (D-09).
        let bin = unique_dir("ps-nojq-bin");
        std::os::unix::fs::symlink(tool("cat"), bin.join("cat")).unwrap();
        let out = run_prompt_submit(Some("some context"), Some(&bin));
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
    }

    // --- Rendering / idempotency / permission tests ---

    fn workflows_config(body: &str) -> RootConfig {
        RootConfig::from_toml_str(body).unwrap()
    }

    #[test]
    fn write_if_changed_is_idempotent_and_rewrites_on_hash_change() {
        let dir = unique_dir("wic");
        let p = dir.join("f");
        assert!(write_if_changed(&p, b"abc", 0o600).unwrap(), "first write");
        assert!(
            !write_if_changed(&p, b"abc", 0o600).unwrap(),
            "identical content is not rewritten"
        );
        assert!(
            write_if_changed(&p, b"abd", 0o600).unwrap(),
            "changed content is rewritten"
        );
    }

    #[test]
    fn install_is_idempotent_across_startups() {
        let base = unique_dir("install");
        let paths = Paths::from_env(|k| match k {
            "HOME" => Some(base.to_string_lossy().into_owned()),
            "XDG_DATA_HOME" => Some(base.join("data").to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap();
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "implement"
source = "github"
mode = "implement"
agent = "herdr"
output = "pull_request"
"#,
        );
        install(&paths, &cfg).unwrap();
        let script = hooks_dir(&paths).join("on-stop.sh");
        let settings = settings_path(&paths, "implement");
        let m1_script = std::fs::metadata(&script).unwrap().modified().unwrap();
        let m1_settings = std::fs::metadata(&settings).unwrap().modified().unwrap();
        // A second startup must not rewrite unchanged files (mtime is preserved
        // exactly because no write happens at all).
        install(&paths, &cfg).unwrap();
        assert_eq!(
            m1_script,
            std::fs::metadata(&script).unwrap().modified().unwrap()
        );
        assert_eq!(
            m1_settings,
            std::fs::metadata(&settings).unwrap().modified().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_sets_script_0700_and_settings_0600() {
        use std::os::unix::fs::PermissionsExt;
        let base = unique_dir("perms");
        let paths = Paths::from_env(|k| match k {
            "HOME" => Some(base.to_string_lossy().into_owned()),
            "XDG_DATA_HOME" => Some(base.join("data").to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap();
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "implement"
source = "github"
mode = "implement"
agent = "herdr"
output = "pull_request"
"#,
        );
        install(&paths, &cfg).unwrap();
        let script_mode = std::fs::metadata(hooks_dir(&paths).join("on-stop.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let settings_mode = std::fs::metadata(settings_path(&paths, "implement"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(script_mode, 0o700);
        assert_eq!(settings_mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn verify_assets_passes_after_install_and_flags_tampering() {
        use std::os::unix::fs::PermissionsExt;
        let base = unique_dir("verify");
        let paths = Paths::from_env(|k| match k {
            "HOME" => Some(base.to_string_lossy().into_owned()),
            "XDG_DATA_HOME" => Some(base.join("data").to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap();
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "implement"
source = "github"
mode = "implement"
agent = "herdr"
output = "pull_request"
"#,
        );
        install(&paths, &cfg).unwrap();
        // A freshly installed set is fully consistent.
        assert!(
            verify_assets(&paths, &cfg).is_empty(),
            "no issues right after install"
        );

        // Content drift is detected (doctor's N-02 tamper check).
        let script = hooks_dir(&paths).join("on-stop.sh");
        std::fs::write(&script, b"#!/bin/sh\necho tampered\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let issues = verify_assets(&paths, &cfg);
        assert!(
            issues
                .iter()
                .any(|i| i.path == script && i.problem.contains("content")),
            "tampered content flagged: {:?}",
            issues.iter().map(|i| &i.problem).collect::<Vec<_>>()
        );

        // A mode drift on the settings file is detected.
        install(&paths, &cfg).unwrap(); // repair the script first
        let settings = settings_path(&paths, "implement");
        std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).unwrap();
        let issues = verify_assets(&paths, &cfg);
        assert!(
            issues
                .iter()
                .any(|i| i.path == settings && i.problem.contains("mode")),
            "mode drift flagged: {:?}",
            issues.iter().map(|i| &i.problem).collect::<Vec<_>>()
        );
    }

    /// Parse a rendered settings string and return its `hooks.Stop` array.
    fn stop_hooks(rendered: &str) -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(rendered).unwrap();
        v["hooks"]["Stop"].as_array().unwrap().clone()
    }

    #[test]
    fn llm_workflow_gets_prompt_hook_with_default_rubric() {
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"
"#,
        );
        let rendered = render_settings(Path::new("/hooks"), &cfg.workflows[0]);
        let stop = stop_hooks(&rendered);
        assert_eq!(stop.len(), 2, "command + prompt hook");
        let prompt = &stop[1]["hooks"][0];
        assert_eq!(prompt["type"], "prompt");
        let text = prompt["prompt"].as_str().unwrap();
        assert!(text.contains(DEFAULT_RUBRIC), "default rubric embedded");
        assert!(
            text.contains(BACKGROUND_EXEMPTION),
            "R-02 intermediate-stop exemption embedded"
        );
        assert!(
            text.contains("<<STATUS:COMPLETED>>"),
            "marker convention embedded"
        );
    }

    #[test]
    fn llm_workflow_uses_custom_rubric_when_set() {
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"
rubric = "回答は対象リポジトリの実調査に基づくこと"
"#,
        );
        let rendered = render_settings(Path::new("/hooks"), &cfg.workflows[0]);
        let stop = stop_hooks(&rendered);
        assert_eq!(stop.len(), 2);
        let text = stop[1]["hooks"][0]["prompt"].as_str().unwrap();
        assert!(text.contains("回答は対象リポジトリの実調査に基づくこと"));
        assert!(
            !text.contains(DEFAULT_RUBRIC),
            "custom rubric replaces default"
        );
        assert!(
            text.contains(BACKGROUND_EXEMPTION),
            "exemption is appended even with a custom rubric"
        );
    }

    #[test]
    fn non_llm_workflows_have_no_prompt_hook() {
        for mode in ["human", "none"] {
            let cfg = workflows_config(&format!(
                r#"
[[workflows]]
name = "design"
source = "github"
mode = "plan"
agent = "herdr"
output = "none"
verification = "{mode}"
"#
            ));
            let rendered = render_settings(Path::new("/hooks"), &cfg.workflows[0]);
            let stop = stop_hooks(&rendered);
            assert_eq!(
                stop.len(),
                1,
                "only the command hook for verification={mode}"
            );
            assert_eq!(stop[0]["hooks"][0]["type"], "command");
        }
    }

    #[test]
    fn all_workflows_get_the_user_prompt_submit_hook() {
        // The invisible-context hook is rendered unconditionally (llm and
        // non-llm alike): the script no-ops when TOTSUKA_PROMPT_CONTEXT is
        // unset, so it is always safe to register.
        for verification in ["llm", "human", "none"] {
            let cfg = workflows_config(&format!(
                r#"
[[workflows]]
name = "wf"
source = "github"
mode = "implement"
agent = "herdr"
output = "pull_request"
verification = "{verification}"
"#
            ));
            let rendered = render_settings(Path::new("/hooks"), &cfg.workflows[0]);
            let v: serde_json::Value = serde_json::from_str(&rendered).unwrap();
            let entry = &v["hooks"]["UserPromptSubmit"][0];
            assert!(
                entry.get("matcher").is_none(),
                "no matcher for verification={verification}"
            );
            let hook = &entry["hooks"][0];
            assert_eq!(hook["type"], "command");
            assert_eq!(hook["command"], "/hooks/on-user-prompt-submit.sh");
            assert_eq!(hook["timeout"], 10);
        }
    }

    #[test]
    fn render_wires_command_paths_under_the_hooks_dir() {
        let cfg = workflows_config(
            r#"
[[workflows]]
name = "implement"
source = "github"
mode = "implement"
agent = "herdr"
output = "pull_request"
"#,
        );
        let rendered = render_settings(Path::new("/xdg/data/totsuka/hooks"), &cfg.workflows[0]);
        let v: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/xdg/data/totsuka/hooks/on-stop.sh"
        );
        assert_eq!(
            v["hooks"]["Notification"][0]["matcher"],
            "permission_prompt|agent_needs_input|idle_prompt"
        );
        assert_eq!(
            v["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "/xdg/data/totsuka/hooks/on-session-end.sh"
        );
    }
}
