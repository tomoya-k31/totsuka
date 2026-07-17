//! Static hook scripts + per-workflow `orchestrator-<workflow>.json` rendering
//! (#131 H-01/H-03, #137).
//!
//! The five hook scripts are baked into the binary with [`include_str!`] and
//! written to `$XDG_DATA_HOME/totsuka/hooks/` at `totsuka run` / `totsuka
//! doctor` startup (0700, idempotent by content hash so a version bump refreshes
//! them but an unchanged run touches nothing). Per-workflow settings are
//! rendered next to them (0600), with the `prompt`-type Stop hook added only for
//! `verification = "llm"` workflows.
//!
//! Job-specific values (job_id / endpoint / token / spool dir) are deliberately
//! kept **out** of these files: `agent-ide-herdr` injects them as env
//! (`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` /
//! `TOTSUKA_HOOK_SPOOL_DIR`, #132 `HookLaunchSpec`), so a single rendered
//! `--settings` path is reusable across `claude --resume` (H-03).

use std::io;
use std::path::{Path, PathBuf};

use orchestrator_core::config::{RootConfig, VerificationMode, WorkflowConfig};
use orchestrator_core::paths::Paths;
use serde_json::json;
use sha2::{Digest, Sha256};

/// The static hook scripts, embedded in file order (`hook-common.sh` first so a
/// reader sees the shared helpers before the entry points that source them).
const HOOK_SCRIPTS: &[(&str, &str)] = &[
    ("hook-common.sh", include_str!("hook-common.sh")),
    ("on-stop.sh", include_str!("on-stop.sh")),
    ("on-notification.sh", include_str!("on-notification.sh")),
    ("on-session-start.sh", include_str!("on-session-start.sh")),
    ("on-session-end.sh", include_str!("on-session-end.sh")),
];

/// Rubric embedded into the `prompt`-type Stop hook when a `verification =
/// "llm"` workflow sets no `rubric` of its own.
pub const DEFAULT_RUBRIC: &str = "作業が指示された要件を実際に満たしているかを、対象リポジトリの現在のコードと状態に基づいて検証してください。表面的な自己申告ではなく、変更が意図どおり機能し破綻や取りこぼしがないことを確認してください。";

/// Appended to the rubric so the verifying model re-emits the status marker the
/// `on-stop.sh` command hook parses (D-12).
const MARKER_CONVENTION: &str = "検証結果を踏まえ、応答の最終行に必ず次のいずれかのマーカーを付けてください: <<STATUS:COMPLETED>> / <<STATUS:NEEDS_INPUT reason=\"...\">> / <<STATUS:FAILED reason=\"...\">>";

/// Directory holding the scripts and rendered settings.
pub fn hooks_dir(paths: &Paths) -> PathBuf {
    paths.data_dir().join("hooks")
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
        let prompt = format!("{rubric}\n\n{MARKER_CONVENTION}");
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
            }]
        }
    });
    serde_json::to_string_pretty(&settings).expect("settings JSON is always serializable")
}

/// Write `content` only when the on-disk bytes differ (content-hash compare),
/// then apply `mode`. Returns whether a write happened (drives the idempotency
/// tests).
fn write_if_changed(path: &Path, content: &[u8], mode: u32) -> io::Result<bool> {
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
        assert_eq!(events[0]["event"], "stop");
        assert_eq!(events[0]["status"], "NEEDS_INPUT");
        assert_eq!(events[0]["reason"], "which branch?");
        assert_eq!(events[0]["job_id"], "job-test");
        assert_eq!(events[0]["prompt_id"], "p1");
        assert_eq!(events[0]["transcript_path"], "/t.jsonl");
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
        assert_eq!(events[0]["event"], "heartbeat");
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
