//! End-to-end plugin flow over a **real Unix-socket fake herdr** modelled on
//! herdr 0.7.4 as verified live in #124: NDJSON with **one request per
//! connection** (the server closes after every response), `events.subscribe` as
//! the only persistent connection (pushing `{event, data}` envelopes), the
//! `agent.start` dispatch, and — crucially — a CLI that **accepts keystrokes
//! before it acts on them**, so early `agent.send`/Enter are dropped.
//!
//! Covers initialize → task/dispatch (typing + submitting the prompt through
//! that startup race, plus 0.1.3 hook `env` injection + `--settings`/`--resume`
//! launch) → the reduced state stream (a `pane.exited` **deadman**: status
//! changes produce no notification, nonzero/absent exit → `failed`, clean exit
//! is silent), `diagnostics/snapshot`, session/attach success and
//! pane-not-found, and `id: ""` error correlation (F-32/F-37/F-38, #131).

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
    /// Only the final `pane.focus` reports a vanished pane — the pane
    /// disappears *between* the liveness check and the focus chain's last
    /// step (`session/focus` must degrade to `focused: false`, not error).
    pane_focus_gone: bool,
    /// The pane's `agent_session` (drives transcript lookup), if reported.
    agent_session: Option<Value>,
    /// The pane's `cwd` as `pane.get` reports it (`None` = the nullable field
    /// is absent — drives `session/release`'s degrade-open path).
    pane_cwd: Option<&'static str>,
    /// The pane's `label` as `pane.get` reports it, if any.
    pane_label: Option<&'static str>,
    /// The full pane inventory `pane.list` reports (#211).
    list_panes: Vec<Value>,
    /// `pane.read` text for the `detection` source.
    detection: &'static str,
    /// When set, `workspace.create` fails with an `id: ""` decode-style error.
    empty_id_error_on_create: bool,
    /// When set, every call the prompt submission makes fails with this herdr
    /// error code — a herdr that is down rather than momentarily busy.
    submission_error: Option<&'static str>,
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
            pane_focus_gone: false,
            agent_session: None,
            pane_cwd: Some("/wt/agent-1"),
            pane_label: None,
            list_panes: Vec::new(),
            detection: "",
            empty_id_error_on_create: false,
            submission_error: None,
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
        let method = req["method"].as_str().unwrap_or("");
        // A herdr that is down fails every call the submission makes — the
        // retries must not turn that into a symptom with no cause.
        if let Some(code) = self.submission_error
            && matches!(
                method,
                "agent.send" | "pane.send_keys" | "pane.get" | "pane.read"
            )
        {
            reply_error(&mut write_half, &id, code, "herdr is not reachable").await;
            return;
        }
        match method {
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

            // The focus chain (`session/focus`, F-94). `pane_gone` fails every
            // pane-scoped call (so the `pane.get` liveness check stops the
            // chain before it starts); `pane_focus_gone` fails only the final
            // `pane.focus` — the pane vanished *after* the liveness check.
            "workspace.focus" | "tab.focus" => {
                reply(&mut write_half, &id, json!({ "type": "ok" })).await
            }
            "pane.focus" if self.pane_gone || self.pane_focus_gone => {
                reply_error(&mut write_half, &id, "pane_not_found", "pane not found").await
            }
            "pane.focus" => reply(&mut write_half, &id, json!({ "type": "ok" })).await,

            "pane.get" if self.pane_gone => {
                reply_error(&mut write_half, &id, "pane_not_found", "pane not found").await
            }
            "pane.get" => {
                let status = self.cli.lock().unwrap().status.clone();
                let mut pane = json!({
                    "pane_id": PANE,
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "agent_status": status,
                });
                if let Some(cwd) = self.pane_cwd {
                    pane["cwd"] = json!(cwd);
                }
                if let Some(label) = self.pane_label {
                    pane["label"] = json!(label);
                }
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
            // The pane inventory (`session/list`, #211).
            "pane.list" => {
                reply(
                    &mut write_half,
                    &id,
                    json!({ "type": "pane_list", "panes": self.list_panes.clone() }),
                )
                .await
            }

            // A vanished pane fails `pane.read` too — `diagnostics/snapshot`
            // maps that to `text: None`, not an error.
            "pane.read" if self.pane_gone => {
                reply_error(&mut write_half, &id, "pane_not_found", "pane not found").await
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
                // The reduced deadman stream takes its event receiver before
                // subscribing, so the ACK-then-push order is enough — no seeding
                // wait is needed (it no longer reads the pane on subscribe).
                let events = self.events_on_subscribe.lock().unwrap().clone();
                for ev in events {
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
    // NB: no exit_code — herdr 0.7.x does not carry one, so the deadman cannot
    // confirm a clean exit and treats it as `failed`.
    json!({
        "event": "pane_exited",
        "data": { "pane_id": pane_id, "workspace_id": "w1", "type": "pane_exited" },
    })
}

/// A `pane_exited` carrying an explicit exit code (a future/hook-aware herdr):
/// nonzero → `failed`, zero → a silent clean exit.
fn exited_event_code(pane_id: &str, code: i64) -> Value {
    json!({
        "event": "pane_exited",
        "data": { "pane_id": pane_id, "workspace_id": "w1", "exit_code": code },
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
    /// timeout so a missing notification fails fast instead of hanging.
    async fn recv(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_secs(15), self.out.recv())
            .await
            .expect("timed out waiting for plugin output")?;
        Some(serde_json::from_str(&line).expect("valid JSON line"))
    }

    /// Wait up to `ms` for an output line; `None` means none arrived — used for
    /// negative assertions ("no notification is produced").
    async fn recv_within(&mut self, ms: u64) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_millis(ms), self.out.recv())
            .await
            .ok()??;
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
            cli.input.contains("Answer in the thread.") && cli.input.contains("multi-line"),
            "the whole multi-line body is typed in, not passed as argv"
        );
        // The truncated title is NOT typed when a body exists — the body carries
        // the full task text, so the title would just be a cut-off duplicate.
        assert!(
            !cli.input.contains("Draft the reply"),
            "the snippet title must not be typed above the body: {:?}",
            cli.input
        );
        assert_eq!(
            cli.input.matches("Answer in the thread.").count(),
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
async fn state_stream_ignores_status_changes_after_the_reduction() {
    // #131: completion is now hook-based, so the state stream is a `pane.exited`
    // deadman. Screen-manifest `pane.agent_status_changed` events — the old
    // completion signal — must produce NO state/notification (locking in the
    // reduction so flicker can never drive task state again).
    let fake = FakeHerdr {
        detection: DETECTION,
        ..FakeHerdr::default()
    };
    *fake.events_on_subscribe.lock().unwrap() = vec![
        exited_event("w9:p9"), // replayed history for another pane: ignored
        status_event(PANE, "blocked"),
        status_event(PANE, "working"),
        status_event(PANE, "idle"), // the old `done`-less completion signal
    ];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    let init = d.init(&socket).await;
    // The 0.1.3 capabilities are declared.
    assert_eq!(init["result"]["capabilities"]["state_stream"], true);
    assert_eq!(init["result"]["capabilities"]["resume_session"], true);
    assert_eq!(init["result"]["capabilities"]["diagnostics_snapshot"], true);

    let disp = d.dispatch("T3", "Draft the reply", "plan").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    let ack = d
        .call("state/subscribe", json!({ "session_id": session_id }))
        .await;
    assert!(ack["error"].is_null(), "subscribe failed: {ack}");

    assert!(
        d.recv_within(700).await.is_none(),
        "status changes must not produce any notification after the deadman reduction"
    );
}

#[tokio::test]
async fn state_stream_reports_failed_on_nonzero_exit() {
    // A nonzero `pane.exited` is the deadman's `failed` signal.
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![exited_event_code(PANE, 1)];
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

#[tokio::test]
async fn state_stream_reports_failed_on_exit_without_a_code() {
    // herdr 0.7.x carries no exit code, so an exit it cannot confirm as clean is
    // treated as abnormal (`failed`) — Claude in interactive mode does not exit
    // on completion, so any unexplained exit really is abnormal.
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![exited_event(PANE)];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T5b", "Failing task", "implement").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    let failed = d.recv().await.expect("failed notification");
    assert_eq!(failed["params"]["state"], "failed");
}

#[tokio::test]
async fn state_stream_is_silent_on_a_clean_exit() {
    // A clean exit (code 0) is the SessionEnd hook's job to report; the deadman
    // stays silent and simply ends the stream.
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![exited_event_code(PANE, 0)];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T5c", "Clean exit", "implement").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    assert!(
        d.recv_within(700).await.is_none(),
        "a clean exit must not produce a notification"
    );
}

#[tokio::test]
async fn diagnostics_snapshot_returns_the_pane_screen() {
    // R-10: `diagnostics/snapshot` reads the pane screen for escalation.
    let fake = FakeHerdr {
        detection: DETECTION,
        ..FakeHerdr::default()
    };
    let (socket, requests) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T10", "Diag", "implement").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();

    let resp = d
        .call("diagnostics/snapshot", json!({ "session_id": session_id }))
        .await;
    assert!(resp["error"].is_null(), "snapshot failed: {resp}");
    assert_eq!(
        resp["result"]["text"], DETECTION,
        "the snapshot carries the pane screen text"
    );
    // It reads the `recent` screen copy (R-10), not `visible`/`detection`.
    let log = requests.lock().unwrap();
    assert!(
        log.iter()
            .any(|r| r["method"] == "pane.read" && r["params"]["source"] == "recent"),
        "snapshot must read the pane with source=recent"
    );
}

#[tokio::test]
async fn diagnostics_snapshot_reports_none_when_pane_gone() {
    // A vanished pane is not an error: the result carries `text: null` so the
    // Orchestrator's escalation path never fails on a snapshot it cannot take.
    let (socket, _) = FakeHerdr {
        pane_gone: true,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "diagnostics/snapshot",
            json!({ "session_id": "w9:p9|gone" }),
        )
        .await;
    assert!(
        resp["error"].is_null(),
        "a pane-gone snapshot must not be an RPC error: {resp}"
    );
    assert!(
        resp["result"]["text"].is_null(),
        "a vanished pane reports text: null, not an error: {resp}"
    );
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
async fn session_focus_focuses_workspace_tab_and_pane_in_order() {
    // F-94: a notification click lands on the pane — the plugin focuses
    // outside-in (workspace → tab → pane) after confirming the pane is alive.
    let (socket, requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("session/focus", json!({ "session_id": "w1:p1|agent-xyz" }))
        .await;
    assert!(resp["error"].is_null(), "focus failed: {resp}");
    assert_eq!(resp["result"]["focused"], true);

    let log = requests.lock().unwrap();
    let focus_calls: Vec<(String, Value)> = log
        .iter()
        .filter(|r| r["method"].as_str().is_some_and(|m| m.ends_with(".focus")))
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_string(),
                r["params"].clone(),
            )
        })
        .collect();
    assert_eq!(
        focus_calls,
        vec![
            (
                "workspace.focus".to_string(),
                json!({ "workspace_id": "w1" })
            ),
            ("tab.focus".to_string(), json!({ "tab_id": "w1:t1" })),
            ("pane.focus".to_string(), json!({ "pane_id": "w1:p1" })),
        ],
        "the focus chain runs outside-in with the pane record's ids"
    );
    // The liveness check runs before any focus call.
    let get_at = log.iter().position(|r| r["method"] == "pane.get").unwrap();
    let first_focus = log
        .iter()
        .position(|r| r["method"] == "workspace.focus")
        .unwrap();
    assert!(
        get_at < first_focus,
        "pane.get must precede the focus chain"
    );
}

#[tokio::test]
async fn session_focus_reports_false_when_pane_gone() {
    let (socket, requests) = FakeHerdr {
        pane_gone: true,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("session/focus", json!({ "session_id": "w9:p9|gone" }))
        .await;
    // A vanished pane is `focused: false`, not an RPC error (a notification
    // clicked after the task ended is a normal path).
    assert!(
        resp["error"].is_null(),
        "should not be an RPC error: {resp}"
    );
    assert_eq!(resp["result"]["focused"], false);
    // The liveness check failed, so no focus call was made.
    let log = requests.lock().unwrap();
    assert!(
        !log.iter()
            .any(|r| { r["method"].as_str().is_some_and(|m| m.ends_with(".focus")) }),
        "no focus call may follow a failed liveness check"
    );
}

#[tokio::test]
async fn session_focus_reports_false_when_the_pane_vanishes_mid_chain() {
    // The pane can vanish *between* the liveness check and the final focus
    // call — the chain must degrade to `focused: false`, not an RPC error.
    let (socket, requests) = FakeHerdr {
        pane_focus_gone: true,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("session/focus", json!({ "session_id": "w1:p1|agent-xyz" }))
        .await;
    assert!(
        resp["error"].is_null(),
        "should not be an RPC error: {resp}"
    );
    assert_eq!(resp["result"]["focused"], false);
    // The chain ran up to the vanished pane: the containers were focused.
    let log = requests.lock().unwrap();
    assert!(
        log.iter().any(|r| r["method"] == "workspace.focus")
            && log.iter().any(|r| r["method"] == "tab.focus")
            && log.iter().any(|r| r["method"] == "pane.focus"),
        "the whole chain must have been attempted before the degrade"
    );
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
async fn dispatch_injects_hook_env_and_settings_and_resume() {
    // 0.1.3: a hook launch spec rides `env` onto both workspace.create and
    // agent.start, appends `--settings <path>` to the argv, and `--resume <id>`
    // when resuming (Slack thread continuation).
    let (socket, requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d
        .call(
            "task/dispatch",
            json!({
                "task": { "id": "TH", "source": "slack", "title": "Resume the thread",
                          "body": "Answer in the thread.\n\nContext:\n- multi-line body" },
                "worktree_path": "/wt/agent-1",
                "mode": "implement",
                "job_id": "job-7",
                "resume_session_id": "claude-sess-abc",
                "hook": {
                    "settings_path": "/data/totsuka/hooks/orchestrator-implement.json",
                    "env": {
                        "TOTSUKA_JOB_ID": "job-7",
                        "TOTSUKA_HOOK_ENDPOINT": "/run/totsuka/hook.sock",
                        "TOTSUKA_HOOK_TOKEN": "tok-1"
                    }
                }
            }),
        )
        .await;
    assert!(disp["error"].is_null(), "dispatch must succeed: {disp}");

    let log = requests.lock().unwrap();
    let expected_env = json!({
        "TOTSUKA_JOB_ID": "job-7",
        "TOTSUKA_HOOK_ENDPOINT": "/run/totsuka/hook.sock",
        "TOTSUKA_HOOK_TOKEN": "tok-1"
    });

    // env rides on workspace.create.
    let create = log
        .iter()
        .find(|r| r["method"] == "workspace.create")
        .expect("a workspace.create request");
    assert_eq!(
        create["params"]["env"], expected_env,
        "the hook env must ride on workspace.create"
    );

    // env rides on agent.start, and `--settings`/`--resume` are in the argv.
    let start = log
        .iter()
        .find(|r| r["method"] == "agent.start")
        .expect("an agent.start request");
    assert_eq!(
        start["params"]["env"], expected_env,
        "the hook env must ride on agent.start"
    );
    let argv: Vec<&str> = start["params"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        argv,
        vec![
            "claude",
            "--settings",
            "/data/totsuka/hooks/orchestrator-implement.json",
            "--resume",
            "claude-sess-abc",
        ],
        "the argv must carry --settings and --resume: {argv:?}"
    );
}

#[tokio::test]
async fn dispatch_without_a_hook_injects_no_env() {
    // The reduction is unconditional, but env injection is not: an old
    // orchestrator that sends no hook spec must not get an `env` key (nor
    // `--settings`/`--resume`).
    let (socket, requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T0", "No hook", "implement").await;
    assert!(disp["error"].is_null(), "dispatch must succeed: {disp}");

    let log = requests.lock().unwrap();
    let start = log
        .iter()
        .find(|r| r["method"] == "agent.start")
        .expect("an agent.start request");
    assert!(
        start["params"].get("env").is_none(),
        "no hook spec → no env on agent.start"
    );
    let create = log
        .iter()
        .find(|r| r["method"] == "workspace.create")
        .expect("a workspace.create request");
    assert!(
        create["params"].get("env").is_none(),
        "no hook spec → no env on workspace.create"
    );
    let argv: Vec<&str> = start["params"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        argv,
        vec!["claude"],
        "no --settings/--resume without a hook"
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
    // 0.1.3: session resume + pane diagnostics snapshots (#131).
    assert!(manifest.capabilities.resume_session);
    assert!(manifest.capabilities.diagnostics_snapshot);
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
    // healthy concurrent tasks (#124 review). A *sibling* pane's close, followed
    // by our own clean exit, must therefore produce no notification at all.
    use agent_ide_herdr::transport::SUBSCRIPTION_CLOSED_EVENT;

    let closed = json!({
        "event": SUBSCRIPTION_CLOSED_EVENT,
        "data": { "pane_id": "w1:p9" },   // a *sibling* task's pane
    });
    let fake = FakeHerdr::default();
    *fake.events_on_subscribe.lock().unwrap() = vec![closed, exited_event_code(PANE, 0)];
    let (socket, _) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T8", "Unaffected task", "plan").await;
    let session_id = disp["result"]["session_id"].as_str().unwrap().to_string();
    d.call("state/subscribe", json!({ "session_id": session_id }))
        .await;

    assert!(
        d.recv_within(700).await.is_none(),
        "another pane's close notice must not fail this task (our own exit was clean)"
    );
}

#[tokio::test]
async fn cancel_takes_down_the_whole_workspace() {
    // `dispatch` gives every task its own workspace, so a cancel that closed
    // only the pane would leave an empty one behind on every cancelled task.
    let (socket, requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call("task/cancel", json!({ "session_id": "w1:p1|sess" }))
        .await;
    assert!(resp["error"].is_null(), "cancel failed: {resp}");

    let log = requests.lock().unwrap();
    let sent = |method: &str| log.iter().any(|r| r["method"] == method);
    assert!(sent("pane.send_keys"), "the agent must be interrupted");
    assert!(sent("pane.close"), "the pane must be closed");
    let closed = log
        .iter()
        .find(|r| r["method"] == "workspace.close")
        .expect("the task's workspace must be closed too");
    assert_eq!(
        closed["params"]["workspace_id"], "w1",
        "the workspace is read off the pane id"
    );
}

#[tokio::test]
async fn release_closes_pane_and_workspace_without_interrupting() {
    // `session/release` (#210) closes a *finished* session's pane: unlike
    // cancel there is nothing to interrupt, so no ctrl+c may be sent.
    let (socket, requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "session/release",
            json!({ "session_id": "w1:p1|sess", "expect_cwd": "/wt/agent-1" }),
        )
        .await;
    assert_eq!(resp["result"]["released"], true, "release failed: {resp}");

    let log = requests.lock().unwrap();
    let sent = |method: &str| log.iter().any(|r| r["method"] == method);
    assert!(
        !sent("pane.send_keys"),
        "release must not interrupt (no ctrl+c): {log:?}"
    );
    assert!(sent("pane.close"), "the pane must be closed");
    assert!(
        sent("workspace.close"),
        "the task's workspace must be closed too"
    );
}

#[tokio::test]
async fn session_list_returns_only_totsuka_labeled_panes() {
    // `session/list` (#211) is the orphan-pane inventory. The `totsuka `
    // label filter is the ownership boundary: herdr serves human-opened
    // panes too, and those must never become release candidates.
    let fake = FakeHerdr {
        list_panes: vec![
            json!({ "pane_id": "w1:p1", "label": "totsuka 7", "cwd": "/wt/7" }),
            json!({ "pane_id": "w2:p1", "label": "my scratch pane", "cwd": "/home" }),
            json!({ "pane_id": "w3:p1", "cwd": "/home" }),
            json!({ "pane_id": "w4:p1", "label": "totsuka 9" }),
        ],
        ..FakeHerdr::default()
    };
    let (socket, _requests) = fake.spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d.call("session/list", json!({})).await;
    assert!(resp["error"].is_null(), "list failed: {resp}");
    let sessions = resp["result"]["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        2,
        "only totsuka-labeled panes: {sessions:?}"
    );
    // The session id encodes the pane with an empty agent session — the bare
    // form `session/release` decodes.
    assert_eq!(sessions[0]["session_id"], "w1:p1|");
    assert_eq!(sessions[0]["label"], "totsuka 7");
    assert_eq!(sessions[0]["cwd"], "/wt/7");
    assert_eq!(sessions[1]["session_id"], "w4:p1|");
    assert!(sessions[1]["cwd"].is_null(), "absent cwd stays absent");
}

#[tokio::test]
async fn session_list_is_empty_without_totsuka_panes() {
    let (socket, _requests) = FakeHerdr::default().spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d.call("session/list", json!({})).await;
    assert!(resp["error"].is_null(), "list failed: {resp}");
    assert_eq!(resp["result"]["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn release_refuses_on_identity_mismatch() {
    // Position-based pane ids can be reused; a live pane whose cwd is not the
    // expected worktree is someone else's pane — nothing may be closed.
    let (socket, requests) = FakeHerdr {
        pane_cwd: Some("/wt/someone-else"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "session/release",
            json!({ "session_id": "w1:p1|sess", "expect_cwd": "/wt/agent-1" }),
        )
        .await;
    assert_eq!(resp["result"]["released"], false);

    let log = requests.lock().unwrap();
    assert!(
        !log.iter()
            .any(|r| r["method"] == "pane.close" || r["method"] == "workspace.close"),
        "a mismatched pane must not be touched: {log:?}"
    );
}

#[tokio::test]
async fn release_refuses_on_label_mismatch_even_when_cwd_matches() {
    // One comparable pair mismatching is enough to refuse — the fields are
    // checked all-must-match, not any-may-match.
    let (socket, requests) = FakeHerdr {
        pane_label: Some("totsuka OTHER"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "session/release",
            json!({
                "session_id": "w1:p1|sess",
                "expect_cwd": "/wt/agent-1",
                "expect_label": "totsuka T1",
            }),
        )
        .await;
    assert_eq!(resp["result"]["released"], false);
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "pane.close" || r["method"] == "workspace.close"),
    );
}

#[tokio::test]
async fn release_degrades_open_when_identity_is_unverifiable() {
    // The pane reports none of the expected fields (herdr's cwd/label are
    // nullable): degrade-open and close anyway — refusing here would leak a
    // pane on every task to guard against a rare reused id.
    let (socket, requests) = FakeHerdr {
        pane_cwd: None,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "session/release",
            json!({ "session_id": "w1:p1|sess", "expect_cwd": "/wt/agent-1" }),
        )
        .await;
    assert_eq!(resp["result"]["released"], true);
    let log = requests.lock().unwrap();
    let sent = |method: &str| log.iter().any(|r| r["method"] == method);
    assert!(sent("pane.close") && sent("workspace.close"));
}

#[tokio::test]
async fn release_reports_false_when_pane_already_gone() {
    // A cancelled task's pane was already closed by `cancel` (#210 risk 4):
    // release finds nothing and answers `released: false` — harmless, and it
    // must not blind-close the workspace either (identity unverified).
    let (socket, requests) = FakeHerdr {
        pane_gone: true,
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let resp = d
        .call(
            "session/release",
            json!({ "session_id": "w1:p1|sess", "expect_cwd": "/wt/agent-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "a vanished pane is not an error");
    assert_eq!(resp["result"]["released"], false);
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "pane.close" || r["method"] == "workspace.close"),
        "nothing may be closed when the pane is unverifiable"
    );
}

#[tokio::test]
async fn a_failing_herdr_is_reported_with_its_cause() {
    // The retries absorb a blip, but a herdr that is simply down would
    // otherwise be reported as the symptom alone ("it never started"), with the
    // real error only in the plugin's stderr — a diagnosability regression the
    // pre-retry code did not have.
    let (socket, _) = FakeHerdr {
        submission_error: Some("internal_error"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("T9", "Herdr is down", "plan").await;
    let message = disp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("internal_error") && message.contains("not reachable"),
        "the failure must carry what herdr actually said: {message}"
    );
}

/// A dispatch that asks to resume `claude-sess-abc`, against a herdr whose
/// calls fail with `code` once the pane is up. Returns the error response.
async fn dispatch_resuming_against(code: &'static str) -> Value {
    let (socket, _) = FakeHerdr {
        submission_error: Some(code),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    d.call(
        "task/dispatch",
        json!({
            "task": { "id": "TR", "source": "slack", "title": "Continue the thread" },
            "worktree_path": "/wt/agent-1",
            "mode": "implement",
            "resume_session_id": "claude-sess-abc",
        }),
    )
    .await
}

#[tokio::test]
async fn a_resumed_dispatch_whose_pane_vanished_is_session_unresumable() {
    // #261: `claude --resume <id>` finding no such conversation exits at once
    // and takes its pane with it — herdr then answers `agent_not_found` to
    // everything the prompt submission tries. The plugin translates its own
    // backend's vocabulary into the protocol's: the Orchestrator retries once
    // without the session (#242) instead of failing the task.
    let disp = dispatch_resuming_against("agent_not_found").await;
    assert_eq!(
        disp["error"]["code"], -32006,
        "a vanished pane on a resumed dispatch is SESSION_UNRESUMABLE: {disp}"
    );
    let message = disp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("agent_not_found"),
        "the herdr error stays in the message for whoever debugs it: {message}"
    );
}

#[tokio::test]
async fn a_vanished_pane_without_resume_keeps_its_own_error() {
    // The other half of the contract: nothing about a dispatch that named no
    // session can be blamed on resuming one. Answering SESSION_UNRESUMABLE
    // here would send the Orchestrator into a retry that changes nothing.
    let (socket, _) = FakeHerdr {
        submission_error: Some("agent_not_found"),
        ..FakeHerdr::default()
    }
    .spawn();

    let mut d = Driver::new();
    d.init(&socket).await;
    let disp = d.dispatch("TN", "A fresh task", "implement").await;
    assert_eq!(
        disp["error"]["code"], -32603,
        "no resume was asked for → the ordinary internal error: {disp}"
    );
}

#[tokio::test]
async fn a_resumed_dispatch_failing_for_another_reason_keeps_its_own_error() {
    // The classification is narrow on purpose: a pane that is alive but
    // failing (herdr busy, socket flaky) is not evidence that the session is
    // unusable, and the retry would drop the very conversation the resume
    // exists to preserve.
    let disp = dispatch_resuming_against("internal_error").await;
    assert_eq!(
        disp["error"]["code"], -32603,
        "only a *vanished* pane means the session could not be resumed: {disp}"
    );
}
