//! End-to-end plugin flow over a **real Unix-socket fake herdr** that mimics
//! the 0.7.4 protocol (#124): NDJSON with **one request per connection** (the
//! server closes after every response), `events.subscribe` as the only
//! persistent connection (pushing `{event, data}` envelopes), and the
//! `agent.start`-based dispatch. Covers initialize → task/dispatch →
//! state/subscribe → mapped state/notification stream (running →
//! waiting_input(question) → running → done(final output)), exit-before-done
//! as `failed`, session/attach success and pane-not-found, and the `id: ""`
//! error correlation (F-32/F-35/F-37/F-38).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use agent_ide_herdr::error::HerdrError;
use agent_ide_herdr::server::{Server, TransportFactory};
use agent_ide_herdr::transport::SocketTransport;

/// How a fake herdr answers `pane.get` (drives attach + idle confirmation).
#[derive(Clone)]
enum PaneGet {
    /// Return a pane record (nested under `pane`) with this `agent_status`.
    Status(&'static str),
    /// Return a `pane_not_found` error (the pane is gone).
    NotFound,
}

/// Scripted fake-herdr behaviour for one test.
#[derive(Clone)]
struct FakeHerdr {
    /// Envelope events pushed on the subscription connection after its ACK.
    events_on_subscribe: Vec<Value>,
    /// How `pane.get` responds.
    pane_get: PaneGet,
    /// `pane.read` text per source (`visible` / `recent`).
    read_visible: &'static str,
    read_recent: &'static str,
    /// When set, `workspace.create` fails with an `id: ""` decode-style error
    /// (herdr does not echo the id on invalid requests).
    empty_id_error_on_create: bool,
}

impl Default for FakeHerdr {
    fn default() -> Self {
        Self {
            events_on_subscribe: vec![],
            pane_get: PaneGet::Status("idle"),
            read_visible: "",
            read_recent: "",
            empty_id_error_on_create: false,
        }
    }
}

impl FakeHerdr {
    /// Bind a fresh socket and serve **one connection per request** (matching
    /// the real herdr connection model). Returns the socket path and a log of
    /// every request received.
    fn spawn(self) -> (PathBuf, Arc<Mutex<Vec<Value>>>) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-test-{}-{}.sock",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake herdr socket");
        let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let fake = self.clone();
                let log = log.clone();
                tokio::spawn(async move { fake.serve(stream, log).await });
            }
        });
        (path, requests)
    }

    /// Serve a single connection: read one request, respond, close — except
    /// `events.subscribe`, which stays open pushing the scripted envelopes.
    async fn serve(&self, stream: UnixStream, log: Arc<Mutex<Vec<Value>>>) {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();
        let Ok(Some(line)) = lines.next_line().await else {
            return;
        };
        let req: Value = serde_json::from_str(&line).expect("fake herdr got valid JSON");
        log.lock().unwrap().push(req.clone());
        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("");
        match method {
            "ping" => reply(&mut write_half, &id, json!({ "type": "pong" })).await,
            "workspace.create" => {
                if self.empty_id_error_on_create {
                    reply_error(
                        &mut write_half,
                        &json!(""),
                        "invalid_request",
                        "invalid request: missing field `cwd`",
                    )
                    .await;
                } else {
                    reply(
                        &mut write_half,
                        &id,
                        json!({ "type": "workspace_created", "workspace": { "workspace_id": "w1" } }),
                    )
                    .await;
                }
            }
            "agent.start" => {
                reply(
                    &mut write_half,
                    &id,
                    json!({
                        "type": "agent_started",
                        "agent": { "pane_id": "w1:p1", "terminal_id": "t1", "agent_status": "unknown" },
                    }),
                )
                .await
            }
            "agent.send" | "pane.send_keys" | "pane.close" => {
                reply(&mut write_half, &id, json!({ "type": "ok" })).await
            }
            "pane.get" => match &self.pane_get {
                PaneGet::Status(status) => {
                    reply(
                        &mut write_half,
                        &id,
                        json!({ "type": "pane_info", "pane": { "pane_id": "w1:p1", "agent_status": status } }),
                    )
                    .await
                }
                PaneGet::NotFound => {
                    reply_error(&mut write_half, &id, "pane_not_found", "pane not found").await
                }
            },
            "pane.read" => {
                let text = match req["params"]["source"].as_str() {
                    Some("visible") => self.read_visible,
                    _ => self.read_recent,
                };
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "pane_read", "read": { "pane_id": "w1:p1", "text": text } }),
                )
                .await
            }
            "events.subscribe" => {
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "subscription_started" }),
                )
                .await;
                for ev in &self.events_on_subscribe {
                    write_line(&mut write_half, ev).await;
                }
                // Keep the subscription connection open like the real herdr;
                // it closes when the test (and its transport) is dropped.
                let mut sink = lines;
                while let Ok(Some(_)) = sink.next_line().await {}
            }
            other => reply_error(&mut write_half, &id, "method_not_found", other).await,
        }
        // Match the real herdr: the connection closes after the response
        // (write_half drops here).
    }
}

/// An envelope event as herdr 0.7.x pushes them.
fn status_event(pane_id: &str, status: &str) -> Value {
    json!({
        "event": "pane_agent_status_changed",
        "data": { "pane_id": pane_id, "workspace_id": "w1", "agent_status": status },
    })
}

fn exited_event(pane_id: &str) -> Value {
    // NB: no exit_code — herdr 0.7.x does not carry one.
    json!({
        "event": "pane_exited",
        "data": { "pane_id": pane_id, "workspace_id": "w1", "type": "pane_exited" },
    })
}

async fn reply(w: &mut tokio::net::unix::OwnedWriteHalf, id: &Value, result: Value) {
    write_line(w, &json!({ "id": id, "result": result })).await;
}

async fn reply_error(w: &mut tokio::net::unix::OwnedWriteHalf, id: &Value, code: &str, msg: &str) {
    write_line(
        w,
        &json!({ "id": id, "error": { "code": code, "message": msg } }),
    )
    .await;
}

async fn write_line(w: &mut tokio::net::unix::OwnedWriteHalf, value: &Value) {
    let mut line = serde_json::to_string(value).unwrap();
    line.push('\n');
    w.write_all(line.as_bytes())
        .await
        .expect("fake herdr write");
    w.flush().await.expect("fake herdr flush");
}

/// A factory that connects the real [`SocketTransport`] to the fake socket.
struct SocketFactory;

impl TransportFactory for SocketFactory {
    type Transport = SocketTransport;
    async fn build(&self, path: &Path, timeout: Duration) -> Result<SocketTransport, HerdrError> {
        SocketTransport::connect(path, timeout).await
    }
}

/// A driver around a `Server` writing to an in-memory line channel.
struct Driver {
    server: Server<SocketFactory>,
    out: mpsc::UnboundedReceiver<String>,
    next_id: i64,
}

impl Driver {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            server: Server::new(SocketFactory, tx),
            out: rx,
            next_id: 0,
        }
    }

    /// Send a request and return the correlated response (the next output line;
    /// requests here never race notifications because we drive them serially).
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        assert!(self.server.handle_line(&line.to_string()).await);
        let resp = self.recv().await.expect("a response line");
        assert_eq!(resp["id"], id, "response id must match request");
        resp
    }

    /// Receive the next output line (response or notification), parsed, with a
    /// timeout so a missing notification fails fast instead of hanging. The
    /// timeout absorbs the stream's idle-confirmation delay.
    async fn recv(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_secs(10), self.out.recv())
            .await
            .expect("timed out waiting for plugin output")?;
        Some(serde_json::from_str(&line).expect("valid JSON line"))
    }

    async fn init(&mut self, socket: &Path) -> Value {
        self.call(
            "initialize",
            json!({
                "protocol_version": "0.1.0",
                "config": { "socket_path": socket.to_str().unwrap() }
            }),
        )
        .await
    }
}

#[tokio::test]
async fn dispatch_then_state_stream_to_done() {
    // herdr pushes: a replayed event for ANOTHER pane (must be filtered), then
    // working → blocked → working → idle. The plugin maps these to running →
    // waiting_input(question) → running, and finalizes the confirmed idle as
    // done carrying the final pane output.
    let (socket, requests) = FakeHerdr {
        events_on_subscribe: vec![
            exited_event("w9:p9"), // replayed history for another pane
            status_event("w1:p1", "working"),
            status_event("w1:p1", "blocked"),
            status_event("w1:p1", "working"),
            status_event("w1:p1", "idle"),
        ],
        pane_get: PaneGet::Status("idle"), // the idle re-check confirms
        read_visible: "working on it...\n\nShould I proceed with the migration? (y/n)",
        read_recent: "Here is the drafted reply:\nzsh is managed via GNU Stow.",
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    let init = d.init(&socket).await;
    assert_eq!(init["result"]["capabilities"]["state_stream"], true);
    assert_eq!(init["result"]["capabilities"]["plan_mode"], true);

    // dispatch → session id encodes the pane handle.
    let disp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T1", "source": "slack", "title": "Draft the reply" },
                "worktree_path": "/wt/agent-1",
                "mode": "plan"
            }),
        )
        .await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    assert_eq!(session_id, "w1:p1|");

    // The dispatch went through workspace.create + agent.start with the prompt
    // riding in argv (plan mode adds the permission-mode flag).
    {
        let log = requests.lock().unwrap();
        let create = log
            .iter()
            .find(|r| r["method"] == "workspace.create")
            .expect("a workspace.create request");
        assert_eq!(create["params"]["cwd"], "/wt/agent-1");
        let start = log
            .iter()
            .find(|r| r["method"] == "agent.start")
            .expect("an agent.start request");
        assert_eq!(start["params"]["workspace_id"], "w1");
        let argv: Vec<&str> = start["params"]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(argv[0], "claude");
        assert!(
            argv.contains(&"--permission-mode") && argv.contains(&"plan"),
            "plan mode must add the permission flag: {argv:?}"
        );
        assert!(
            argv.last().unwrap().contains("Draft the reply"),
            "the prompt rides in the trailing argv element: {argv:?}"
        );
    }

    // subscribe → ACK, then the mapped notification stream.
    let ack = d
        .call("state/subscribe", json!({ "session_id": session_id }))
        .await;
    assert!(ack["error"].is_null(), "subscribe failed: {ack}");

    let notes = collect_notes(&mut d, 4).await;
    let states: Vec<&str> = notes
        .iter()
        .map(|n| n["params"]["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        vec!["running", "waiting_input", "running", "done"],
        "working/blocked/working/idle maps to the normalized transitions"
    );
    // F-35: the blocked question is carried best-effort from the visible text.
    assert_eq!(
        notes[1]["params"]["log_chunk"], "Should I proceed with the migration? (y/n)",
        "waiting_input must carry the extracted question"
    );
    // #124: the terminal done carries the final pane output — the only channel
    // the reply text reaches the Orchestrator's `output = source` artifact.
    assert_eq!(
        notes[3]["params"]["log_chunk"], "Here is the drafted reply:\nzsh is managed via GNU Stow.",
        "done must carry the final output"
    );
}

/// Collect `n` state/notification lines from the stream.
async fn collect_notes(d: &mut Driver, n: usize) -> Vec<Value> {
    let mut notes = Vec::new();
    for _ in 0..n {
        let note = d.recv().await.expect("a notification");
        assert_eq!(note["method"], "state/notification");
        notes.push(note);
    }
    notes
}

#[tokio::test]
async fn state_stream_reports_failed_on_exit_before_completion() {
    // herdr 0.7.x pane_exited carries no exit code: an exit that arrives
    // before a confirmed completion is the `failed` source.
    let (socket, _) = FakeHerdr {
        events_on_subscribe: vec![status_event("w1:p1", "working"), exited_event("w1:p1")],
        pane_get: PaneGet::Status("working"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T2", "source": "notion", "title": "Failing task" },
                "worktree_path": "/wt/agent-2",
                "mode": "implement"
            }),
        )
        .await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    let running = d.recv().await.expect("running notification");
    assert_eq!(running["params"]["state"], "running");
    let failed = d.recv().await.expect("failed notification");
    assert_eq!(failed["params"]["state"], "failed");
}

#[tokio::test]
async fn attach_reports_live_pane_state() {
    let (socket, _) = FakeHerdr {
        pane_get: PaneGet::Status("working"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("session/attach", json!({ "session_id": "w1:p1|agent-xyz" }))
        .await;
    assert_eq!(resp["result"]["attached"], true);
    assert_eq!(resp["result"]["state"], "running");
}

#[tokio::test]
async fn attach_reports_not_attached_when_pane_gone() {
    let (socket, _) = FakeHerdr {
        pane_get: PaneGet::NotFound,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("session/attach", json!({ "session_id": "w9:p9|gone" }))
        .await;
    // A vanished pane is `attached: false`, not an RPC error (F-37).
    assert!(
        resp["error"].is_null(),
        "should not be an RPC error: {resp}"
    );
    assert_eq!(resp["result"]["attached"], false);
}

#[tokio::test]
async fn empty_id_error_is_surfaced_to_the_caller() {
    // herdr answers decode failures with `id: ""`; with one request per
    // connection the error still correlates to the in-flight call instead of
    // hanging it until timeout.
    let (socket, _) = FakeHerdr {
        empty_id_error_on_create: true,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "T3", "source": "slack", "title": "t" },
                "worktree_path": "/wt", "mode": "plan"
            }),
        )
        .await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("invalid_request") || message.contains("missing field"),
        "the empty-id error must surface promptly: {resp}"
    );
}

#[tokio::test]
async fn config_validate_pings_herdr() {
    let (socket, _) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    let resp = d
        .call(
            "config/validate",
            json!({ "config": { "socket_path": socket.to_str().unwrap() } }),
        )
        .await;
    assert_eq!(resp["result"]["valid"], true, "ping should succeed: {resp}");
}

#[tokio::test]
async fn config_validate_reports_unreachable_socket() {
    let mut d = Driver::new();
    let resp = d
        .call(
            "config/validate",
            json!({ "config": { "socket_path": "/nonexistent/herdr.sock" } }),
        )
        .await;
    assert_eq!(resp["result"]["valid"], false);
    let errors = resp["result"]["errors"].as_array().unwrap();
    assert!(!errors.is_empty(), "an unreachable socket must be reported");
}

#[test]
fn shipped_manifest_is_valid_agent_ide() {
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "herdr");
    assert_eq!(manifest.kind, plugin_protocol::PluginKind::AgentIde);
    assert!(manifest.capabilities.plan_mode);
    assert!(manifest.capabilities.state_stream);
    assert!(manifest.capabilities.design_preview);
    assert!(manifest.capabilities.pane_control);
    assert!(manifest.is_compatible_with(&plugin_protocol::protocol_version()));
}

#[tokio::test]
async fn methods_before_initialize_are_rejected() {
    let mut d = Driver::new();
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
