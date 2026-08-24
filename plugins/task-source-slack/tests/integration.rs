//! End-to-end plugin flow over a recorded Web API transport (no network):
//! initialize with the TokenGuard (`auth.test` + identity check), the offline
//! `config/validate`, the stubbed task_source methods, and shutdown.

mod common;

use serde_json::{Value, json};

use common::{Canned, FakeFactory, LookupHarness, Shared, SubmitHarness, scratch_state_dir};
use plugin_protocol::jsonrpc::{Response, error_code};
use task_source_slack::server::Server;
use task_source_slack::transport::TokenKind;

fn server(shared: &Shared) -> (Server<FakeFactory>, SubmitHarness) {
    // Protocol-level tests: the Socket Mode runtime would consume the canned
    // transport queue in the background, so it stays off here. The full
    // mention flow (runtime on) is covered by tests/mention_flow.rs. The
    // harness is returned so its ack channel outlives the server.
    let harness = SubmitHarness::new();
    let srv = Server::new(
        FakeFactory {
            shared: shared.clone(),
        },
        harness.client.clone(),
        LookupHarness::new().client,
    )
    .without_runtime();
    (srv, harness)
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
        "state_dir": scratch_state_dir(),
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

fn connections_ok() -> Value {
    json!({ "ok": true, "url": "wss://socket.test/link" })
}

/// Queue the full TokenGuard exchange (`auth.test` + `apps.connections.open`).
fn push_guard_ok(shared: &Shared) {
    shared.push(Canned::Data(auth_ok()));
    shared.push(Canned::Data(connections_ok()));
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
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);

    let result = result_of(call(&mut srv, 1, "initialize", init_params()).await);
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
    assert!(result["plugin_version"].is_string());

    // The guard authenticated the *user* token via auth.test, then the
    // App-Level Token via apps.connections.open.
    let requests = shared.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "auth.test");
    assert_eq!(requests[0].token, TokenKind::User);
    assert_eq!(requests[1].method, "apps.connections.open");
    assert_eq!(requests[1].token, TokenKind::App);
}

/// A missing scope is a **warning, not a refusal**: mentions, drafts and
/// approvals all still work, so taking the plugin down over an opt-in feature
/// would cost more than it saves (#379).
///
/// That the warning is *produced* is pinned by `server::tests::scope_warnings`
/// — asserting it here would need the log captured, and a test that only
/// checks `initialize` succeeded would pass with the check deleted.
#[tokio::test]
async fn a_missing_scope_warns_without_failing_initialize() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    // The scopes the app was actually installed with: no `reactions:read`,
    // and neither channel-name scope for the configured `[[channel_groups]]`.
    shared.set_scopes(&["chat:write", "im:write", "users:read"]);
    let (mut srv, _harness) = server(&shared);

    let config = init_config();
    // The reaction trigger is declared as a workflow trigger, so it has to be
    // supplied here — without it the `reactions:read` half of this test would
    // pass for the wrong reason (no trigger configured means no scope wanted).
    let triggers = json!([
        { "workflow": "slack-reaction", "trigger": { "reaction": "totsuka-test" } },
    ]);

    // It is a warning, not a refusal: mentions, drafts and approvals all still
    // work without these scopes — only the opt-in features are dead, and
    // taking a working setup down over them would cost more than it saves.
    let result = result_of(
        call(
            &mut srv,
            1,
            "initialize",
            json!({ "protocol_version": "0.1.0", "config": config, "triggers": triggers }),
        )
        .await,
    );
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
}

/// A transport that cannot read headers reports `None`, and "cannot tell" must
/// cost nothing: no extra round trip that could not have told us anything.
#[tokio::test]
async fn unknown_scopes_add_no_round_trip() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    // No `set_scopes`: the default is the real "headers not visible" case.
    let (mut srv, _harness) = server(&shared);

    let config = init_config();
    let triggers = json!([
        { "workflow": "slack-reaction", "trigger": { "reaction": "totsuka-test" } },
    ]);

    let result = result_of(
        call(
            &mut srv,
            1,
            "initialize",
            json!({ "protocol_version": "0.1.0", "config": config, "triggers": triggers }),
        )
        .await,
    );
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
    // The guard spent exactly its two probes — no extra scope round trip that
    // could not have told it anything.
    assert_eq!(shared.requests().len(), 2);
}

#[tokio::test]
async fn initialize_probes_the_bot_token_when_configured() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    shared.push(Canned::Data(json!({ "ok": true, "user_id": "U_BOT" })));
    let (mut srv, _harness) = server(&shared);

    let mut params = init_params();
    params["config"]["bot_token"] = json!("xoxb-bot-test");
    result_of(call(&mut srv, 1, "initialize", params).await);

    // User probe, app probe, then the bot probe (#305) — no identity check
    // on the bot: it is its own identity.
    let requests = shared.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].method, "auth.test");
    assert_eq!(requests[2].token, TokenKind::Bot);
}

#[tokio::test]
async fn initialize_rejects_a_bad_bot_token_with_guidance() {
    // A configured-but-dead bot token must fail startup visibly (like the
    // xapp token), not silently drop every notification nudge (#305).
    let shared = Shared::default();
    push_guard_ok(&shared);
    shared.push(Canned::Data(
        json!({ "ok": false, "error": "invalid_auth" }),
    ));
    let (mut srv, _harness) = server(&shared);

    let mut params = init_params();
    params["config"]["bot_token"] = json!("xoxb-bot-test");
    let response = call(&mut srv, 1, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("invalid_auth"), "{message}");
    assert!(message.contains("xoxb"), "{message}");
}

#[tokio::test]
async fn initialize_rejects_a_bad_app_token_with_guidance() {
    let shared = Shared::default();
    shared.push(Canned::Data(auth_ok()));
    shared.push(Canned::Data(
        json!({ "ok": false, "error": "invalid_auth" }),
    ));
    let (mut srv, _harness) = server(&shared);

    let response = call(&mut srv, 1, "initialize", init_params()).await;
    let (code, message) = error_of(&response);
    // Every apps.connections.open API error is credential-class → config.
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("invalid_auth"), "{message}");
    assert!(message.contains("xapp"), "{message}");
}

// ---------------------------------------------------------------------------
// initialize / repositories fallback (#109)
// ---------------------------------------------------------------------------

/// `initialize` params carrying orchestrator-supplied repositories.
fn init_params_with_repos(config: Value, repositories: Value) -> Value {
    json!({
        "protocol_version": "0.1.1",
        "config": config,
        "repositories": repositories,
    })
}

#[tokio::test]
async fn initialize_falls_back_to_supplied_repositories() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);

    // No `[[repos]]` in the plugin config: the orchestrator's list is the
    // candidate set — its channel_groups references validate against it.
    let config = json!({
        "state_dir": scratch_state_dir(),
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
        "channel_groups": [{ "prefix": "dev-", "repos": ["web-app"] }],
    });
    let params = init_params_with_repos(
        config,
        json!([{ "name": "web-app", "summary": "customer web app", "path": "/repos/web-app" }]),
    );
    let result = result_of(call(&mut srv, 1, "initialize", params).await);
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
}

#[tokio::test]
async fn initialize_validates_the_supplied_candidates() {
    // Two supplied repositories without an `[llm]` cannot be classified —
    // the deferred static check fires at initialize, before any network.
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);
    let config = json!({
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
    });
    let params = init_params_with_repos(config, json!([{ "name": "a" }, { "name": "b" }]));
    let response = call(&mut srv, 1, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("[llm]"), "{message}");
    assert!(shared.requests().is_empty());
}

#[tokio::test]
async fn initialize_prefers_explicit_repos_over_supplied() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);

    // The channel rule references the *explicit* repo; were the supplied
    // list merged in instead, this reference check would not prove
    // precedence — so the supplied list names something else entirely.
    let config = json!({
        "state_dir": scratch_state_dir(),
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
        "repos": [{ "name": "web-app" }],
        "channel_groups": [{ "prefix": "dev-", "repos": ["web-app"] }],
    });
    let params = init_params_with_repos(config.clone(), json!([{ "name": "design-system" }]));
    result_of(call(&mut srv, 1, "initialize", params).await);

    // And the converse: with no explicit repos, the same channel rule fails
    // against the supplied list — proof the fallback is what got validated.
    let mut config = config;
    config["repos"] = json!([]);
    let params = init_params_with_repos(config, json!([{ "name": "design-system" }]));
    let (mut srv, _harness) = server(&shared);
    let response = call(&mut srv, 2, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("web-app"), "{message}");
}

#[tokio::test]
async fn initialize_without_any_repositories_is_config_invalid() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);
    let config = json!({
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
    });
    let response = call(
        &mut srv,
        1,
        "initialize",
        json!({
            "protocol_version": "0.1.0",
            "config": config,
        }),
    )
    .await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("[[slack.repos]]"), "{message}");
    assert!(message.contains("[[repositories]]"), "{message}");
    // Rejected before the TokenGuard spent a network call.
    assert!(shared.requests().is_empty());
}

// ---------------------------------------------------------------------------
// initialize / llm default + override (#119)
// ---------------------------------------------------------------------------

/// `initialize` params carrying the orchestrator's `[llm]` alongside its
/// repositories.
fn init_params_with_llm(config: Value, repositories: Value, llm: Value) -> Value {
    json!({
        "protocol_version": "0.1.2",
        "config": config,
        "repositories": repositories,
        "llm": llm,
    })
}

/// A plugin config with no `[llm]` of its own (classification would need one
/// as soon as there is more than one candidate).
fn config_without_llm() -> Value {
    json!({
        "state_dir": scratch_state_dir(),
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
    })
}

#[tokio::test]
async fn initialize_adopts_the_supplied_llm() {
    // Two candidates without a plugin `[llm]` would be CONFIG_INVALID (see
    // initialize_validates_the_supplied_candidates) — the orchestrator's
    // `[llm]` supplied at initialize fills in and startup succeeds.
    let shared = Shared::default();
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);
    let params = init_params_with_llm(
        config_without_llm(),
        json!([{ "name": "a" }, { "name": "b" }]),
        json!({
            "base_url": "https://openrouter.ai/api/v1",
            "model": "anthropic/claude-haiku-4.5",
            "api_key": "sk-or-resolved",
        }),
    );
    let result = result_of(call(&mut srv, 1, "initialize", params).await);
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));
}

#[tokio::test]
async fn initialize_prefers_the_explicit_llm_over_supplied() {
    // The supplied `[llm]` is unusable (empty base_url). With an explicit
    // plugin `[llm]` present, initialize must succeed — proof the explicit
    // table won and the supplied one was never adopted.
    let shared = Shared::default();
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);
    let mut config = config_without_llm();
    config["llm"] = json!({ "base_url": "https://llm.test/v1", "model": "m", "api_key": "k" });
    let broken_supplied = json!({ "base_url": "", "model": "m", "api_key": "k" });
    let params = init_params_with_llm(
        config,
        json!([{ "name": "a" }, { "name": "b" }]),
        broken_supplied.clone(),
    );
    result_of(call(&mut srv, 1, "initialize", params).await);

    // And the converse: without the explicit table, the same unusable
    // supplied `[llm]` counts as "nothing supplied" and the candidate check
    // fires — proof the fallback path is what got exercised above.
    let (mut srv, _harness) = server(&shared);
    let params = init_params_with_llm(
        config_without_llm(),
        json!([{ "name": "a" }, { "name": "b" }]),
        broken_supplied,
    );
    let response = call(&mut srv, 2, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("[llm]"), "{message}");
}

#[tokio::test]
async fn keyless_supplied_llm_is_not_adopted() {
    // The plugin always authenticates its classifier calls, so a supplied
    // `[llm]` without an api_key is treated as absent — with two candidates
    // that is CONFIG_INVALID, pointing at both config locations, before any
    // network call.
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);
    let params = init_params_with_llm(
        config_without_llm(),
        json!([{ "name": "a" }, { "name": "b" }]),
        json!({ "base_url": "https://openrouter.ai/api/v1", "model": "m" }),
    );
    let response = call(&mut srv, 1, "initialize", params).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("`[slack]` in config.toml"), "{message}");
    assert!(message.contains("api_key_ref"), "{message}");
    assert!(shared.requests().is_empty());
}

#[tokio::test]
async fn config_validate_accepts_an_omitted_repos_list() {
    // Offline validation cannot know what initialize will supply, so an
    // omitted `[[repos]]` (and its channel references) defer to initialize.
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);
    let config = json!({
        "app_token": "xapp-1-A1-test",
        "user_token": "xoxp-user-test",
        "target_user_id": "U_ME",
        "channel_groups": [{ "prefix": "dev-", "repos": ["web-app"] }],
    });
    let result = result_of(call(&mut srv, 1, "config/validate", json!({ "config": config })).await);
    assert_eq!(result["valid"], json!(true));
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
        let (mut srv, _harness) = server(&shared);

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
    let (mut srv, _harness) = server(&shared);

    let response = call(&mut srv, 1, "initialize", init_params()).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::CONFIG_INVALID);
    assert!(message.contains("U_SOMEONE_ELSE"), "{message}");
    assert!(message.contains("U_ME"), "{message}");
    assert!(message.contains("target_user_id"), "{message}");

    // A failed guard leaves the server uninitialized.
    let response = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "1", "status": "実装待ち" }),
    )
    .await;
    assert_eq!(error_of(&response).0, error_code::INVALID_REQUEST);
}

#[tokio::test]
async fn initialize_network_failure_is_internal_not_config() {
    let shared = Shared::default();
    shared.push(Canned::Network);
    let (mut srv, _harness) = server(&shared);

    let response = call(&mut srv, 1, "initialize", init_params()).await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::INTERNAL_ERROR);
    assert!(message.contains("transport"), "{message}");
}

#[tokio::test]
async fn initialize_rejects_malformed_config() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);

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
    let (mut srv, _harness) = server(&shared);

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
    let (mut srv, _harness) = server(&shared);

    // Bot token instead of user token, and a channel rule referencing an
    // unknown repo. Two repos without an `[llm]` are legal offline since
    // #119 — initialize may adopt the orchestrator's `[llm]`, so that check
    // fires there (see initialize_prefers_the_explicit_llm_over_supplied).
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
    assert!(!all.contains("[llm]"), "{all}");
}

#[tokio::test]
async fn config_validate_reports_unknown_keys() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);

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
// task_source methods (runtime off: protocol behavior only)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_source_methods_require_initialize() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);

    for (method, params) in [
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
async fn task_source_methods_answer_after_initialize() {
    let shared = Shared::default();
    push_guard_ok(&shared);
    let (mut srv, _harness) = server(&shared);
    result_of(call(&mut srv, 1, "initialize", init_params()).await);

    // Status update: accepted as a no-op (Slack has no status column).
    let result = call(
        &mut srv,
        3,
        "task/update_status",
        json!({ "task_id": "C1:1.2", "status": "done" }),
    )
    .await;
    assert_eq!(result_of(result), Value::Null);

    // Publish for a task nobody is waiting on (e.g. after a restart): the
    // reply has nowhere to go, so the request fails instead of silently
    // dropping the draft.
    let response = call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "C1:1.2", "content": "draft", "format": "markdown" }),
    )
    .await;
    let (code, message) = error_of(&response);
    assert_eq!(code, error_code::INTERNAL_ERROR);
    assert!(message.contains("C1:1.2"), "{message}");

    // Nothing beyond the TokenGuard's two probes hit the transport.
    assert_eq!(shared.requests().len(), 2);
}

// ---------------------------------------------------------------------------
// protocol plumbing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_flags_exit() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);
    let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "shutdown" });
    let reply = srv.handle_line(&line.to_string()).await;
    assert!(reply.shutdown);
    assert!(reply.line.is_some());
}

#[tokio::test]
async fn malformed_and_notification_lines() {
    let shared = Shared::default();
    let (mut srv, _harness) = server(&shared);

    // Non-JSON → PARSE_ERROR with a response line.
    let reply = srv.handle_line("not json").await;
    let response: Response = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(error_of(&response).0, error_code::PARSE_ERROR);

    // A notification (no id) and a blank line get no reply.
    let notification = json!({ "jsonrpc": "2.0", "method": "task/update_status" });
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
