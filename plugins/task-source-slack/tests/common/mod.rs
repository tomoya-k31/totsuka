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
use task_source_slack::llm::{ChatError, ChatTransport};
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
    chat_responses: Arc<Mutex<VecDeque<Result<Value, ChatError>>>>,
    chat_requests: Arc<Mutex<Vec<Value>>>,
    /// What `granted_scopes` reports. `None` (the default) is the real
    /// transport-cannot-see-headers case, which the scope check must ignore.
    scopes: Arc<Mutex<Option<Vec<String>>>>,
}

impl Shared {
    /// Make `granted_scopes` report `scopes` instead of "cannot tell" (#379).
    pub fn set_scopes(&self, scopes: &[&str]) {
        *self.scopes.lock().unwrap() = Some(scopes.iter().map(|s| s.to_string()).collect());
    }
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
    /// Queue a response *ahead* of the ones already registered for `method`.
    /// The last entry stays sticky, so this is how a test makes the first
    /// call fail while later calls still get the normal answer.
    pub fn push_front_for(&self, method: &str, canned: Canned) {
        self.keyed
            .lock()
            .unwrap()
            .entry(method.to_string())
            .or_default()
            .push_front(canned);
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
    pub fn push_chat(&self, outcome: Result<Value, ChatError>) {
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
    fn granted_scopes(
        &self,
        _token: TokenKind,
    ) -> impl Future<Output = Result<Option<Vec<String>>, SlackError>> + Send {
        let scopes = self.shared.scopes.lock().unwrap().clone();
        async move { Ok(scopes) }
    }

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
    ) -> impl Future<Output = Result<Value, ChatError>> + Send {
        self.shared.chat_requests.lock().unwrap().push(body);
        let next = self.shared.chat_responses.lock().unwrap().pop_front();
        async move { next.unwrap_or_else(|| Err(ChatError::transport("no canned chat response"))) }
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

/// Like [`call`], but for the cases where the *rejection* is the behaviour
/// under test. Returns the error message.
pub async fn call_expecting_error(
    srv: &mut Server<FakeFactory>,
    id: i64,
    method: &str,
    params: Value,
) -> String {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    let response: Response =
        serde_json::from_str(&reply.line.expect("a response line")).expect("valid response");
    match response.error {
        Some(error) => error.message,
        None => panic!("{method} was expected to fail but succeeded"),
    }
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
    mention_envelope_in(envelope_id, ts, Some("100.0"))
}

/// [`mention_envelope`] with an explicit enclosing thread; `None` posts it
/// top-level. Since #242 the thread decides the **task id**, so a test that
/// wants a second *conversation* (rather than a second message of the same
/// one) has to vary this.
pub fn mention_envelope_in(envelope_id: &str, ts: &str, thread_ts: Option<&str>) -> Value {
    let mut event = json!({
        "type": "message",
        "channel": "C1",
        "user": "U_OTHER",
        "text": "<@U_ME> 原因わかりますか",
        "ts": ts,
    });
    if let Some(thread_ts) = thread_ts {
        event["thread_ts"] = json!(thread_ts);
    }
    json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": { "event": event }
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

/// The lookup-side harness (0.2.4, #242): answers every `task/lookup` the
/// pipeline sends, from a table of conversations the orchestrator "knows".
///
/// Uses the real [`plugin_sdk::LookupClient`] over a channel writer, so the
/// request/response correlation under test is the production one. Anything
/// not in the table answers `known: false` — the default a test wants, since
/// most mentions open a new conversation.
pub struct LookupHarness {
    /// The client wired into the server under test.
    pub client: plugin_sdk::LookupClient,
    known: Arc<Mutex<std::collections::HashMap<String, Option<String>>>>,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl LookupHarness {
    /// A harness that answers every lookup.
    pub fn new() -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let client = plugin_sdk::LookupClient::new(plugin_sdk::Writer::from_channel(tx));
        let known: Arc<Mutex<std::collections::HashMap<String, Option<String>>>> = Arc::default();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let responder = client.clone();
        let table = Arc::clone(&known);
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "task/lookup", "{request}");
                let params = request["params"].clone();
                let task_id = params["task_id"].as_str().unwrap_or_default().to_string();
                log.lock().unwrap().push(params);
                let answer = match table.lock().unwrap().get(&task_id) {
                    Some(repo) => json!({ "known": true, "repo": repo }),
                    None => json!({ "known": false }),
                };
                responder.resolve(&json!({
                    "jsonrpc": "2.0", "id": request["id"], "result": answer,
                }));
            }
        });
        Self {
            client,
            known,
            seen,
        }
    }

    /// A harness that **never answers** — the degradation path, where the
    /// orchestrator's event loop is busy. The timeout is squeezed to keep the
    /// test quick; production waits 10s.
    pub fn unanswered() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        // `_rx` is dropped, so `send_line` fails immediately and the client
        // reports "writer closed" — the same `Lookup::Unknown` a timeout
        // produces, without the wait.
        let client = plugin_sdk::LookupClient::new(plugin_sdk::Writer::from_channel(tx))
            .with_timeout(Duration::from_millis(50));
        Self {
            client,
            known: Arc::default(),
            seen: Arc::default(),
        }
    }

    /// Declare `task_id` an existing conversation, bound to `repo` (or to
    /// none, when repository selection has not settled yet).
    pub fn mark_known(&self, task_id: &str, repo: Option<&str>) {
        self.known
            .lock()
            .unwrap()
            .insert(task_id.to_string(), repo.map(str::to_string));
    }

    /// The params of every `task/lookup` asked so far.
    pub fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

impl Default for LookupHarness {
    fn default() -> Self {
        Self::new()
    }
}
