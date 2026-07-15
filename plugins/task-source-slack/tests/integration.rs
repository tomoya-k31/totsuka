//! End-to-end plugin flow over a recorded Web API transport (no network):
//! initialize with the TokenGuard (`auth.test` + identity check), the offline
//! `config/validate`, the stubbed task_source methods, and shutdown.

mod common;

use serde_json::{Value, json};

use common::{Canned, FakeFactory, Shared};
use plugin_protocol::jsonrpc::{Response, error_code};
use task_source_slack::server::Server;
use task_source_slack::transport::TokenKind;

fn server(shared: &Shared) -> Server<FakeFactory> {
    // Protocol-level tests: the Socket Mode runtime would consume the canned
    // transport queue in the background, so it stays off here. The full
    // mention flow (runtime on) is covered by tests/mention_flow.rs.
    Server::new(FakeFactory {
        shared: shared.clone(),
    })
    .without_runtime()
}

/// Send one JSON-RPC request line and return the parsed response.
async fn call(srv: &mut Server<FakeFactory>, id: i64, method: &str, params: Value) -> Response {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    serde_json::from_str(&reply.line.expect("a response line")).expect("valid response")
}

/// A config with two repos, channel rules, and the LLM table — the full shape.
fn init_config() -> Value {
    json!({
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
        "thread_context_limit": 4,
        "reply_style": "丁寧語で簡潔に",
        "llm": {
            "base_url": "https://openrouter.test/api/v1",
            "model": "test-model",
            "api_key": "sk-test",
            "confidence_threshold": 0.7
        },
        "channel_groups": [
            { "prefix": "dev-frontend-", "repos": ["web-app"] }
        ],
        "repos": [
            { "name": "web-app", "summary": "customer web app" },
            { "name": "design-system" }
        ]
    })
}

fn init_params() -> Value {
    json!({ "protocol_version": "0.1.0", "config": init_config() })
}

fn auth_ok() -> Value {
    json!({ "ok": true, "user_id": "U_ME", "user": "me", "team": "T1" })
}

/// The error object of an error response.
fn error_of(response: &Response) -> (i64, String) {
    let error = response.error.as_ref().expect("an error response");
    (error.code, error.message.clone())
}

/// The result value of a success response.
fn result_of(response: Response) -> Value {
    if let Some(error) = &response.error {
        panic!("expected a result, got error: {}", error.message);
    }
    // A JSON `"result": null` deserializes to `None` (serde folds JSON null
    // into the Option), so absence here means a null result, not an error.
    response.result.unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// initialize / TokenGuard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_runs_token_guard_and_declares_capabilities() {
    let shared = Shared::default();
    shared.push(Canned::Data(auth_ok()));
    let mut srv = server(&shared);

    let result = result_of(call(&mut srv, 1, "initialize", init_params()).await);
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
    assert!(result["plugin_version"].is_string());

    // The guard authenticated with the *user* token via auth.test.
    let requests = shared.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "auth.test");
    assert_eq!(requests[0].token, TokenKind::User);
}

#[tokio::test]
async fn initialize_fails_with_guidance_per_auth_error() {
    for (code, expect) in [
        ("invalid_auth", "re-issue"),
        ("token_revoked", "re-install"),
        ("account_inactive", "deactivated"),
    ] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({ "ok": false, "error": code })));
        let mut srv = server(&shared);

        let response = call(&mut srv, 1, "initialize", init_params()).await;
        let (rpc_code, message) = error_of(&response);
        assert_eq!(rpc_code, error_code::CONFIG_INVALID, "{code}");
        assert!(message.contains(code), "{code}: {message}");
        assert!(message.contains(expect), "{code}: {message}");
    }
}

#[tokio::test]
async fn initialize_rejects_identity_mismatch() {
    let shared = Shared::default();
    shared.push(Canned::Data(
        json!({ "ok": true, "user_id": "U_SOMEONE_ELSE" }),
    ));
    let mut srv = server(&shared);

    let response = call(&mut srv, 1, "initialize", init_params()).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("U_SOMEONE_ELSE"), "{message}");
    assert!(message.contains("U_ME"), "{message}");
    assert!(message.contains("target_user_id"), "{message}");

    // A failed guard leaves the server uninitialized.
    let response = call(&mut srv, 2, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(error_of(&response).0, error_code::INVALID_REQUEST);
}

#[tokio::test]
async fn initialize_network_failure_is_internal_not_config() {
    let shared = Shared::default();
    shared.push(Canned::Network);
    let mut srv = server(&shared);

    let response = call(&mut srv, 1, "initialize", init_params()).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::INTERNAL_ERROR);
    assert!(message.contains("transport"), "{message}");
}

#[tokio::test]
async fn initialize_rejects_malformed_config() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    let mut config = init_config();
    config["typo_field"] = json!(true);
    let params = json!({ "protocol_version": "0.1.0", "config": config });
    let response = call(&mut srv, 1, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("typo_field"), "{message}");
    // Rejected before any network call.
    assert!(shared.requests().is_empty());
}

// ---------------------------------------------------------------------------
// config/validate (offline)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_validate_accepts_a_valid_config_without_network() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    let result = result_of(
        call(
            &mut srv,
            1,
            "config/validate",
            json!({ "config": init_config() }),
        )
        .await,
    );
    assert_eq!(result["valid"], json!(true));
    // Static validation only: no Web API call was made.
    assert!(shared.requests().is_empty());
}

#[tokio::test]
async fn config_validate_reports_static_errors() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // Bot token instead of user token, a channel rule referencing an unknown
    // repo, and two repos without [llm].
    let config = json!({
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxb-bot-token",
        "target_user_id": "U_ME",
        "channel_groups": [{ "prefix": "dev-", "repos": ["ghost"] }],
        "repos": [{ "name": "web-app" }, { "name": "design-system" }]
    });
    let result = result_of(call(&mut srv, 1, "config/validate", json!({ "config": config })).await);
    assert_eq!(result["valid"], json!(false));
    let errors = result["errors"].as_array().unwrap();
    let all = errors
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("xoxp-"), "{all}");
    assert!(all.contains("ghost"), "{all}");
    assert!(all.contains("[llm]"), "{all}");
}

#[tokio::test]
async fn config_validate_reports_unknown_keys() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    let mut config = init_config();
    config["typo_field"] = json!(true);
    let result = result_of(call(&mut srv, 1, "config/validate", json!({ "config": config })).await);
    assert_eq!(result["valid"], json!(false));
    assert!(
        result["errors"][0].as_str().unwrap().contains("typo_field"),
        "{result}"
    );
}

// ---------------------------------------------------------------------------
// task_source stubs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_source_methods_require_initialize() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    for (method, params) in [
        ("tasks/fetch", json!({ "trigger": {} })),
        (
            "task/update_status",
            json!({ "task_id": "C1:1.2", "status": "done" }),
        ),
        (
            "result/publish",
            json!({ "task_id": "C1:1.2", "content": "draft" }),
        ),
    ] {
        let response = call(&mut srv, 1, method, params).await;
        let (code, message) = error_of(&response);
        assert_eq!(code, error_code::INVALID_REQUEST, "{method}");
        assert!(message.contains("initialize"), "{method}: {message}");
    }
}

#[tokio::test]
async fn stubs_answer_after_initialize() {
    let shared = Shared::default();
    shared.push(Canned::Data(auth_ok()));
    let mut srv = server(&shared);
    result_of(call(&mut srv, 1, "initialize", init_params()).await);

    // Fetch: no tasks yet (the mention pipeline is not part of the skeleton).
    let result = result_of(call(&mut srv, 2, "tasks/fetch", json!({ "trigger": {} })).await);
    assert_eq!(result["tasks"], json!([]));

    // Status update and publish: accepted as no-ops.
    let result = call(
        &mut srv,
        3,
        "task/update_status",
        json!({ "task_id": "C1:1.2", "status": "done" }),
    )
    .await;
    assert_eq!(result_of(result), Value::Null);
    let result = call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "C1:1.2", "content": "draft", "format": "markdown" }),
    )
    .await;
    assert_eq!(result_of(result), Value::Null);

    // Nothing beyond the TokenGuard's auth.test hit the transport.
    assert_eq!(shared.requests().len(), 1);
}

// ---------------------------------------------------------------------------
// protocol plumbing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_flags_exit() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "shutdown" });
    let reply = srv.handle_line(&line.to_string()).await;
    assert!(reply.shutdown);
    assert!(reply.line.is_some());
}

#[tokio::test]
async fn malformed_and_notification_lines() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // Non-JSON → PARSE_ERROR with a response line.
    let reply = srv.handle_line("not json").await;
    let response: Response = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(error_of(&response).0, error_code::PARSE_ERROR);

    // A notification (no id) and a blank line get no reply.
    let notification = json!({ "jsonrpc": "2.0", "method": "tasks/fetch" });
    assert!(
        srv.handle_line(&notification.to_string())
            .await
            .line
            .is_none()
    );
    assert!(srv.handle_line("   ").await.line.is_none());

    // Unknown method → METHOD_NOT_FOUND.
    let response = call(&mut srv, 9, "no/such", json!({})).await;
    assert_eq!(error_of(&response).0, error_code::METHOD_NOT_FOUND);
}
