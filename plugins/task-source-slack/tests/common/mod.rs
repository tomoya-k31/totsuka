//! Shared recorded-transport fake for this plugin's test crates: canned Web
//! API responses in, recorded requests out — no network involved.

// Each test crate compiles its own copy of this module and uses a different
// subset of it; unused helpers in one crate are not dead code.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use task_source_slack::config::LlmConfig;
use task_source_slack::error::SlackError;
use task_source_slack::llm::ChatTransport;
use task_source_slack::server::TransportFactory;
use task_source_slack::transport::{SlackTransport, TokenKind, TransportSettings};

/// A canned Web API outcome for one `call`.
#[derive(Clone)]
pub enum Canned {
    /// A full response body.
    Data(Value),
    /// Simulate a network failure.
    Network,
}

/// One recorded request: its token kind, method, body, and retry class.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub token: TokenKind,
    pub method: String,
    pub body: Option<Value>,
    pub idempotent: bool,
}

/// One recorded `response_url` POST.
#[derive(Clone, Debug)]
pub struct PostedUrl {
    pub url: String,
    pub body: Value,
}

/// State shared between the factory, the transports it builds, and the test.
#[derive(Clone, Default)]
pub struct Shared {
    responses: Arc<Mutex<VecDeque<Canned>>>,
    /// Method-keyed responses, consulted before the global queue. For tests
    /// where concurrent background tasks make a single ordered queue racy.
    keyed: Arc<Mutex<std::collections::HashMap<String, VecDeque<Canned>>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    posted_urls: Arc<Mutex<Vec<PostedUrl>>>,
    chat_responses: Arc<Mutex<VecDeque<Result<Value, String>>>>,
    chat_requests: Arc<Mutex<Vec<Value>>>,
}

impl Shared {
    pub fn push(&self, canned: Canned) {
        self.responses.lock().unwrap().push_back(canned);
    }
    /// Queue a response for one specific Web API `method`. The last entry is
    /// sticky: it keeps answering repeats of the method.
    pub fn push_for(&self, method: &str, canned: Canned) {
        self.keyed
            .lock()
            .unwrap()
            .entry(method.to_string())
            .or_default()
            .push_back(canned);
    }
    fn next_response(&self, method: &str) -> Option<Canned> {
        if let Some(queue) = self.keyed.lock().unwrap().get_mut(method) {
            return match queue.len() {
                0 => None,
                1 => queue.front().cloned(), // sticky last answer
                _ => queue.pop_front(),
            };
        }
        self.responses.lock().unwrap().pop_front()
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
    pub fn posted_urls(&self) -> Vec<PostedUrl> {
        self.posted_urls.lock().unwrap().clone()
    }
    /// Queue one chat-completion outcome for the repo classifier.
    pub fn push_chat(&self, outcome: Result<Value, String>) {
        self.chat_responses.lock().unwrap().push_back(outcome);
    }
    pub fn chat_requests(&self) -> Vec<Value> {
        self.chat_requests.lock().unwrap().clone()
    }
}

/// A [`SlackTransport`] answering from the shared canned queue.
pub struct FakeTransport {
    pub shared: Shared,
}

impl SlackTransport for FakeTransport {
    fn call(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, SlackError>> + Send {
        self.shared.requests.lock().unwrap().push(Recorded {
            token,
            method: method.to_string(),
            body,
            idempotent,
        });
        let next = self.shared.next_response(method);
        async move {
            match next {
                Some(Canned::Data(v)) => Ok(v),
                Some(Canned::Network) => Err(SlackError::Transport("connection refused".into())),
                None => Err(SlackError::InvalidResponse("no canned response".into())),
            }
        }
    }

    fn post_url(
        &self,
        url: &str,
        body: Value,
    ) -> impl Future<Output = Result<(), SlackError>> + Send {
        self.shared.posted_urls.lock().unwrap().push(PostedUrl {
            url: url.to_string(),
            body,
        });
        async { Ok(()) }
    }
}

/// A [`TransportFactory`] producing [`FakeTransport`]s over the same state.
pub struct FakeFactory {
    pub shared: Shared,
}

impl TransportFactory for FakeFactory {
    type Transport = FakeTransport;
    type Chat = FakeChat;
    fn build(&self, _settings: TransportSettings<'_>) -> FakeTransport {
        FakeTransport {
            shared: self.shared.clone(),
        }
    }
    fn build_chat(&self) -> FakeChat {
        FakeChat {
            shared: self.shared.clone(),
        }
    }
}

/// A [`ChatTransport`] answering from the shared canned queue.
pub struct FakeChat {
    pub shared: Shared,
}

impl ChatTransport for FakeChat {
    fn complete(
        &self,
        _config: &LlmConfig,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        self.shared.chat_requests.lock().unwrap().push(body);
        let next = self.shared.chat_responses.lock().unwrap().pop_front();
        async move { next.unwrap_or_else(|| Err("no canned chat response".into())) }
    }
}

/// A transport over (a clone of) `shared`.
pub fn transport(shared: &Shared) -> FakeTransport {
    FakeTransport {
        shared: shared.clone(),
    }
}

/// A fresh scratch `state_dir` for one test server, so the persisted draft
/// store (#122) never touches the developer's real XDG state and tests never
/// share a `drafts.json`. Each call returns a distinct directory.
pub fn scratch_state_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "totsuka-slack-state-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// Runtime-on harness: a local WebSocket mock plays Slack's Socket Mode side,
// plus JSON-RPC and envelope helpers shared by the flow test crates.
// ---------------------------------------------------------------------------

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

use plugin_protocol::jsonrpc::Response;
use task_source_slack::server::Server;

/// Send one JSON-RPC request line and return its (successful) result value.
pub async fn call(srv: &mut Server<FakeFactory>, id: i64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    let response: Response =
        serde_json::from_str(&reply.line.expect("a response line")).expect("valid response");
    if let Some(error) = &response.error {
        panic!("{method} failed: {}", error.message);
    }
    response.result.unwrap_or(Value::Null)
}

/// A bound TCP listener for the Socket Mode mock and its `ws://` URL.
pub async fn ws_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// Accept one WebSocket connection and greet it with Slack's `hello`.
pub async fn accept_with_hello(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (socket, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(socket).await.unwrap();
    ws.send(WsMessage::text(json!({ "type": "hello" }).to_string()))
        .await
        .unwrap();
    ws
}

/// Push one envelope to the plugin and wait for its ack.
pub async fn send_and_await_ack(ws: &mut WebSocketStream<TcpStream>, envelope: Value) {
    ws.send(WsMessage::text(envelope.to_string()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ack within 2s")
        .expect("stream open")
        .expect("readable frame");
    let ack: Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
    assert!(ack["envelope_id"].is_string());
}

/// A `message` event envelope: `U_OTHER` mentioning `U_ME` in `C1`, inside
/// the thread rooted at `100.0`.
pub fn mention_envelope(envelope_id: &str, ts: &str) -> Value {
    json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": { "event": {
            "type": "message",
            "channel": "C1",
            "user": "U_OTHER",
            "text": "<@U_ME> 原因わかりますか",
            "ts": ts,
            "thread_ts": "100.0"
        }}
    })
}

/// A `block_actions` envelope: one button press (`value` as-is) inside
/// `channel`, carrying the standard test `response_url`.
pub fn block_actions_envelope(
    envelope_id: &str,
    action_id: &str,
    value: &str,
    channel: &str,
) -> Value {
    json!({
        "type": "interactive",
        "envelope_id": envelope_id,
        "payload": {
            "type": "block_actions",
            "response_url": "https://hooks.slack.test/r/1",
            "container": { "channel_id": channel },
            "actions": [{ "action_id": action_id, "value": value }]
        }
    })
}

/// The push-side observation channel (0.1.6): the pipeline's `task/submit`
/// requests land in `rx`; [`SubmitHarness::next_task`] reads one, acks it
/// `accepted`, and returns the task — the push analogue of the old
/// fetch-until-tasks polling.
pub struct SubmitHarness {
    /// The client wired into the server under test.
    pub client: plugin_sdk::SubmitClient,
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl SubmitHarness {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Short timeouts: a test that never acks must not stall for minutes.
        let client = plugin_sdk::SubmitClient::new(plugin_sdk::Writer::from_channel(tx))
            .with_timeouts(Duration::from_secs(5), Duration::from_millis(10));
        Self { client, rx }
    }

    /// Await the next `task/submit`, ack it `accepted`, return its task.
    pub async fn next_task(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.rx.recv())
            .await
            .expect("no task/submit within 5s")
            .expect("submit channel closed");
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "task/submit", "{request}");
        self.client.resolve(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": { "status": "accepted" }
        }));
        request["params"]["task"].clone()
    }

    /// Assert nothing is submitted within `window`.
    pub async fn assert_no_task(&mut self, window: Duration) {
        match tokio::time::timeout(window, self.rx.recv()).await {
            Err(_) => {}
            Ok(line) => panic!("unexpected task/submit: {line:?}"),
        }
    }
}

impl Default for SubmitHarness {
    fn default() -> Self {
        Self::new()
    }
}

/// Wait until `condition` holds (the pipeline handles envelopes after the
/// ack, so effects trail `send_and_await_ack`).
pub async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timeout waiting for {what}");
}
