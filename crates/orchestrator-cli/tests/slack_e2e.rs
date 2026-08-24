//! Slack plugin end-to-end (#108): drive the real `totsuka` binary with the
//! real `task-source-slack` plugin binary against an **in-process mock
//! Slack** (Web API over raw-TCP HTTP + Socket Mode over WebSocket) and the
//! mock agent, through the epic's whole loop:
//!
//! mention envelope → mention pipeline → `task/submit` push (0.1.6) →
//! dispatch (mock agent) → `result/publish` → draft surfaces (thread
//! ephemeral + self-DM)
//! → approve button (`block_actions`) → reply posted in the thread under the
//! operator's own user token — plus `totsuka doctor`'s live probe (TokenGuard
//! `auth.test` + `apps.connections.open`) against the same mock.
//!
//! Follows the harness patterns of `tests/e2e.rs` (XDG scratch env, real
//! plugin subprocesses, wall-clock guards) — the mock Slack is the only new
//! piece.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

use test_support::scratch;

/// Path to the compiled `totsuka` binary.
fn totsuka() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_totsuka"))
}

/// Build (once per test process, or not at all under
/// `TEST_SUPPORT_PREBUILT_BINS`) and locate a sibling workspace binary under
/// the same target profile dir as the test binary (#281).
fn build_bin(package: &str, bin: &str) -> PathBuf {
    test_support::sibling_bin(&totsuka(), package, bin)
}

// ---------------------------------------------------------------------------
// Mock Slack: Web API (HTTP) + Socket Mode (WebSocket)
// ---------------------------------------------------------------------------

/// One recorded Web API call.
#[derive(Clone, Debug)]
struct ApiCall {
    /// Request path, e.g. `/chat.postMessage`.
    path: String,
    /// The bearer token that authenticated the call (empty when absent).
    bearer: String,
    /// Decoded `application/x-www-form-urlencoded` fields.
    form: HashMap<String, String>,
}

/// The in-process mock Slack. HTTP answers every known Web API method with a
/// sticky canned response and records each call; the WebSocket side greets
/// every connection with `hello` and hands the stream to the test to drive.
struct MockSlack {
    /// Web API base URL for the plugin's `api_url`.
    api_url: String,
    calls: Arc<Mutex<Vec<ApiCall>>>,
    /// Accepted (and greeted) Socket Mode connections, in accept order.
    connections: mpsc::UnboundedReceiver<WebSocketStream<TcpStream>>,
}

impl MockSlack {
    async fn start() -> Self {
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_url = format!("http://{}", http.local_addr().unwrap());
        let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}", ws.local_addr().unwrap());

        let calls: Arc<Mutex<Vec<ApiCall>>> = Arc::default();
        tokio::spawn(serve_http(http, ws_url, Arc::clone(&calls)));

        let (conn_tx, connections) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Every connection (the watch run's, doctor's probe, reconnects)
            // gets Slack's `hello`; the test drives whichever it needs.
            while let Ok((socket, _)) = ws.accept().await {
                let Ok(mut stream) = accept_async(socket).await else {
                    continue;
                };
                let hello = json!({ "type": "hello" }).to_string();
                if stream.send(WsMessage::text(hello)).await.is_err() {
                    continue;
                }
                let _ = conn_tx.send(stream);
            }
        });

        Self {
            api_url,
            calls,
            connections,
        }
    }

    fn calls(&self) -> Vec<ApiCall> {
        self.calls.lock().unwrap().clone()
    }

    /// The first recorded call to `path` matching `pred`, if any.
    fn find(&self, path: &str, pred: impl Fn(&ApiCall) -> bool) -> Option<ApiCall> {
        self.calls().into_iter().find(|c| c.path == path && pred(c))
    }
}

/// Accept HTTP connections and answer Web API calls forever (keep-alive:
/// reqwest reuses connections, so each socket serves many requests).
async fn serve_http(listener: TcpListener, ws_url: String, calls: Arc<Mutex<Vec<ApiCall>>>) {
    while let Ok((mut socket, _)) = listener.accept().await {
        let ws_url = ws_url.clone();
        let calls = Arc::clone(&calls);
        tokio::spawn(async move {
            while let Some(call) = read_http_request(&mut socket).await {
                let body = respond(&call.path, &ws_url).to_string();
                calls.lock().unwrap().push(call);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Read one HTTP/1.1 request off `socket`; `None` on EOF/garbage.
async fn read_http_request(socket: &mut TcpStream) -> Option<ApiCall> {
    let mut buf = Vec::new();
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let path = lines.next()?.split_whitespace().nth(1)?.to_string();
    let mut bearer = String::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "authorization" => bearer = value.strip_prefix("Bearer ").unwrap_or(value).to_string(),
            "content-length" => content_length = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Some(ApiCall {
        path,
        bearer,
        form: parse_form(&body),
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decode an `application/x-www-form-urlencoded` body. A JSON body (the
/// `response_url` POST) produces junk entries, which no assertion reads.
fn parse_form(body: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in body.split(|&b| b == b'&') {
        if pair.is_empty() {
            continue;
        }
        let mut kv = pair.splitn(2, |&b| b == b'=');
        let key = percent_decode(kv.next().unwrap_or_default());
        let value = percent_decode(kv.next().unwrap_or_default());
        map.insert(key, value);
    }
    map
}

fn percent_decode(input: &[u8]) -> String {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < input.len() => {
                match std::str::from_utf8(&input[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The sticky canned Web API surface for the whole flow.
fn respond(path: &str, ws_url: &str) -> Value {
    match path {
        "/auth.test" => json!({ "ok": true, "user_id": "U_ME", "user": "me" }),
        "/apps.connections.open" => json!({ "ok": true, "url": ws_url }),
        "/conversations.open" => json!({ "ok": true, "channel": { "id": "D_SELF" } }),
        "/users.info" => json!({
            "ok": true,
            "user": { "name": "alice", "profile": { "display_name": "アリス" } }
        }),
        "/conversations.info" => json!({ "ok": true, "channel": { "name": "dev-general" } }),
        // The reacted-to message, re-fetched from the event's coordinates
        // (#319). Note it carries no `<@U_ME>` tag — that is what makes the
        // reaction, not a mention, the thing that started the task.
        "/conversations.history" => json!({
            "ok": true,
            "messages": [{
                "user": "U_OTHER",
                "text": "あとで調べておいてほしい",
                "ts": "300.0"
            }]
        }),
        "/conversations.replies" => json!({
            "ok": true,
            "messages": [{
                "user": "U_OTHER",
                "text": "<@U_ME> デプロイが失敗しています。原因わかりますか？",
                "ts": "100.1",
                "thread_ts": "100.0"
            }]
        }),
        "/chat.getPermalink" => {
            json!({ "ok": true, "permalink": "https://slack.test/archives/C1/p1001" })
        }
        "/chat.postMessage" => json!({ "ok": true, "ts": "200.0" }),
        "/chat.postEphemeral" => json!({ "ok": true, "message_ts": "9.1" }),
        "/chat.update" => json!({ "ok": true }),
        "/response_url/1" => json!({ "ok": true }),
        other => json!({ "ok": false, "error": format!("mock has no handler for {other}") }),
    }
}

/// Push one envelope to the plugin and wait for its ack (the plugin acks
/// every envelope immediately on receipt).
async fn send_and_await_ack(ws: &mut WebSocketStream<TcpStream>, envelope: Value) {
    ws.send(WsMessage::text(envelope.to_string()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("ack within 10s")
        .expect("stream open")
        .expect("readable frame");
    let ack: Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
    assert!(ack["envelope_id"].is_string(), "ack: {ack}");
}

/// The mention: `U_OTHER` mentions `U_ME` in `C1`, thread rooted at `100.0`.
fn mention_envelope() -> Value {
    json!({
        "type": "events_api",
        "envelope_id": "env-mention-1",
        "payload": { "event": {
            "type": "message",
            "channel": "C1",
            "user": "U_OTHER",
            "text": "<@U_ME> デプロイが失敗しています。原因わかりますか？",
            "ts": "100.1",
            "thread_ts": "100.0"
        }}
    })
}

/// The operator adding `:eyes:` to a *different* message in `C1` (#396).
///
/// A separate `ts` on purpose: reusing the mention's would land on the same
/// conversation task (ADR-0015) and prove nothing about workflow selection.
fn reaction_envelope() -> Value {
    json!({
        "type": "events_api",
        "envelope_id": "env-reaction-1",
        "payload": { "event": {
            "type": "reaction_added",
            "user": "U_ME",
            "reaction": "eyes",
            "item": { "type": "message", "channel": "C1", "ts": "300.0" },
            "item_user": "U_OTHER",
            "event_ts": "900.0"
        }}
    })
}

/// The operator pressing 承認して返信 on the draft.
fn approve_envelope(api_url: &str, draft_id: &str) -> Value {
    json!({
        "type": "interactive",
        "envelope_id": "env-approve-1",
        "payload": {
            "type": "block_actions",
            "response_url": format!("{api_url}/response_url/1"),
            "container": { "channel_id": "C1" },
            "actions": [{ "action_id": "approve_reply", "value": draft_id }]
        }
    })
}

/// The `value` of the first button in a recorded draft presentation.
fn draft_id_of(call: &ApiCall) -> String {
    let blocks: Value = serde_json::from_str(&call.form["blocks"]).expect("blocks are JSON");
    blocks
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["type"] == "actions")
        .and_then(|b| b["elements"][0]["value"].as_str())
        .expect("an actions block with a draft id")
        .to_string()
}

// ---------------------------------------------------------------------------
// XDG scratch environment (mirrors tests/e2e.rs)
// ---------------------------------------------------------------------------

struct Env {
    base: PathBuf,
}

impl Env {
    fn cfg_dir(&self) -> PathBuf {
        self.base.join("cfg/totsuka")
    }
    fn state_dir(&self) -> PathBuf {
        self.base.join("state/totsuka")
    }
    fn plugins_store(&self) -> PathBuf {
        self.base.join("data/totsuka/plugins")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(totsuka());
        cmd.args(args)
            .env("XDG_CONFIG_HOME", self.base.join("cfg"))
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_STATE_HOME", self.base.join("state"))
            .env("XDG_CACHE_HOME", self.base.join("cache"))
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run `totsuka <args>` to completion under a wall-clock guard.
    fn run(&self, args: &[&str]) -> Output {
        let start = Instant::now();
        let mut child = self.command(args).spawn().unwrap();
        let mut out_pipe = child.stdout.take().unwrap();
        let mut err_pipe = child.stderr.take().unwrap();
        let out_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out_pipe.read_to_end(&mut buf);
            buf
        });
        let err_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err_pipe.read_to_end(&mut buf);
            buf
        });
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() >= Duration::from_secs(120) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`totsuka {args:?}` did not finish within 120s (killed)");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Output {
            status,
            stdout: out_reader.join().unwrap(),
            stderr: err_reader.join().unwrap(),
        }
    }
}

/// Install a plugin binary as `name` (kind `kind`) into the store.
fn install_plugin(env: &Env, name: &str, kind: &str, binary: &Path) {
    let dir = env.plugins_store().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    test_support::place_binary(binary, &dir.join(name));
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nkind = \"{kind}\"\nversion = \"0.1.0\"\n\
             protocol_version = \">=0.1.6, <0.6\"\n\n[capabilities]\nstate_stream = true\n\
             outputs = [\"source\"]\n"
        ),
    )
    .unwrap();
}

/// XDG scratch env: git origin + clone, the real slack plugin + the mock
/// agent, and a `slack → mock_agent, output = source` workflow polling every
/// second.
fn setup(mock_api_url: &str) -> Env {
    let base = scratch("slack-e2e");
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    test_support::bare_origin_and_clone(&repo);

    let env = Env { base };
    let cfg_dir = env.cfg_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(env.state_dir()).unwrap();

    install_plugin(
        &env,
        "slack",
        "task_source",
        &build_bin("task-source-slack", "slack"),
    );
    install_plugin(
        &env,
        "mock_agent",
        "agent_ide",
        &build_bin("orchestrator-core", "mock_plugin"),
    );

    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"
poll_interval_secs = 1

[plugins.mock_agent]
enabled = true
kind = "agent_ide"

[[repositories]]
name = "clone"
path = "{clone}"

[worktree]
location = "{state}/wt/{{repo_name}}/{{worktree_name}}"
cleanup = "immediate"
plan_cleanup = "immediate"

# The emoji workflow is defined **first**: reaction triggers are more
# specific than the mention catch-all, and putting it last would make it
# unreachable (#396, `validate_workflows` warns about exactly that).
[[workflows]]
name = "watch"
source = "slack"
trigger = {{ reaction = "eyes" }}
mode = "plan"
agent = "mock_agent"
output = "none"

[[workflows]]
name = "reply"
source = "slack"
trigger = {{}}
mode = "plan"
agent = "mock_agent"
output = "source"

# Deliberately no `[[slack.repos]]`: the orchestrator supplies its single
# `[[repositories]]` entry at initialize (#109), and one candidate resolves
# without any LLM — the acceptance path for the fallback.
[slack]
app_token = "xapp-1-A1-e2e"
user_token = "xoxp-e2e-user"
target_user_id = "U_ME"
api_url = "{mock_api_url}"

[mock_agent]
stream_states = ["running", "done"]
"#,
            clone = repo.join("clone").display(),
            state = env.state_dir().display(),
        ),
    )
    .unwrap();

    env
}

/// Poll `f` until it yields, panicking with `what` after `timeout`.
fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(start.elapsed() < timeout, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill the watch run and reap it (scratch env, nothing to preserve).
fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// The E2E
// ---------------------------------------------------------------------------

#[test]
fn e2e_slack_mention_to_approved_reply_and_doctor() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut mock = rt.block_on(MockSlack::start());
    let env = setup(&mock.api_url);

    // A resident run: the approval press arrives *after* the task finishes,
    // so the plugin must outlive the publish (one-shot would shut it down).
    let child = env.command(&["run", "--watch"]).spawn().unwrap();

    // The plugin's Socket Mode client connects; Slack (the mock) pushes the
    // mention at it.
    let mut ws = rt
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(60), mock.connections.recv()).await
        })
        .expect("a Socket Mode connection within 60s")
        .expect("accept loop alive");
    rt.block_on(send_and_await_ack(&mut ws, mention_envelope()));

    // watch: submit → dispatch → done → result/publish → the draft shows up
    // as a thread ephemeral (and a self-DM record).
    let ephemeral = wait_for("the draft ephemeral", Duration::from_secs(90), || {
        mock.find("/chat.postEphemeral", |_| true)
    });
    assert_eq!(ephemeral.form["channel"], "C1");
    assert_eq!(ephemeral.form["thread_ts"], "100.0");
    assert_eq!(ephemeral.form["user"], "U_ME");
    let dm_record = wait_for("the self-DM record", Duration::from_secs(30), || {
        mock.find("/chat.postMessage", |c| {
            c.form.get("channel").map(String::as_str) == Some("D_SELF")
        })
    });
    assert!(dm_record.form.contains_key("blocks"));

    // Approve. The draft id rides in the button's `value`.
    let draft_id = draft_id_of(&ephemeral);
    rt.block_on(send_and_await_ack(
        &mut ws,
        approve_envelope(&mock.api_url, &draft_id),
    ));

    // The reply lands in the mention's thread, under the *user* token, with
    // the agent's published text — as the notification-fallback `text` and as
    // a `markdown` block so its Markdown renders properly (#454).
    let reply = wait_for("the approved reply", Duration::from_secs(30), || {
        mock.find("/chat.postMessage", |c| {
            c.form.get("thread_ts").map(String::as_str) == Some("100.0")
        })
    });
    assert_eq!(reply.bearer, "xoxp-e2e-user");
    assert_eq!(reply.form["channel"], "C1");
    // The mock agent streamed exactly one log chunk; publish trims nothing
    // beyond the mechanical `<@asker>` mention prefix.
    assert_eq!(reply.form["text"], "<@U_OTHER> compiling...");
    let reply_blocks: Value =
        serde_json::from_str(&reply.form["blocks"]).expect("reply blocks are JSON");
    assert_eq!(
        reply_blocks,
        json!([{ "type": "markdown", "text": "<@U_OTHER> compiling..." }])
    );

    // Both draft surfaces were finalized: the pressed ephemeral through its
    // response_url, the self-DM record through chat.update.
    wait_for("the response_url rewrite", Duration::from_secs(30), || {
        mock.find("/response_url/1", |_| true)
    });
    wait_for("the self-DM finalize", Duration::from_secs(30), || {
        mock.find("/chat.update", |c| {
            c.form.get("channel").map(String::as_str) == Some("D_SELF")
        })
    });

    // #396, riding the same run: an `:eyes:` reaction on a *different*
    // message must reach the `watch` workflow, not the mention catch-all.
    //
    // This is the one place the whole chain is exercised against real
    // processes — core sends `trigger.reaction` at `initialize`, the plugin
    // reads it and stamps `reaction:eyes` into `Task.labels`, and core
    // re-checks that label to select the workflow. Each link has its own unit
    // test; none of them can catch a break *between* two links, which is the
    // failure mode this notation invites (the `triggers` contract existed for
    // versions before anything read it).
    rt.block_on(send_and_await_ack(&mut ws, reaction_envelope()));
    let watched = wait_for("the reaction task", Duration::from_secs(60), || {
        let out = env.run(&["task", "list", "--json"]);
        let tasks: Value = serde_json::from_slice(&out.stdout).ok()?;
        tasks
            .as_array()?
            .iter()
            .find(|t| t["source_task_id"] == "C1:300.0")
            .cloned()
    });
    assert_eq!(
        watched["workflow"], "watch",
        "the reaction must select its own workflow, not the catch-all: {watched}"
    );
    // And the control: the mention task took the catch-all even though the
    // reaction workflow is defined above it.
    let out = env.run(&["task", "list", "--json"]);
    let tasks: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mention_task = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["source_task_id"] == "C1:100.0")
        .unwrap_or_else(|| panic!("the mention task: {tasks}"));
    assert_eq!(mention_task["workflow"], "reply", "{mention_task}");

    // Every worktree must be gone before `doctor` runs. `cleanup = immediate`
    // removes them, but waiting on the task state would not be enough:
    // `apply_event(Complete)` lands *before* `cleanup_worktree`
    // (`run/finalize.rs`), so a task can read `done` while its worktree is
    // still on disk. Wait on the directory — the condition `doctor` actually
    // checks — or the run gets killed mid-cleanup and `doctor` reports an
    // orphan worktree (observed once in CI, passed on re-run).
    let worktree_root = env.state_dir().join("wt/clone");
    wait_for(
        "the worktrees to be cleaned up",
        Duration::from_secs(60),
        || {
            match std::fs::read_dir(&worktree_root) {
                Ok(mut entries) => entries.next().is_none().then_some(()),
                // The root itself is gone: nothing is left, by definition.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(()),
                // Anything else (permissions, transient IO) must not read as
                // "clean" — that is the silent pass this wait exists to
                // prevent, and it would put the flake straight back.
                Err(e) => panic!("cannot read {}: {e}", worktree_root.display()),
            }
        },
    );

    stop(child);

    // doctor: the live probe launches the plugin and runs the TokenGuard
    // (auth.test + apps.connections.open) against the mock — all green.
    let out = env.run(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "doctor failed:\n{stdout}");
    assert!(stdout.contains("ok:   plugin:slack"), "{stdout}");

    let _ = std::fs::remove_dir_all(&env.base);
}
