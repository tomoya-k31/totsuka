//! The full mention path over the JSON-RPC boundary with the resident
//! runtime ON: a local WebSocket mock plays Slack's Socket Mode side, the
//! keyed recorded transport plays the Web API, and the test drives
//! `initialize` → mention envelope → `tasks/fetch` (issue #105 acceptance).

mod common;

use std::time::Duration;

use serde_json::{Value, json};

use common::{
    Canned, FakeFactory, Shared, accept_with_hello, block_actions_envelope, call,
    fetch_until_tasks, mention_envelope, send_and_await_ack, ws_listener,
};
use task_source_slack::server::Server;

fn server(shared: &Shared) -> Server<FakeFactory> {
    Server::new(FakeFactory {
        shared: shared.clone(),
    })
}

/// One-repo config (repo_hint short-circuit until #106) pointing the Web API
/// at the recorded transport and Socket Mode at the local WS mock.
fn init_params() -> Value {
    json!({
        "protocol_version": "0.1.0",
        "config": {
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "thread_context_limit": 3,
            "reply_style": "丁寧語で簡潔に",
            "repos": [{ "name": "web-app", "summary": "customer web app" }]
        }
    })
}

/// Canned Web API surface for the whole flow (keyed per method; the last
/// entry per method keeps answering, so reconnects and caches never starve).
fn canned_web_api(shared: &Shared, ws_url: &str) {
    canned_web_api_in_channel(shared, ws_url, "dev-frontend");
}

/// Like [`canned_web_api`] but the mention channel resolves to `channel_name`
/// (drives the `[[channel_groups]]` prefix rules).
fn canned_web_api_in_channel(shared: &Shared, ws_url: &str, channel_name: &str) {
    shared.push_for(
        "auth.test",
        Canned::Data(json!({ "ok": true, "user_id": "U_ME" })),
    );
    shared.push_for(
        "apps.connections.open",
        Canned::Data(json!({ "ok": true, "url": ws_url })),
    );
    shared.push_for(
        "conversations.open",
        Canned::Data(json!({ "ok": true, "channel": { "id": "D_SELF" } })),
    );
    shared.push_for(
        "users.info",
        Canned::Data(json!({
            "ok": true,
            "user": { "name": "alice", "profile": { "display_name": "アリス" } }
        })),
    );
    shared.push_for(
        "conversations.info",
        Canned::Data(json!({ "ok": true, "channel": { "name": channel_name } })),
    );
    shared.push_for(
        "chat.postEphemeral",
        Canned::Data(json!({ "ok": true, "message_ts": "9.1" })),
    );
    shared.push_for(
        "conversations.replies",
        Canned::Data(json!({
            "ok": true,
            "messages": [
                { "user": "U_OTHER", "text": "デプロイが失敗してる", "ts": "100.0", "thread_ts": "100.0" },
                { "user": "U_OTHER", "text": "ログ見た?", "ts": "100.1", "thread_ts": "100.0" },
                { "user": "U_OTHER", "text": "<@U_ME> 原因わかりますか", "ts": "100.2", "thread_ts": "100.0" },
            ]
        })),
    );
    shared.push_for(
        "chat.getPermalink",
        Canned::Data(json!({
            "ok": true,
            "permalink": "https://ws.slack.test/archives/C1/p1002"
        })),
    );
}

#[tokio::test]
async fn mention_becomes_a_task_and_fetch_drains_the_buffer() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    let tasks = fetch_until_tasks(&mut srv, 2).await;
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];

    // Stable id, source, title shape.
    assert_eq!(task["id"], "C1:100.2");
    assert_eq!(task["source"], "slack");
    let title = task["title"].as_str().unwrap();
    assert!(
        title.starts_with("Slack: アリス in #dev-frontend:"),
        "{title}"
    );

    // Body: instruction, reply style, mention, and the thread context
    // (mention itself excluded, speaker names resolved).
    let body = task["body"].as_str().unwrap();
    assert!(body.contains("返信案を日本語で作成"), "{body}");
    assert!(body.contains("丁寧語で簡潔に"), "{body}");
    assert!(body.contains("<@U_ME> 原因わかりますか"), "{body}");
    assert!(body.contains("アリス: デプロイが失敗してる"), "{body}");
    assert!(body.contains("アリス: ログ見た?"), "{body}");
    assert!(
        !body.contains("スレッド文脈の取得に失敗"),
        "context lookup must have succeeded: {body}"
    );

    // Single-repo config → resolved hint; permalink → url.
    assert_eq!(task["repo_hint"], "web-app");
    assert_eq!(task["url"], "https://ws.slack.test/archives/C1/p1002");

    // A second fetch never sees the same task again.
    let result = call(&mut srv, 3, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(result["tasks"], json!([]));

    // A redelivery of the same envelope must not create another task.
    send_and_await_ack(&mut ws, mention_envelope("e1-redelivery", "100.2")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let result = call(&mut srv, 4, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(result["tasks"], json!([]), "duplicate must be deduped");

    // A different mention is a different task.
    send_and_await_ack(&mut ws, mention_envelope("e2", "100.9")).await;
    let tasks = fetch_until_tasks(&mut srv, 5).await;
    assert_eq!(tasks[0]["id"], "C1:100.9");
}

#[tokio::test]
async fn enrichment_failures_degrade_the_task_instead_of_dropping_it() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    // Only the connection surface answers; every enrichment lookup fails, so
    // names fall back to raw ids, the body notes the missing context, and
    // the url is absent.
    shared.push_for(
        "auth.test",
        Canned::Data(json!({ "ok": true, "user_id": "U_ME" })),
    );
    shared.push_for(
        "apps.connections.open",
        Canned::Data(json!({ "ok": true, "url": url })),
    );
    shared.push_for(
        "conversations.open",
        Canned::Data(json!({ "ok": true, "channel": { "id": "D_SELF" } })),
    );
    shared.push_for("users.info", Canned::Network);
    shared.push_for("conversations.info", Canned::Network);
    shared.push_for("conversations.replies", Canned::Network);
    shared.push_for("chat.getPermalink", Canned::Network);
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    let tasks = fetch_until_tasks(&mut srv, 2).await;
    let task = &tasks[0];
    assert_eq!(task["id"], "C1:100.2");
    let title = task["title"].as_str().unwrap();
    assert!(title.starts_with("Slack: U_OTHER in #C1:"), "{title}");
    let body = task["body"].as_str().unwrap();
    assert!(body.contains("スレッド文脈の取得に失敗"), "{body}");
    assert!(task["url"].is_null(), "{task}");
}

// ---------------------------------------------------------------------------
// repository resolution (#106)
// ---------------------------------------------------------------------------

/// Two-repo config with a channel rule that does NOT match the test channel,
/// so resolution reaches stage ② (LLM) and, when that is inconclusive, stage
/// ③ (the ephemeral picker).
fn init_params_multi_repo() -> Value {
    json!({
        "protocol_version": "0.1.0",
        "config": {
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "thread_context_limit": 3,
            "llm": {
                "base_url": "https://llm.test/v1",
                "model": "test-model",
                "api_key": "sk-test",
                "confidence_threshold": 0.6
            },
            "channel_groups": [
                { "prefix": "team-b-", "repos": ["design-system"] }
            ],
            "repos": [
                { "name": "web-app", "summary": "customer web app" },
                { "name": "design-system", "summary": "component library" }
            ]
        }
    })
}

fn chat_verdict(repo: &str, confidence: f64) -> Value {
    json!({ "choices": [{ "message": {
        "role": "assistant",
        "content": json!({ "repo": repo, "confidence": confidence, "reason": "t" }).to_string()
    }}]})
}

/// The selection buttons of the last posted ephemeral (from the recorded
/// chat.postEphemeral request).
fn last_ephemeral_buttons(shared: &Shared) -> Vec<Value> {
    let requests = shared.requests();
    let ephemeral = requests
        .iter()
        .rev()
        .find(|r| r.method == "chat.postEphemeral")
        .expect("an ephemeral was posted");
    // The fake transport records the pre-encoding JSON body, so `blocks` is
    // still a JSON array here (production form-encodes it to text).
    let blocks = ephemeral.body.as_ref().unwrap()["blocks"].clone();
    blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "actions")
        .expect("an actions block")["elements"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn confident_llm_verdict_resolves_the_repo_without_asking() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_chat(Ok(chat_verdict("design-system", 0.9)));
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params_multi_repo()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    let tasks = fetch_until_tasks(&mut srv, 2).await;
    assert_eq!(tasks[0]["repo_hint"], "design-system");

    // The classifier saw the mention and both candidates; no ephemeral.
    let chat_requests = shared.chat_requests();
    assert_eq!(chat_requests.len(), 1);
    let user = chat_requests[0]["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("原因わかりますか"), "{user}");
    assert!(user.contains("web-app"), "{user}");
    assert!(user.contains("design-system"), "{user}");
    assert!(
        !shared
            .requests()
            .iter()
            .any(|r| r.method == "chat.postEphemeral"),
        "no picker for a confident verdict"
    );
}

#[tokio::test]
async fn channel_prefix_rule_short_circuits_the_llm() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    // The mention arrives in a channel matching the `team-b-` rule.
    canned_web_api_in_channel(&shared, &url, "team-b-general");
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params_multi_repo()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    let tasks = fetch_until_tasks(&mut srv, 2).await;
    assert_eq!(tasks[0]["repo_hint"], "design-system");
    assert!(
        shared.chat_requests().is_empty(),
        "rule resolved; no LLM call"
    );
}

#[tokio::test]
async fn low_confidence_asks_via_ephemeral_and_the_answer_submits_the_task() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_chat(Ok(chat_verdict("web-app", 0.2)));
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params_multi_repo()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    // Low confidence → no task yet, an ephemeral picker instead.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let result = call(&mut srv, 2, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(
        result["tasks"],
        json!([]),
        "no task while selection is pending"
    );

    let buttons = last_ephemeral_buttons(&shared);
    // One button per candidate plus the skip.
    assert_eq!(buttons.len(), 3, "{buttons:?}");
    let web_app = buttons
        .iter()
        .find(|b| b["text"]["text"] == "web-app")
        .expect("a web-app button");
    let value: Value = serde_json::from_str(web_app["value"].as_str().unwrap()).unwrap();
    assert_eq!(value["task"], "C1:100.2");

    // The operator picks web-app; the task appears with that hint.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope(
            "e2",
            web_app["action_id"].as_str().unwrap(),
            &json!({ "task": "C1:100.2", "repo": "web-app" }).to_string(),
            "C1",
        ),
    )
    .await;
    let tasks = fetch_until_tasks(&mut srv, 3).await;
    assert_eq!(tasks[0]["id"], "C1:100.2");
    assert_eq!(tasks[0]["repo_hint"], "web-app");

    // The ephemeral was rewritten via its response_url.
    let posted = shared.posted_urls();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].url, "https://hooks.slack.test/r/1");
    assert_eq!(posted[0].body["replace_original"], true);
}

#[tokio::test]
async fn skip_discards_the_mention_without_a_task() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_chat(Err("connection refused".into()));
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params_multi_repo()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;

    // API failure → picker. The operator skips.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_and_await_ack(
        &mut ws,
        block_actions_envelope(
            "e2",
            "skip_mention",
            &json!({ "task": "C1:100.2" }).to_string(),
            "C1",
        ),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let result = call(&mut srv, 2, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(
        result["tasks"],
        json!([]),
        "skipped mention never becomes a task"
    );
    // The picker was posted, then rewritten to the skip confirmation.
    assert!(
        shared
            .requests()
            .iter()
            .any(|r| r.method == "chat.postEphemeral")
    );
    assert_eq!(shared.posted_urls().len(), 1);
}

#[tokio::test]
async fn stale_selection_answer_gets_an_expiry_notice() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let mut srv = server(&shared);

    call(&mut srv, 1, "initialize", init_params_multi_repo()).await;
    let mut ws = accept_with_hello(&listener).await;

    // A button press for a mention nobody is waiting on (e.g. after a
    // restart): no task, but the ephemeral is rewritten with the notice.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope(
            "e1",
            "select_repo_0",
            &json!({ "task": "C1:999.9", "repo": "web-app" }).to_string(),
            "C1",
        ),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let result = call(&mut srv, 2, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(result["tasks"], json!([]));
    let posted = shared.posted_urls();
    assert_eq!(posted.len(), 1, "{posted:?}");
    let text = posted[0].body["text"].as_str().unwrap();
    assert!(text.contains("期限切れ"), "{text}");
}
