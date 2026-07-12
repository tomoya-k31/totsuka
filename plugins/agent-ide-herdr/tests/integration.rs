//! End-to-end plugin flow over a **real Unix-socket fake herdr** (NDJSON, `id`
//! correlation): initialize → task/dispatch → state/subscribe → mapped
//! state/notification stream (running → waiting_input(question) → done), plus
//! session/attach success and pane-not-found (F-32/F-35/F-37/F-38).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use agent_ide_herdr::error::HerdrError;
use agent_ide_herdr::server::{Server, TransportFactory};
use agent_ide_herdr::transport::SocketTransport;

/// How a fake herdr answers `pane.get` (drives attach + question extraction).
#[derive(Clone)]
enum PaneGet {
    /// Return a pane record with this JSON (agent_status / scrollback).
    Ok(Value),
    /// Return a `not_found` error (the pane is gone).
    NotFound,
}

/// Scripted fake-herdr behaviour for one test.
#[derive(Clone)]
struct FakeHerdr {
    /// Events pushed on the connection right after an `events.subscribe` ACK.
    events_on_subscribe: Vec<Value>,
    /// How `pane.get` responds.
    pane_get: PaneGet,
}

impl FakeHerdr {
    /// Bind a fresh socket and serve one connection with this script. Returns the
    /// socket path (unlinked on drop is the caller's concern — temp dir cleanup).
    fn spawn(self) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-test-{}-{}.sock",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake herdr socket");
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                self.serve(stream).await;
            }
        });
        path
    }

    async fn serve(&self, stream: UnixStream) {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let req: Value = serde_json::from_str(&line).expect("fake herdr got valid JSON");
            let id = req["id"].clone();
            let method = req["method"].as_str().unwrap_or("");
            match method {
                "ping" => reply(&mut write_half, &id, json!({ "type": "pong" })).await,
                "workspace.create" => {
                    reply(&mut write_half, &id, json!({ "pane_id": "w1:p1" })).await
                }
                "agent.send" | "session.snapshot" | "pane.send_keys" | "pane.close" => {
                    reply(&mut write_half, &id, json!({})).await
                }
                "pane.get" => match &self.pane_get {
                    PaneGet::Ok(v) => reply(&mut write_half, &id, v.clone()).await,
                    PaneGet::NotFound => {
                        reply_error(&mut write_half, &id, "not_found", "pane not found").await
                    }
                },
                "events.subscribe" => {
                    reply(&mut write_half, &id, json!({ "type": "ack" })).await;
                    for ev in &self.events_on_subscribe {
                        write_line(&mut write_half, ev).await;
                    }
                }
                other => reply_error(&mut write_half, &id, "method_not_found", other).await,
            }
        }
    }
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
    /// timeout so a missing notification fails fast instead of hanging.
    async fn recv(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_secs(5), self.out.recv())
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
    // herdr pushes: working → blocked → exited(0). The plugin maps these to
    // running → waiting_input(with question) → done.
    let socket = FakeHerdr {
        events_on_subscribe: vec![
            json!({ "type": "pane.agent_status_changed", "pane_id": "w1:p1", "agent_status": "working" }),
            json!({ "type": "pane.agent_status_changed", "pane_id": "w1:p1", "agent_status": "blocked" }),
            json!({ "type": "pane.exited", "pane_id": "w1:p1", "exit_code": 0 }),
        ],
        pane_get: PaneGet::Ok(json!({
            "agent_status": "blocked",
            "scrollback": "working on it...\n\nShould I proceed with the migration? (y/n)"
        })),
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
                "task": { "id": "T1", "source": "notion", "title": "Do the thing" },
                "worktree_path": "/wt/agent-1",
                "mode": "implement"
            }),
        )
        .await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    assert_eq!(session_id, "w1:p1|");

    // subscribe → ACK, then the mapped notification stream.
    let ack = d
        .call("state/subscribe", json!({ "session_id": session_id }))
        .await;
    assert!(ack["error"].is_null(), "subscribe failed: {ack}");

    let states = collect_states(&mut d, 3).await;
    assert_eq!(
        states,
        vec!["running", "waiting_input", "done"],
        "herdr working/blocked/exited maps to the normalized transitions"
    );
}

/// Collect `n` state/notification payloads from the stream.
async fn collect_states(d: &mut Driver, n: usize) -> Vec<String> {
    let mut states = Vec::new();
    let mut question_seen = false;
    for _ in 0..n {
        let note = d.recv().await.expect("a notification");
        assert_eq!(note["method"], "state/notification");
        let params = &note["params"];
        let state = params["state"].as_str().unwrap().to_string();
        if state == "waiting_input" {
            // F-35: the blocked question is carried best-effort from scrollback.
            assert_eq!(
                params["log_chunk"], "Should I proceed with the migration? (y/n)",
                "waiting_input must carry the extracted question"
            );
            question_seen = true;
        }
        states.push(state);
    }
    assert!(
        question_seen,
        "the waiting_input notification carried a question"
    );
    states
}

#[tokio::test]
async fn attach_reports_live_pane_state() {
    let socket = FakeHerdr {
        events_on_subscribe: vec![],
        pane_get: PaneGet::Ok(json!({ "agent_status": "working" })),
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
    let socket = FakeHerdr {
        events_on_subscribe: vec![],
        pane_get: PaneGet::NotFound,
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
async fn config_validate_pings_herdr() {
    let socket = FakeHerdr {
        events_on_subscribe: vec![],
        pane_get: PaneGet::Ok(json!({})),
    }
    .spawn();

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
