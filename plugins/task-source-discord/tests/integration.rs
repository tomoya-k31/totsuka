//! CLI-level tests: drive the JSON-RPC surface with a recorded transport, so
//! `initialize`'s validation and the token guard are exercised end to end
//! with no network.

use std::collections::VecDeque;
use std::sync::Mutex;

use plugin_protocol::jsonrpc::{Response, error_code};
use serde_json::{Value, json};
use task_source_discord::server::{Server, TransportFactory};
use task_source_discord::transport::{DiscordTransport, HttpMethod, TransportSettings};

/// Canned responses, handed out in the order they were queued.
#[derive(Default)]
struct Shared {
    queued: Mutex<VecDeque<Result<Value, u16>>>,
    /// Every `(method, path)` actually called, for asserting on round trips.
    calls: Mutex<Vec<(HttpMethod, String)>>,
}

impl Shared {
    fn push_ok(&self, value: Value) {
        self.queued.lock().unwrap().push_back(Ok(value));
    }
    fn push_status(&self, status: u16) {
        self.queued.lock().unwrap().push_back(Err(status));
    }
    fn calls(&self) -> Vec<(HttpMethod, String)> {
        self.calls.lock().unwrap().clone()
    }
}

struct FakeTransport(std::sync::Arc<Shared>);

impl DiscordTransport for FakeTransport {
    async fn call(
        &self,
        method: HttpMethod,
        path: &str,
        _body: Option<Value>,
        _idempotent: bool,
    ) -> Result<Value, task_source_discord::error::DiscordError> {
        self.0
            .calls
            .lock()
            .unwrap()
            .push((method, path.to_string()));
        let queued = self.0.queued.lock().unwrap().pop_front();
        match queued {
            Some(Ok(value)) => Ok(value),
            Some(Err(status)) => Err(task_source_discord::error::auth_failure(status)),
            // Fail loudly rather than answering `null`: a silent default here
            // would let an unexpected extra round trip pass as success, which
            // is exactly what the `calls()` assertions exist to catch.
            None => panic!("unexpected discord call: {method:?} {path} (no response queued)"),
        }
    }
}

struct FakeFactory(std::sync::Arc<Shared>);

impl TransportFactory for FakeFactory {
    type Transport = FakeTransport;
    fn build(&self, _settings: TransportSettings<'_>) -> Self::Transport {
        FakeTransport(std::sync::Arc::clone(&self.0))
    }
}

fn server(shared: &std::sync::Arc<Shared>) -> Server<FakeFactory> {
    let stdio = plugin_sdk::runtime::stdio();
    // No runtime: a Gateway task would consume the canned responses meant for
    // the assertions below.
    Server::new(FakeFactory(std::sync::Arc::clone(shared)), stdio.submit).without_runtime()
}

async fn call(srv: &mut Server<FakeFactory>, method: &str, params: Value) -> Response {
    let line = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    serde_json::from_str(&reply.line.expect("a response line")).expect("a JSON-RPC response")
}

fn config() -> Value {
    json!({
        "bot_token": "bot-token",
        "operator_user_id": "111111111111111111",
        "api_url": "https://discord.test/api/v10",
    })
}

fn watch_trigger() -> Value {
    json!({ "channel": "222222222222222222", "channel_name": "clip", "repo": "my-docs" })
}

fn init_params(config: Value, trigger: Value) -> Value {
    json!({
        "protocol_version": "0.6.0",
        "config": config,
        "repositories": [{ "name": "my-docs" }],
        "workflows": [{
            "workflow": "clip",
            "trigger": trigger,
            "task_id_prefix": "impl",
            "instructions_kind": "implement",
        }],
    })
}

fn error_of(response: &Response) -> (i64, String) {
    let error = response.error.as_ref().expect("an error response");
    (error.code, error.message.clone())
}

#[tokio::test]
async fn a_well_formed_watch_initializes_after_the_token_guard() {
    let shared = std::sync::Arc::new(Shared::default());
    shared.push_ok(json!({ "id": "999999999999999999", "username": "totsuka" }));
    let mut srv = server(&shared);

    let resp = call(
        &mut srv,
        "initialize",
        init_params(config(), watch_trigger()),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    // The guard is a real round trip, not an assumption.
    assert_eq!(
        shared.calls(),
        vec![(HttpMethod::Get, "/users/@me".to_string())]
    );
}

/// A revoked or regenerated token must stop startup as a *config* problem,
/// with the guidance, rather than as an internal error nobody can act on.
#[tokio::test]
async fn a_rejected_token_fails_initialize_as_config_invalid() {
    let shared = std::sync::Arc::new(Shared::default());
    shared.push_status(401);
    let mut srv = server(&shared);

    let resp = call(
        &mut srv,
        "initialize",
        init_params(config(), watch_trigger()),
    )
    .await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("Reset Token"), "{message}");
}

/// The one trigger kind this source has: watching nothing means it would run
/// and do nothing at all, which is never what an operator meant.
#[tokio::test]
async fn a_source_with_no_watch_fails_initialize() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let params = json!({
        "protocol_version": "0.6.0",
        "config": config(),
        "repositories": [{ "name": "my-docs" }],
        "workflows": [{ "workflow": "clip", "trigger": {} }],
    });
    let resp = call(&mut srv, "initialize", params).await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("no workflow watches"), "{message}");
    assert!(
        shared.calls().is_empty(),
        "config errors precede the network"
    );
}

#[tokio::test]
async fn a_watch_on_an_unknown_repo_fails_initialize() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let mut trigger = watch_trigger();
    trigger["repo"] = json!("nope");
    let resp = call(&mut srv, "initialize", init_params(config(), trigger)).await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("`nope`"), "{message}");
    assert!(message.contains("my-docs"), "{message}");
}

/// The watch keys are all in this source's valid-key list, so an unknown one
/// is a typo — and a dropped key widens the trigger rather than narrowing it.
#[tokio::test]
async fn an_unknown_trigger_key_fails_initialize() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let mut trigger = watch_trigger();
    trigger["chanel_name"] = json!("clip");
    let resp = call(&mut srv, "initialize", init_params(config(), trigger)).await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("chanel_name"), "{message}");
}

/// `config/validate` is deliberately offline, so `doctor` and
/// `config validate` need no network and no live token.
#[tokio::test]
async fn config_validate_makes_no_round_trip() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let resp = call(&mut srv, "config/validate", json!({ "config": config() })).await;
    let result = resp.result.expect("a result");
    assert_eq!(result["valid"], true);
    assert!(shared.calls().is_empty(), "validation must stay offline");

    // …and it reports the same problems `initialize` would.
    let mut bad = config();
    bad["operator_user_id"] = json!("tomoya");
    let resp = call(&mut srv, "config/validate", json!({ "config": bad })).await;
    let result = resp.result.expect("a result");
    assert_eq!(result["valid"], false);
    assert!(
        result["errors"][0]
            .as_str()
            .unwrap()
            .contains("Copy User ID")
    );
}

/// `result/publish` before `initialize` must say so rather than panic on an
/// absent session.
#[tokio::test]
async fn result_publish_before_initialize_is_refused() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let resp = call(
        &mut srv,
        "result/publish",
        json!({ "task_id": "t1", "content": "done" }),
    )
    .await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::INVALID_REQUEST);
    assert!(message.contains("initialize"), "{message}");
}

/// A published result for a task this process never raised has nowhere to go,
/// and must say that instead of posting into the wrong place.
#[tokio::test]
async fn result_publish_without_coordinates_reports_where_they_went() {
    let shared = std::sync::Arc::new(Shared::default());
    shared.push_ok(json!({ "id": "999999999999999999" }));
    let mut srv = server(&shared);
    call(
        &mut srv,
        "initialize",
        init_params(config(), watch_trigger()),
    )
    .await;

    let resp = call(
        &mut srv,
        "result/publish",
        json!({ "task_id": "impl:222222222222222222:333", "content": "done" }),
    )
    .await;
    let (code, message) = error_of(&resp);
    assert_eq!(code, error_code::INTERNAL_ERROR);
    assert!(
        message.contains("no pending Discord coordinates"),
        "{message}"
    );
}

/// Slack has a status column to move; Discord has none, so the method is a
/// no-op — but it must still *succeed*, or every task would look failed.
#[tokio::test]
async fn update_status_is_an_accepted_no_op() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let resp = call(
        &mut srv,
        "task/update_status",
        json!({ "task_id": "t1", "status": "done" }),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert!(shared.calls().is_empty());
}

/// A line that is not JSON has no id to correlate against, so the reply must
/// carry a **null** id rather than a made-up empty string.
#[tokio::test]
async fn a_non_json_line_answers_with_a_null_id() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let reply = srv.handle_line("not json at all").await;
    let line = reply.line.expect("a response line");
    let value: Value = serde_json::from_str(&line).expect("valid JSON");
    assert_eq!(value["id"], Value::Null, "{line}");
    assert_eq!(value["error"]["code"], error_code::PARSE_ERROR);
}

/// Blank lines and notifications get no reply at all — answering a
/// notification would put an unexpected line on the wire.
#[tokio::test]
async fn blank_lines_and_notifications_are_not_answered() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    assert!(srv.handle_line("   ").await.line.is_none());
    let notification = json!({ "jsonrpc": "2.0", "method": "shutdown" });
    assert!(
        srv.handle_line(&notification.to_string())
            .await
            .line
            .is_none()
    );
}

/// Malformed request params are a *protocol* problem. Reporting them as
/// `CONFIG_INVALID` would send the operator to edit a file that is not the
/// cause.
#[tokio::test]
async fn malformed_initialize_params_are_invalid_params() {
    let shared = std::sync::Arc::new(Shared::default());
    let mut srv = server(&shared);

    let resp = call(&mut srv, "initialize", json!({ "protocol_version": 42 })).await;
    let (code, _) = error_of(&resp);
    assert_eq!(code, error_code::INVALID_PARAMS);
}
