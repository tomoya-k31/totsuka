//! End-to-end plugin flow over a **fake orca CLI** (responses keyed by
//! subcommand): initialize → task/dispatch → state/subscribe → mapped
//! state/notification stream (running → waiting_input(question) → done), plus
//! session/attach success + missing worktree, capability negotiation, and
//! config/validate (F-32/F-33/F-35/F-37/F-38).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use agent_ide_orca::cli::OrcaCli;
use agent_ide_orca::config::OrcaConfig;
use agent_ide_orca::error::OrcaError;
use agent_ide_orca::server::{CliFactory, Server};

/// A scripted response for one `orca <sub> <verb>` invocation.
#[derive(Clone)]
enum Canned {
    Ok(Value),
    /// A CLI failure with this stderr (drives `is_missing` / error paths).
    Fail(String),
}

/// A fake orca CLI: responses keyed by the first two args (e.g. "worktree ps").
/// Each key holds a queue; the last response repeats once the queue drains, so a
/// polling loop keeps seeing the terminal state.
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
    fn calls_to(&self, key: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| cli_key(c) == key)
            .count()
    }
}

/// Key an invocation by its subcommand (+ verb): the first arg, plus the second
/// only when it is a verb rather than a flag (`status --json` → `status`,
/// `worktree ps …` → `worktree ps`).
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
                Canned::Fail(stderr) => Err(OrcaError::CliFailed { code: 1, stderr }),
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
        // A tiny poll interval keeps the state loop fast in tests.
        self.call(
            "initialize",
            json!({ "protocol_version": "0.1.0", "config": { "poll_interval_ms": 5 } }),
        )
        .await
    }
}

#[tokio::test]
async fn dispatch_then_state_stream_to_done() {
    let cli = FakeCli::default();
    cli.on("worktree create", vec![Canned::Ok(json!({ "id": "wt1" }))]);
    // ps returns working, then waiting, then done (last repeats).
    cli.on(
        "worktree ps",
        vec![
            Canned::Ok(
                json!({ "worktrees": [{ "id": "wt1", "state": "working", "terminal": "t1" }] }),
            ),
            Canned::Ok(
                json!({ "worktrees": [{ "id": "wt1", "state": "waiting", "terminal": "t1" }] }),
            ),
            Canned::Ok(
                json!({ "worktrees": [{ "id": "wt1", "state": "done", "terminal": "t1" }] }),
            ),
        ],
    );
    cli.on("terminal wait", vec![Canned::Ok(json!({ "idle": true }))]);
    cli.on(
        "terminal read",
        vec![Canned::Ok(
            json!({ "output": "building...\n\nProceed with deploy? [y/N]" }),
        )],
    );

    let mut d = Driver::new(cli.clone());
    let init = d.init().await;
    // Capability negotiation (F-33): declares plan_mode + state_stream, and
    // NOT design_preview / pane_control (orca can't fulfil them).
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["plan_mode"], true);
    assert_eq!(caps["state_stream"], true);
    assert_eq!(caps["design_preview"], false);
    assert_eq!(caps["pane_control"], false);

    let disp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T-1", "source": "github", "title": "Do it" },
                "worktree_path": "/wt/agent-1",
                "mode": "implement"
            }),
        )
        .await;
    assert_eq!(disp["result"]["session_id"], "wt1");

    let ack = d
        .call("state/subscribe", json!({ "session_id": "wt1" }))
        .await;
    assert!(ack["error"].is_null(), "subscribe failed: {ack}");

    let mut states = Vec::new();
    for _ in 0..3 {
        let note = d.recv().await.expect("a notification");
        assert_eq!(note["method"], "state/notification");
        let params = &note["params"];
        let state = params["state"].as_str().unwrap().to_string();
        if state == "waiting_input" {
            assert_eq!(
                params["log_chunk"], "Proceed with deploy? [y/N]",
                "waiting_input carries the extracted question (F-35)"
            );
        }
        states.push(state);
    }
    assert_eq!(states, vec!["running", "waiting_input", "done"]);
}

#[tokio::test]
async fn state_stream_reports_failed_on_abnormal_state() {
    let cli = FakeCli::default();
    cli.on("worktree create", vec![Canned::Ok(json!({ "id": "wt2" }))]);
    cli.on(
        "worktree ps",
        vec![
            Canned::Ok(json!({ "worktrees": [{ "id": "wt2", "state": "working" }] })),
            Canned::Ok(json!({ "worktrees": [{ "id": "wt2", "state": "crashed" }] })),
        ],
    );

    let mut d = Driver::new(cli.clone());
    d.init().await;
    let disp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T-2", "source": "github", "title": "Fail" },
                "worktree_path": "/wt/agent-2",
                "mode": "implement"
            }),
        )
        .await;
    assert_eq!(disp["result"]["session_id"], "wt2");
    d.call("state/subscribe", json!({ "session_id": "wt2" }))
        .await;

    let running = d.recv().await.unwrap();
    assert_eq!(running["params"]["state"], "running");
    let failed = d.recv().await.unwrap();
    assert_eq!(failed["params"]["state"], "failed");
}

#[tokio::test]
async fn transient_ps_miss_does_not_report_false_done() {
    // A worktree momentarily absent from `ps` must be confirmed via
    // `worktree show` before concluding the run ended — a transient miss while
    // the agent is still working must NOT emit a spurious `done`.
    let cli = FakeCli::default();
    cli.on("worktree create", vec![Canned::Ok(json!({ "id": "wt3" }))]);
    cli.on(
        "worktree ps",
        vec![
            Canned::Ok(json!({ "worktrees": [{ "id": "wt3", "state": "working" }] })),
            // Transient miss: worktree not listed this poll.
            Canned::Ok(json!({ "worktrees": [] })),
            Canned::Ok(json!({ "worktrees": [{ "id": "wt3", "state": "done" }] })),
        ],
    );
    // `worktree show` confirms it is still alive & working during the miss.
    cli.on(
        "worktree show",
        vec![Canned::Ok(json!({ "id": "wt3", "state": "working" }))],
    );

    let mut d = Driver::new(cli.clone());
    d.init().await;
    d.call(
        "task/dispatch",
        json!({
            "task": { "id": "T-3", "source": "github", "title": "Transient" },
            "worktree_path": "/wt/agent-3",
            "mode": "implement"
        }),
    )
    .await;
    d.call("state/subscribe", json!({ "session_id": "wt3" }))
        .await;

    // Exactly two notifications: running then done — no false done in between.
    let running = d.recv().await.unwrap();
    assert_eq!(running["params"]["state"], "running");
    let done = d.recv().await.unwrap();
    assert_eq!(done["params"]["state"], "done");
}

#[tokio::test]
async fn attach_success_and_missing_worktree() {
    let cli = FakeCli::default();
    cli.on(
        "worktree show",
        vec![Canned::Ok(json!({ "id": "wt1", "state": "working" }))],
    );
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let ok = d
        .call("session/attach", json!({ "session_id": "wt1" }))
        .await;
    assert_eq!(ok["result"]["attached"], true);
    assert_eq!(ok["result"]["state"], "running");

    // A missing worktree → attached:false, not an RPC error (F-37).
    cli.on(
        "worktree show",
        vec![Canned::Fail("worktree not found".into())],
    );
    let gone = d
        .call("session/attach", json!({ "session_id": "gone" }))
        .await;
    assert!(
        gone["error"].is_null(),
        "should not be an RPC error: {gone}"
    );
    assert_eq!(gone["result"]["attached"], false);
}

#[tokio::test]
async fn cancel_removes_worktree_and_is_idempotent() {
    let cli = FakeCli::default();
    cli.on("worktree rm", vec![Canned::Ok(json!({ "removed": true }))]);
    let mut d = Driver::new(cli.clone());
    d.init().await;
    let resp = d.call("task/cancel", json!({ "session_id": "wt1" })).await;
    assert!(resp["error"].is_null(), "cancel failed: {resp}");
    assert_eq!(cli.calls_to("worktree rm"), 1);

    // An already-removed worktree is still success (idempotent).
    cli.on("worktree rm", vec![Canned::Fail("no such worktree".into())]);
    let again = d.call("task/cancel", json!({ "session_id": "wt1" })).await;
    assert!(
        again["error"].is_null(),
        "idempotent cancel failed: {again}"
    );
}

#[tokio::test]
async fn config_validate_pings_orca_status() {
    let cli = FakeCli::default();
    cli.on("status", vec![Canned::Ok(json!({ "running": true }))]);
    let mut d = Driver::new(cli.clone());
    let ok = d.call("config/validate", json!({ "config": {} })).await;
    assert_eq!(ok["result"]["valid"], true);
    assert_eq!(cli.calls_to("status"), 1);

    // orca not running → invalid with guidance.
    cli.on("status", vec![Canned::Fail("connection refused".into())]);
    let bad = d.call("config/validate", json!({ "config": {} })).await;
    assert_eq!(bad["result"]["valid"], false);
    assert!(!bad["result"]["errors"].as_array().unwrap().is_empty());
}

#[test]
fn shipped_manifest_declares_only_supported_capabilities() {
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "orca");
    assert_eq!(manifest.kind, plugin_protocol::PluginKind::AgentIde);
    assert!(manifest.capabilities.plan_mode);
    assert!(manifest.capabilities.state_stream);
    // orca cannot fulfil these, so they must not be advertised (F-33): the
    // Orchestrator only requests declared capabilities, so a workflow needing
    // e.g. a design preview simply won't route it here.
    assert!(!manifest.capabilities.design_preview);
    assert!(!manifest.capabilities.pane_control);
    assert!(manifest.is_compatible_with(&plugin_protocol::protocol_version()));
}

#[tokio::test]
async fn methods_before_initialize_are_rejected() {
    let mut d = Driver::new(FakeCli::default());
    let resp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T", "source": "s", "title": "t" },
                "worktree_path": "/wt", "mode": "implement"
            }),
        )
        .await;
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("initialize")
    );
}
