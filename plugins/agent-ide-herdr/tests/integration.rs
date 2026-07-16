//! End-to-end plugin flow over a **real Unix-socket fake herdr** modelled on
//! herdr 0.7.4 as verified live in #124: NDJSON with **one request per
//! connection** (the server closes after every response), `events.subscribe` as
//! the only persistent connection (pushing `{event, data}` envelopes), the
//! `agent.start` dispatch, and — crucially — a CLI that **accepts keystrokes
//! before it acts on them**, so early `agent.send`/Enter are dropped.
//!
//! Covers initialize → task/dispatch (typing + submitting the prompt through
//! that startup race) → state/subscribe → mapped state/notification stream
//! (running → waiting_input(question) → running → done(final answer)),
//! exit-before-done as `failed`, session/attach success and pane-not-found, and
//! `id: ""` error correlation (F-32/F-35/F-37/F-38).

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

const PANE: &str = "w1:p1";

/// The fake agent CLI's observable state — the pane's screen and status.
#[derive(Default)]
struct Cli {
    /// What the input box shows (what `agent.send` typed in).
    input: String,
    /// herdr's `agent_status` for the pane.
    status: String,
    sends: usize,
    enters: usize,
    /// `pane.get` calls so far. A subscription waits for the next one before
    /// pushing, so events land *after* the subscriber seeded its state — as on
    /// a real run, where an agent's transitions are spread over its work.
    pane_gets: usize,
}

/// Scripted fake-herdr behaviour for one test.
#[derive(Clone)]
struct FakeHerdr {
    cli: Arc<Mutex<Cli>>,
    /// Envelope events pushed on the subscription connection after its ACK.
    events_on_subscribe: Arc<Mutex<Vec<Value>>>,
    /// Startup race: this many `agent.send`/Enter presses are dropped before
    /// the CLI starts acting on them (0 = a CLI that is ready immediately).
    deaf_sends: usize,
    deaf_enters: usize,
    /// `pane.get` reports a vanished pane.
    pane_gone: bool,
    /// The pane's `agent_session` (drives transcript lookup), if reported.
    agent_session: Option<Value>,
    /// `pane.read` text for the `detection` source.
    detection: &'static str,
    /// When set, `workspace.create` fails with an `id: ""` decode-style error.
    empty_id_error_on_create: bool,
}

impl Default for FakeHerdr {
    fn default() -> Self {
        Self {
            cli: Arc::new(Mutex::new(Cli {
                status: "idle".to_string(),
                ..Cli::default()
            })),
            events_on_subscribe: Arc::default(),
            deaf_sends: 0,
            deaf_enters: 0,
            pane_gone: false,
            agent_session: None,
            detection: "",
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
        let params = &req["params"];
        match req["method"].as_str().unwrap_or("") {
            "ping" => reply(&mut write_half, &id, json!({ "type": "pong" })).await,

            "workspace.create" if self.empty_id_error_on_create => {
                // herdr does not echo the id when it cannot decode a request.
                reply_error(
                    &mut write_half,
                    &json!(""),
                    "invalid_request",
                    "invalid request: missing field `cwd`",
                )
                .await;
            }
            "workspace.create" => {
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "workspace_created", "workspace": { "workspace_id": "w1" } }),
                )
                .await
            }
            "agent.start" => {
                reply(
                    &mut write_half,
                    &id,
                    json!({
                        "type": "agent_started",
                        "agent": { "pane_id": PANE, "terminal_id": "t1", "agent_status": "idle" },
                    }),
                )
                .await
            }

            // The CLI ignores input until it is ready; once ready, typed text
            // lands in the input box without being submitted. Text **appends**
            // like real keystrokes do — a send that overwrote would hide a
            // double-typed prompt.
            "agent.send" => {
                {
                    let mut cli = self.cli.lock().unwrap();
                    cli.sends += 1;
                    if cli.sends > self.deaf_sends {
                        cli.input
                            .push_str(params["text"].as_str().unwrap_or_default());
                    }
                }
                reply(&mut write_half, &id, json!({ "type": "ok" })).await
            }
            // Enter submits whatever is in the box — once the CLI is listening.
            // On an empty box it is a no-op, exactly like the real TUI.
            "pane.send_keys" => {
                let enter = params["keys"]
                    .as_array()
                    .is_some_and(|keys| keys.iter().any(|k| k == "enter"));
                if enter {
                    let mut cli = self.cli.lock().unwrap();
                    cli.enters += 1;
                    if cli.enters > self.deaf_enters && !cli.input.is_empty() {
                        cli.status = "working".to_string();
                    }
                    drop(cli);
                }
                reply(&mut write_half, &id, json!({ "type": "ok" })).await
            }
            "pane.close" | "workspace.close" => {
                reply(&mut write_half, &id, json!({ "type": "ok" })).await
            }

            "pane.get" if self.pane_gone => {
                reply_error(&mut write_half, &id, "pane_not_found", "pane not found").await
            }
            "pane.get" => {
                let status = {
                    let mut cli = self.cli.lock().unwrap();
                    cli.pane_gets += 1;
                    cli.status.clone()
                };
                let mut pane = json!({
                    "pane_id": PANE,
                    "cwd": "/wt/agent-1",
                    "agent_status": status,
                });
                if let Some(session) = &self.agent_session {
                    pane["agent_session"] = session.clone();
                }
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "pane_info", "pane": pane }),
                )
                .await
            }
            "pane.read" => {
                let text = match params["source"].as_str() {
                    // The input box is on the visible screen.
                    Some("visible") => self.cli.lock().unwrap().input.clone(),
                    _ => self.detection.to_string(),
                };
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "pane_read", "read": { "pane_id": PANE, "text": text } }),
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
                let events = self.events_on_subscribe.lock().unwrap().clone();
                if !events.is_empty() {
                    // Let the subscriber seed its state from the pane first;
                    // pushing everything before it looked would compress the
                    // whole run into "already finished".
                    let seeded_at = { self.cli.lock().unwrap().pane_gets };
                    while { self.cli.lock().unwrap().pane_gets } == seeded_at {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                for ev in events {
                    // An event herdr pushes also moves the pane's own status,
                    // which the plugin re-reads to confirm a completion.
                    if let Some(status) = ev["data"]["agent_status"].as_str() {
                        self.cli.lock().unwrap().status = status.to_string();
                    }
                    write_line(&mut write_half, &ev).await;
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

/// An envelope event as herdr 0.7.x pushes them. NB the kind is **dotted** here
/// while [`exited_event`]'s is underscored — herdr really is inconsistent, and
/// matching only one form silently drops every status change (#124).
fn status_event(pane_id: &str, status: &str) -> Value {
    json!({
        "event": "pane.agent_status_changed",
        "data": { "pane_id": pane_id, "workspace_id": "w1", "agent": "claude", "agent_status": status },
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
        let line = tokio::time::timeout(Duration::from_secs(15), self.out.recv())
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

    async fn dispatch(&mut self, id: &str, title: &str, mode: &str) -> Value {
        self.call(
            "task/dispatch",
            json!({
                "task": { "id": id, "source": "slack", "title": title,
                          "body": "Answer in the thread.\n\nContext:\n- multi-line, like every Slack task body" },
                "worktree_path": "/wt/agent-1",
                "mode": mode
            }),
        )
        .await
    }
}

/// The `detection` view herdr renders: chrome-free, `⏺` per agent turn.
const DETECTION: &str = "\n ▐▛███▜▌   Claude Code\n\n\
     ❯ Draft the reply\n\n\
     ⏺ Read(README.md)\n  ⎿ read 40 lines\n\n\
     ⏺ zsh is managed via GNU Stow.\n  Edit the repo, not the symlink.\n\n\
     ✻ Cooked for 4s\n";

#[tokio::test]
async fn dispatch_types_and_submits_the_prompt_through_the_startup_race() {
    // The CLI ignores the first send and the first two Enters — the real
    // startup race that left panes idle forever with the prompt unsent (#124).
    let fake = FakeHerdr {
        deaf_sends: 1,
        deaf_enters: 2,
        ..FakeHerdr::default()
    };
    let cli = fake.cli.clone();
    let (socket, requests) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T1", "Draft the reply", "plan").await;

    assert!(disp["error"].is_null(), "dispatch must recover: {disp}");
    assert_eq!(disp["result"]["session_id"], "w1:p1|");
    {
        let cli = cli.lock().unwrap();
        assert_eq!(cli.status, "working", "the agent must actually be started");
        assert!(
            cli.input.contains("Draft the reply") && cli.input.contains("multi-line"),
            "the whole multi-line prompt is typed in, not passed as argv"
        );
        assert_eq!(
            cli.input.matches("Draft the reply").count(),
            1,
            "the retries must not type the prompt in twice: {:?}",
            cli.input
        );
        assert!(
            cli.sends >= 2 && cli.enters >= 3,
            "retries must have happened"
        );
    }
    // The prompt is never in argv: a multi-line argv prompt is never submitted.
    let log = requests.lock().unwrap();
    let start = log
        .iter()
        .find(|r| r["method"] == "agent.start")
        .expect("an agent.start request");
    let argv: Vec<&str> = start["params"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(argv, vec!["claude", "--permission-mode", "plan"]);
}

#[tokio::test]
async fn dispatch_fails_loudly_when_the_agent_never_starts() {
    // A CLI that never listens must surface an error, not a session id whose
    // state stream would hang forever.
    let (socket, requests) = FakeHerdr {
        deaf_enters: usize::MAX,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T2", "Never starts", "plan").await;
    assert!(
        disp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("never started"),
        "expected a loud failure: {disp}"
    );

    // …and it must take its workspace down with it: a failed dispatch reports
    // no session id, so nothing else could ever close the pane or its CLI.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["method"] == "workspace.close")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "a failed dispatch must not leak its workspace"
    );
}

#[tokio::test]
async fn state_stream_maps_transitions_and_carries_the_answer() {
    let fake = FakeHerdr {
        detection: DETECTION,
        ..FakeHerdr::default()
    };
    *fake.events_on_subscribe.lock().unwrap() = vec![
        exited_event("w9:p9"), // replayed history for another pane: ignored
        status_event(PANE, "blocked"),
        status_event(PANE, "working"),
        status_event(PANE, "idle"), // completion for a `done`-less agent
    ];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    let init = d.init(&socket).await;
    assert_eq!(init["result"]["capabilities"]["state_stream"], true);
    assert_eq!(init["result"]["capabilities"]["plan_mode"], true);

    let disp = d.dispatch("T3", "Draft the reply", "plan").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    let ack = d
        .call("state/subscribe", json!({ "session_id": session_id }))
        .await;
    assert!(ack["error"].is_null(), "subscribe failed: {ack}");

    let notes = collect_notes(&mut d, 3).await;
    let states: Vec<&str> = notes
        .iter()
        .map(|n| n["params"]["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["waiting_input", "running", "done"]);
    // F-35: the question is carried best-effort from the visible screen.
    assert!(
        notes[0]["params"]["log_chunk"]
            .as_str()
            .unwrap_or_default()
            .contains("multi-line"),
        "waiting_input must carry the screen question: {}",
        notes[0]
    );
    // #124: `done` carries the answer — with no transcript reachable, the
    // detection view is the fallback, and tool turns are not the answer.
    assert_eq!(
        notes[2]["params"]["log_chunk"],
        "zsh is managed via GNU Stow.\nEdit the repo, not the symlink.",
    );
}

#[tokio::test]
async fn state_stream_reports_done_when_the_answer_landed_before_subscribing() {
    // A fast agent finishes between dispatch and state/subscribe; seeding from
    // the pane keeps that from hanging the stream forever.
    let fake = FakeHerdr {
        detection: DETECTION,
        ..FakeHerdr::default()
    };
    let cli = fake.cli.clone();
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T4", "Fast answer", "plan").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    cli.lock().unwrap().status = "done".to_string(); // finished, no events left to push

    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;
    let note = d.recv().await.expect("a notification");
    assert_eq!(note["params"]["state"], "done");
    assert_eq!(
        note["params"]["log_chunk"],
        "zsh is managed via GNU Stow.\nEdit the repo, not the symlink."
    );
}

#[tokio::test]
async fn state_stream_reports_failed_on_exit_before_completion() {
    // herdr 0.7.x pane_exited carries no exit code: an exit that arrives before
    // a completion is the `failed` source.
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![exited_event(PANE)];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T5", "Failing task", "implement").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    let failed = d.recv().await.expect("failed notification");
    assert_eq!(failed["params"]["state"], "failed");
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
async fn attach_reports_live_pane_state() {
    let fake = FakeHerdr::default();
    fake.cli.lock().unwrap().status = "working".to_string();
    let (socket, _) = fake.spawn();

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
        pane_gone: true,
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
async fn dispatch_carries_the_agent_session_for_resume_and_transcripts() {
    let (socket, _) = FakeHerdr {
        agent_session: Some(
            json!({ "source": "herdr:claude", "agent": "claude", "kind": "id", "value": "sess-42" }),
        ),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T6", "Draft the reply", "plan").await;
    assert_eq!(
        disp["result"]["session_id"], "w1:p1|sess-42",
        "the agent's own session id rides in the handle (resume + transcript lookup)"
    );
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
    let resp = d.dispatch("T7", "t", "plan").await;
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

#[tokio::test]
async fn a_closed_subscription_only_fails_its_own_panes() {
    // The transport's event broadcast is shared by every subscription in the
    // process, so a close notice that skipped the pane filter would fail
    // healthy concurrent tasks (#124 review).
    use agent_ide_herdr::transport::SUBSCRIPTION_CLOSED_EVENT;

    let closed = json!({
        "event": SUBSCRIPTION_CLOSED_EVENT,
        "data": { "pane_id": "w1:p9" },   // a *sibling* task's pane
    });
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![closed, status_event(PANE, "done")];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T8", "Unaffected task", "plan").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    let note = d.recv().await.expect("a notification");
    assert_eq!(
        note["params"]["state"], "done",
        "another pane's close notice must not fail this task: {note}"
    );
}
