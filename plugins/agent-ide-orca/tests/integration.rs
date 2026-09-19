//! End-to-end plugin flow over a **fake orca CLI** (answers keyed by
//! subcommand + verb, in the shapes measured on orca 1.4.205): initialize →
//! task/dispatch → the exit deadman, and the pane-control surface —
//! attach, cancel, release, list, focus, snapshot — plus capability
//! negotiation and config/validate (F-32/F-33/F-37/F-38/F-94).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use agent_ide_orca::cli::OrcaCli;
use agent_ide_orca::config::OrcaConfig;
use agent_ide_orca::error::OrcaError;
use agent_ide_orca::server::{CliFactory, Server};

/// A scripted answer for one `orca <sub> <verb>` invocation.
#[derive(Clone)]
enum Canned {
    /// The envelope's `result`.
    Ok(Value),
    /// An `ok: false` envelope with this `error.code`.
    Err(&'static str),
}

/// A fake orca CLI: answers keyed by the first two args (e.g. "terminal wait").
/// Each key holds a queue; the last answer repeats once the queue drains.
#[derive(Clone, Default)]
struct FakeCli {
    scripts: Arc<Mutex<HashMap<String, Vec<Canned>>>>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl FakeCli {
    fn on(&self, key: &str, responses: Vec<Canned>) {
        self.scripts
            .lock()
            .unwrap()
            .insert(key.to_string(), responses);
    }

    fn calls_to(&self, key: &str) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| cli_key(c) == key)
            .cloned()
            .collect()
    }

    fn keys(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| cli_key(c))
            .collect()
    }
}

/// The value passed for `flag` in an argv (`--name x` → `x`).
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let at = argv.iter().position(|a| a == flag)?;
    argv.get(at + 1).map(String::as_str)
}

/// Key an invocation by its subcommand (+ verb).
fn cli_key(args: &[String]) -> String {
    match (args.first(), args.get(1)) {
        (Some(a), Some(b)) if !b.starts_with('-') => format!("{a} {b}"),
        (Some(a), _) => a.clone(),
        _ => String::new(),
    }
}

impl OrcaCli for FakeCli {
    fn run(&self, args: Vec<String>) -> impl Future<Output = Result<Value, OrcaError>> + Send {
        let key = cli_key(&args);
        self.calls.lock().unwrap().push(args);
        let mut scripts = self.scripts.lock().unwrap();
        let outcome = match scripts.get_mut(&key) {
            Some(queue) if queue.len() > 1 => queue.remove(0),
            Some(queue) => queue.first().cloned().unwrap_or(Canned::Ok(Value::Null)),
            None => Canned::Ok(Value::Null),
        };
        async move {
            match outcome {
                Canned::Ok(v) => Ok(v),
                Canned::Err(code) => Err(OrcaError::Orca {
                    code: code.into(),
                    message: code.into(),
                }),
            }
        }
    }
}

struct FakeFactory {
    cli: FakeCli,
}

impl CliFactory for FakeFactory {
    type Cli = FakeCli;
    fn build(&self, _config: &OrcaConfig) -> FakeCli {
        self.cli.clone()
    }
}

/// A driver around a `Server` writing to an in-memory line channel.
struct Driver {
    server: Server<FakeFactory>,
    out: mpsc::UnboundedReceiver<String>,
    next_id: i64,
}

impl Driver {
    fn new(cli: FakeCli) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            server: Server::new(FakeFactory { cli }, tx),
            out: rx,
            next_id: 0,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        assert!(self.server.handle_line(&line.to_string()).await);
        let resp = self.recv().await.expect("a response line");
        assert_eq!(resp["id"], id, "response id must match request");
        resp
    }

    async fn recv(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.out.recv())
            .await
            .expect("timed out waiting for plugin output")?;
        Some(serde_json::from_str(&line).expect("valid JSON line"))
    }

    async fn init(&mut self) -> Value {
        self.init_with(json!({})).await
    }

    async fn init_with(&mut self, config: Value) -> Value {
        self.call(
            "initialize",
            json!({ "protocol_version": "0.7.0", "config": config }),
        )
        .await
    }
}

const HANDLE: &str = "term_1";
const WORKTREE: &str = "/wt/agent-1";

/// `terminal create`'s answer.
fn created() -> Canned {
    Canned::Ok(json!({ "terminal": { "handle": HANDLE, "tabId": "tab1" } }))
}

/// A `terminal show` answer.
fn shown(connected: bool, cwd: &str) -> Canned {
    Canned::Ok(json!({ "terminal": {
        "handle": HANDLE, "connected": connected, "worktreePath": cwd,
        "title": "totsuka T-1",
    }}))
}

/// A `terminal show` answer once orca has recognised the agent — what the
/// dispatch waits for before it sends the prompt.
fn agent_shown() -> Canned {
    Canned::Ok(json!({ "terminal": {
        "handle": HANDLE, "connected": true, "worktreePath": WORKTREE,
        "title": "✳ Claude Code", "agentIdentity": "claude",
    }}))
}

/// A dispatch request with a resolved launch.
fn dispatch_params(resume: Option<&str>) -> Value {
    let mut p = json!({
        "task": { "id": "T-1", "source": "github", "title": "Do it", "body": "line one\nline two" },
        "worktree_path": WORKTREE,
        "mode": "implement",
        "task_number": 3,
        "repo_name": "web",
        "tool_launch": {
            "program": "claude",
            "args": ["--settings", "/cfg/hooks settings.json"],
            "env": { "TOTSUKA_JOB_ID": "3.1" }
        }
    });
    if let Some(sid) = resume {
        p["resume_session_id"] = json!(sid);
    }
    p
}

#[tokio::test]
async fn initialize_declares_the_herdr_capability_set() {
    let mut d = Driver::new(FakeCli::default());
    let init = d.init().await;
    let caps = &init["result"]["capabilities"];
    for declared in [
        "pane_control",
        "state_stream",
        "hook_completion",
        "diagnostics_snapshot",
    ] {
        assert_eq!(caps[declared], true, "{declared}: {init}");
    }
    for retired in ["design_preview", "plan_mode", "resume_session"] {
        assert!(caps.get(retired).is_none(), "{retired} must be gone");
    }
}

/// The manifest and `initialize` must declare the same set — the Orchestrator
/// gates on the manifest before the plugin ever runs.
#[test]
fn manifest_matches_the_declared_capabilities() {
    let manifest: toml_like::Manifest = toml_like::read();
    for declared in [
        "pane_control",
        "state_stream",
        "hook_completion",
        "diagnostics_snapshot",
    ] {
        assert!(manifest.declares(declared), "plugin.toml lacks {declared}");
    }
}

/// A tiny reader for the `[capabilities]` table, so this test needs no TOML
/// dependency.
mod toml_like {
    pub struct Manifest(String);

    pub fn read() -> Manifest {
        Manifest(
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"))
                .expect("plugin.toml"),
        )
    }

    impl Manifest {
        pub fn declares(&self, key: &str) -> bool {
            self.0.split("[capabilities]").nth(1).is_some_and(|table| {
                table
                    .lines()
                    .any(|l| l.replace(' ', "") == format!("{key}=true"))
            })
        }
    }
}

#[tokio::test]
async fn a_removed_key_fails_initialize_by_name() {
    let mut d = Driver::new(FakeCli::default());
    let init = d.init_with(json!({ "agent": "claude" })).await;
    let message = init["error"]["message"].as_str().unwrap();
    assert!(message.contains("`agent` was removed"), "{message}");
}

#[tokio::test]
async fn dispatch_launches_tool_launch_in_the_tasks_worktree_and_submits_the_prompt() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![agent_shown()]);
    cli.on(
        "terminal wait",
        vec![Canned::Ok(
            json!({ "wait": { "condition": "tui-idle", "satisfied": true, "status": "running" } }),
        )],
    );
    cli.on(
        "terminal send",
        vec![Canned::Ok(json!({ "send": { "accepted": true, "prompt": {
            "stages": ["input_accepted", "turn_started"], "observation": "supported"
        }}}))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;

    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert_eq!(disp["result"]["session_id"], HANDLE, "{disp}");

    let create = &cli.calls_to("terminal create")[0];
    assert_eq!(
        flag_value(create, "--worktree"),
        Some("path:/wt/agent-1"),
        "the terminal opens in totsuka's worktree"
    );
    assert_eq!(flag_value(create, "--title"), Some("totsuka T-1"));
    assert_eq!(
        flag_value(create, "--command"),
        Some("exec env 'TOTSUKA_JOB_ID=3.1' 'claude' '--settings' '/cfg/hooks settings.json'"),
    );

    let identity = &cli.calls_to("worktree set")[0];
    assert_eq!(flag_value(identity, "--display-name"), Some("web: Do it"));

    let wait = &cli.calls_to("terminal wait")[0];
    assert_eq!(flag_value(wait, "--for"), Some("tui-idle"));

    let send = &cli.calls_to("terminal send")[0];
    assert_eq!(flag_value(send, "--terminal"), Some(HANDLE));
    assert_eq!(flag_value(send, "--text"), Some("line one\nline two"));
    assert!(send.iter().any(|a| a == "--enter"));
    assert!(flag_value(send, "--wait-submit").is_some());

    // The ownership marker goes on with `rename` — `--title` is only the
    // initial title, which the agent's own replaces — and only after the
    // prompt, because orca shows the agent's identity off the agent's title.
    let rename = &cli.calls_to("terminal rename")[0];
    assert_eq!(flag_value(rename, "--title"), Some("totsuka T-1"));
    let keys = cli.keys();
    let at = |k: &str| keys.iter().position(|x| x == k).unwrap();
    assert!(at("terminal wait") < at("terminal show"), "{keys:?}");
    assert!(at("terminal show") < at("terminal send"), "{keys:?}");
    assert!(at("terminal send") < at("terminal rename"), "{keys:?}");

    // The plugin no longer owns worktrees: nothing is created or removed.
    assert!(!keys.contains(&"worktree create".to_string()), "{keys:?}");
    assert!(!keys.contains(&"worktree rm".to_string()), "{keys:?}");
    // No companion shell by default.
    assert!(!keys.contains(&"terminal split".to_string()), "{keys:?}");
}

#[tokio::test]
async fn layout_shell_splits_a_companion_off_the_agent() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![agent_shown()]);
    let mut d = Driver::new(cli.clone());
    d.init_with(json!({ "layout": { "shell": true, "direction": "vertical" }, "identity": { "enabled": false } }))
        .await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert_eq!(disp["result"]["session_id"], HANDLE, "{disp}");
    let split = &cli.calls_to("terminal split")[0];
    assert_eq!(flag_value(split, "--terminal"), Some(HANDLE));
    assert_eq!(flag_value(split, "--direction"), Some("vertical"));
    assert!(cli.calls_to("worktree set").is_empty(), "identity is off");
}

#[tokio::test]
async fn dispatch_without_tool_launch_is_invalid_params() {
    let cli = FakeCli::default();
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let mut params = dispatch_params(None);
    params.as_object_mut().unwrap().remove("tool_launch");
    let disp = d.call("task/dispatch", params).await;
    assert_eq!(
        disp["error"]["code"],
        plugin_protocol::error_code::INVALID_PARAMS
    );
    assert!(
        cli.keys().is_empty(),
        "nothing is launched: {:?}",
        cli.keys()
    );
}

#[tokio::test]
async fn an_unknown_worktree_points_at_repo_registration() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![Canned::Err("selector_not_found")]);
    let mut d = Driver::new(cli);
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    let message = disp["error"]["message"].as_str().unwrap();
    assert!(message.contains("orca repo add"), "{message}");
}

#[tokio::test]
async fn a_resume_that_kills_the_agent_is_unresumable_and_cleaned_up() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    // `claude --resume <gone>` exits, and `exec` makes that the terminal's
    // exit; the startup wait then sees it.
    cli.on("terminal wait", vec![Canned::Err("terminal_exited")]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d
        .call("task/dispatch", dispatch_params(Some("sid-1")))
        .await;
    assert_eq!(
        disp["error"]["code"],
        plugin_protocol::error_code::SESSION_UNRESUMABLE,
        "{disp}"
    );
    let close = &cli.calls_to("terminal close")[0];
    assert_eq!(flag_value(close, "--terminal"), Some(HANDLE));
    assert!(
        cli.calls_to("terminal send").is_empty(),
        "nothing to prompt"
    );
}

#[tokio::test]
async fn a_fresh_dispatch_whose_agent_dies_keeps_its_own_error() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on(
        "terminal wait",
        vec![Canned::Ok(
            json!({ "wait": { "satisfied": true, "status": "exited", "exitCode": 1 } }),
        )],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert_eq!(
        disp["error"]["code"],
        plugin_protocol::error_code::INTERNAL_ERROR,
        "{disp}"
    );
    assert_eq!(cli.calls_to("terminal close").len(), 1);
}

#[tokio::test]
async fn a_tui_that_never_reports_idle_is_still_prompted() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![agent_shown()]);
    cli.on("terminal wait", vec![Canned::Err("timeout")]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert_eq!(disp["result"]["session_id"], HANDLE, "{disp}");
    assert_eq!(cli.calls_to("terminal send").len(), 1);
}

/// An agent orca has no integration for never gets an identity. The prompt
/// still goes in once the wait runs out — on a paused clock, so the 30s cost
/// nothing here.
#[tokio::test(start_paused = true)]
async fn an_agent_orca_never_recognises_is_prompted_after_the_wait() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![shown(true, WORKTREE)]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert_eq!(disp["result"]["session_id"], HANDLE, "{disp}");
    assert!(
        cli.calls_to("terminal show").len() > 10,
        "it kept asking before giving up"
    );
    assert_eq!(cli.calls_to("terminal send").len(), 1);
}

#[tokio::test]
async fn an_agent_that_exits_before_it_is_recognised_fails_the_dispatch() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![shown(false, WORKTREE)]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d
        .call("task/dispatch", dispatch_params(Some("sid-1")))
        .await;
    assert_eq!(
        disp["error"]["code"],
        plugin_protocol::error_code::SESSION_UNRESUMABLE,
        "{disp}"
    );
    assert!(cli.calls_to("terminal send").is_empty());
    assert_eq!(cli.calls_to("terminal close").len(), 1);
}

#[tokio::test]
async fn the_deadman_reports_failed_when_the_agent_exits() {
    let cli = FakeCli::default();
    cli.on(
        "terminal wait",
        vec![
            // A still-running agent: orca's own timeout, asked again.
            Canned::Err("timeout"),
            Canned::Ok(json!({ "wait": {
                "condition": "exit", "satisfied": true, "status": "exited", "exitCode": 0,
                "exitCause": { "kind": "unknown", "reason": "host_status_unavailable" }
            }})),
        ],
    );
    // `terminal show` confirms the exit (the deadman no longer trusts `wait`
    // alone).
    cli.on("terminal show", vec![shown(false, WORKTREE)]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let ack = d
        .call("state/subscribe", json!({ "session_id": HANDLE }))
        .await;
    assert!(ack["error"].is_null(), "{ack}");
    let note = d.recv().await.expect("a notification");
    assert_eq!(note["method"], "state/notification");
    assert_eq!(note["params"]["state"], "failed");
    assert!(
        note["params"]["log_chunk"]
            .as_str()
            .unwrap()
            .contains("exited"),
        "{note}"
    );
    let waits = cli.calls_to("terminal wait");
    assert_eq!(waits.len(), 2);
    assert_eq!(flag_value(&waits[0], "--for"), Some("exit"));
}

#[tokio::test]
async fn the_deadman_reports_failed_when_the_terminal_is_gone() {
    let cli = FakeCli::default();
    cli.on("terminal wait", vec![Canned::Err("terminal_handle_stale")]);
    cli.on("terminal show", vec![Canned::Err("terminal_handle_stale")]);
    let mut d = Driver::new(cli);
    d.init().await;
    d.call("state/subscribe", json!({ "session_id": HANDLE }))
        .await;
    let note = d.recv().await.expect("a notification");
    assert_eq!(note["params"]["state"], "failed");
}

/// The live failure this guards against (first orca e2e, task 9): `wait`
/// answered "gone" for a terminal whose agent was still working, and the task
/// was failed 5s into a run that went on to finish. A claim of the end is now
/// checked against `terminal show`; a connected terminal means "wait again".
///
/// `start_paused` because the re-wait is paced by `ERROR_BACKOFF`.
#[tokio::test(start_paused = true)]
async fn a_spurious_end_from_wait_is_not_reported_as_failed() {
    let cli = FakeCli::default();
    cli.on(
        "terminal wait",
        vec![
            Canned::Err("terminal_handle_stale"),
            Canned::Ok(json!({ "wait": { "satisfied": true, "status": "exited" } })),
            Canned::Ok(json!({ "wait": { "satisfied": true, "status": "exited", "exitCode": 0 } })),
        ],
    );
    // The first two claims are spurious — the terminal is connected — and
    // only the third is borne out.
    cli.on(
        "terminal show",
        vec![
            shown(true, WORKTREE),
            shown(true, WORKTREE),
            shown(false, WORKTREE),
        ],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    d.call("state/subscribe", json!({ "session_id": HANDLE }))
        .await;
    let note = d.recv().await.expect("a notification");
    assert_eq!(note["params"]["state"], "failed");
    assert_eq!(
        cli.calls_to("terminal wait").len(),
        3,
        "both spurious claims were waited through"
    );
    assert_eq!(cli.calls_to("terminal show").len(), 3);
}

#[tokio::test]
async fn attach_reads_the_terminal_and_the_worktree_status() {
    let cli = FakeCli::default();
    cli.on(
        "terminal show",
        vec![
            shown(true, WORKTREE),
            shown(false, WORKTREE),
            Canned::Err("terminal_handle_stale"),
        ],
    );
    cli.on(
        "worktree ps",
        vec![Canned::Ok(
            json!({ "worktrees": [{ "path": WORKTREE, "status": "waiting" }] }),
        )],
    );
    let mut d = Driver::new(cli);
    d.init().await;

    let live = d
        .call("session/attach", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(live["result"]["attached"], true);
    assert_eq!(live["result"]["state"], "waiting_input");

    for _ in 0..2 {
        let gone = d
            .call("session/attach", json!({ "session_id": HANDLE }))
            .await;
        assert_eq!(gone["result"]["attached"], false, "{gone}");
    }
}

#[tokio::test]
async fn cancel_closes_the_tab_and_is_idempotent() {
    let cli = FakeCli::default();
    cli.on(
        "terminal close",
        vec![
            Canned::Ok(json!({ "close": { "handle": HANDLE } })),
            Canned::Err("terminal_handle_stale"),
        ],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    for _ in 0..2 {
        let r = d.call("task/cancel", json!({ "session_id": HANDLE })).await;
        assert!(r["error"].is_null(), "{r}");
    }
    let close = &cli.calls_to("terminal close")[0];
    assert!(close.iter().any(|a| a == "--tab"), "{close:?}");
    assert!(
        cli.calls_to("worktree rm").is_empty(),
        "the worktree is not ours"
    );
}

#[tokio::test]
async fn release_closes_a_live_matching_terminal() {
    let cli = FakeCli::default();
    cli.on("terminal show", vec![shown(true, WORKTREE)]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let r = d
        .call(
            "session/release",
            json!({ "session_id": HANDLE, "expect_cwd": WORKTREE }),
        )
        .await;
    assert_eq!(r["result"]["released"], true, "{r}");
    assert_eq!(cli.calls_to("terminal close").len(), 1);
}

#[tokio::test]
async fn release_of_a_gone_terminal_is_gone() {
    let cli = FakeCli::default();
    cli.on("terminal show", vec![Canned::Err("terminal_handle_stale")]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let r = d
        .call("session/release", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(r["result"]["released"], false);
    assert_eq!(r["result"]["not_released"], "gone");
    assert!(cli.calls_to("terminal close").is_empty());
}

#[tokio::test]
async fn release_of_an_exited_terminal_tidies_its_tab_and_says_gone() {
    let cli = FakeCli::default();
    cli.on("terminal show", vec![shown(false, WORKTREE)]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let r = d
        .call(
            "session/release",
            json!({ "session_id": HANDLE, "expect_cwd": WORKTREE }),
        )
        .await;
    assert_eq!(r["result"]["released"], false);
    assert_eq!(r["result"]["not_released"], "gone");
    assert_eq!(cli.calls_to("terminal close").len(), 1);
}

#[tokio::test]
async fn release_refuses_a_terminal_in_another_worktree() {
    let cli = FakeCli::default();
    cli.on("terminal show", vec![shown(true, "/somewhere/else")]);
    // The task's own terminal is still alive under another handle.
    cli.on(
        "terminal list",
        vec![Canned::Ok(json!({ "terminals": [
            { "handle": "term_2", "title": "totsuka T-1", "worktreePath": WORKTREE, "connected": true }
        ]}))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let r = d
        .call(
            "session/release",
            json!({ "session_id": HANDLE, "expect_cwd": WORKTREE }),
        )
        .await;
    assert_eq!(r["result"]["released"], false);
    assert_eq!(r["result"]["not_released"], "refused");
    assert!(cli.calls_to("terminal close").is_empty());
}

/// `doctor` releases with `expect_label` and no cwd. A stale handle whose task
/// still has a live terminal under another handle must answer `refused`,
/// found by that label — not `gone`.
#[tokio::test]
async fn a_stale_handle_whose_task_lives_on_is_refused_by_label() {
    let cli = FakeCli::default();
    cli.on("terminal show", vec![Canned::Err("terminal_handle_stale")]);
    cli.on(
        "terminal list",
        vec![Canned::Ok(json!({ "terminals": [
            { "handle": "term_2", "title": "totsuka T-1", "worktreePath": WORKTREE, "connected": true }
        ]}))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let r = d
        .call(
            "session/release",
            json!({ "session_id": HANDLE, "expect_label": "totsuka T-1" }),
        )
        .await;
    assert_eq!(r["result"]["released"], false);
    assert_eq!(r["result"]["not_released"], "refused", "{r}");
    assert!(cli.calls_to("terminal close").is_empty());
}

#[tokio::test]
async fn a_prompt_orca_refuses_fails_the_dispatch_and_closes_the_tab() {
    let cli = FakeCli::default();
    cli.on("terminal create", vec![created()]);
    cli.on("terminal show", vec![agent_shown()]);
    cli.on(
        "terminal send",
        vec![Canned::Ok(json!({ "send": { "accepted": false } }))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert!(
        disp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("accepted"),
        "{disp}"
    );
    assert_eq!(cli.calls_to("terminal close").len(), 1);
    assert!(cli.calls_to("terminal rename").is_empty());
}

/// A `terminal create` that succeeds without a handle may still have started
/// an agent. It is found by its initial title and closed.
#[tokio::test]
async fn a_create_without_a_handle_closes_what_it_may_have_started() {
    let cli = FakeCli::default();
    cli.on(
        "terminal create",
        vec![Canned::Ok(json!({ "terminal": {} }))],
    );
    cli.on(
        "terminal list",
        vec![Canned::Ok(json!({ "terminals": [
            { "handle": "term_x", "title": "totsuka T-1", "worktreePath": WORKTREE },
            { "handle": "term_human", "title": "Terminal 1", "worktreePath": WORKTREE },
        ]}))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d.call("task/dispatch", dispatch_params(None)).await;
    assert!(disp["error"].is_object(), "{disp}");
    let list = &cli.calls_to("terminal list")[0];
    assert_eq!(flag_value(list, "--worktree"), Some("path:/wt/agent-1"));
    let closes = cli.calls_to("terminal close");
    assert_eq!(closes.len(), 1, "only the task's terminal: {closes:?}");
    assert_eq!(flag_value(&closes[0], "--terminal"), Some("term_x"));
}

#[tokio::test]
async fn cancel_of_an_already_exited_terminal_succeeds() {
    let cli = FakeCli::default();
    cli.on("terminal close", vec![Canned::Err("terminal_exited")]);
    let mut d = Driver::new(cli);
    d.init().await;
    let r = d.call("task/cancel", json!({ "session_id": HANDLE })).await;
    assert!(r["error"].is_null(), "{r}");
}

#[tokio::test]
async fn list_returns_only_totsuka_terminals() {
    let cli = FakeCli::default();
    cli.on(
        "terminal list",
        vec![Canned::Ok(json!({ "terminals": [
            { "handle": "term_a", "title": "totsuka T-1", "worktreePath": "/wt/a", "connected": true },
            // A companion shell split off it: no title of its own.
            { "handle": "term_b", "title": null, "worktreePath": "/wt/a", "connected": true },
            { "handle": "term_c", "title": "Terminal 1", "worktreePath": "/repo", "connected": true },
        ]}))],
    );
    let mut d = Driver::new(cli);
    d.init().await;
    let r = d.call("session/list", json!({})).await;
    assert_eq!(
        r["result"]["sessions"],
        json!([{ "session_id": "term_a", "label": "totsuka T-1", "cwd": "/wt/a" }])
    );
}

#[tokio::test]
async fn focus_switches_and_reports_a_gone_terminal_as_unfocused() {
    let cli = FakeCli::default();
    cli.on(
        "terminal switch",
        vec![
            Canned::Ok(json!({ "switch": { "handle": HANDLE } })),
            Canned::Err("terminal_exited"),
        ],
    );
    let mut d = Driver::new(cli);
    d.init().await;
    let first = d
        .call("session/focus", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(first["result"]["focused"], true);
    let second = d
        .call("session/focus", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(second["result"]["focused"], false);
}

#[tokio::test]
async fn snapshot_prefers_the_screen_and_falls_back_to_the_stream() {
    let cli = FakeCli::default();
    cli.on(
        "terminal read",
        vec![
            Canned::Ok(json!({ "terminal": { "source": "screen", "tail": ["❯ hello", "done"] } })),
            Canned::Ok(json!({ "terminal": { "source": "screen-unavailable", "tail": [] } })),
            Canned::Ok(json!({ "terminal": { "source": "stream", "tail": ["from the stream"] } })),
            Canned::Err("terminal_handle_stale"),
        ],
    );
    let mut d = Driver::new(cli);
    d.init().await;
    let screen = d
        .call("diagnostics/snapshot", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(screen["result"]["text"], "❯ hello\ndone");
    let fallback = d
        .call("diagnostics/snapshot", json!({ "session_id": HANDLE }))
        .await;
    assert_eq!(fallback["result"]["text"], "from the stream");
    let gone = d
        .call("diagnostics/snapshot", json!({ "session_id": HANDLE }))
        .await;
    assert!(gone["error"].is_null(), "never an error: {gone}");
    assert!(gone["result"].get("text").is_none(), "{gone}");
}

#[tokio::test]
async fn config_validate_checks_the_runtime_and_names_removed_keys() {
    let cli = FakeCli::default();
    cli.on(
        "status",
        vec![
            Canned::Ok(json!({ "runtime": { "reachable": true } })),
            Canned::Ok(json!({ "runtime": { "reachable": false } })),
        ],
    );
    let mut d = Driver::new(cli);
    let ok = d.call("config/validate", json!({ "config": {} })).await;
    assert_eq!(ok["result"]["valid"], true, "{ok}");
    let down = d.call("config/validate", json!({ "config": {} })).await;
    assert_eq!(down["result"]["valid"], false);
    assert!(
        down["result"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("orca open")
    );
    let removed = d
        .call(
            "config/validate",
            json!({ "config": { "poll_interval_ms": 5 } }),
        )
        .await;
    assert!(
        removed["result"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("`poll_interval_ms` was removed")
    );
}

#[tokio::test]
async fn methods_before_initialize_are_rejected() {
    let mut d = Driver::new(FakeCli::default());
    for method in [
        "task/dispatch",
        "session/attach",
        "session/focus",
        "session/release",
        "session/list",
        "diagnostics/snapshot",
    ] {
        let r = d.call(method, json!({ "session_id": HANDLE })).await;
        assert_eq!(
            r["error"]["code"],
            plugin_protocol::error_code::INVALID_REQUEST,
            "{method}: {r}"
        );
    }
}
