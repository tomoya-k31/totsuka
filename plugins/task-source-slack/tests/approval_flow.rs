//! The approval flow over the JSON-RPC boundary with the resident runtime ON
//! (issue #107 acceptance): `result/publish` presents the draft (in-thread
//! ephemeral + self-DM record), the approve/reject `block_actions` finish
//! it, and only an approval posts the reply — under the operator's own name.

mod common;

use std::time::Duration;

use serde_json::{Value, json};

use common::{
    Canned, FakeFactory, Recorded, Shared, SubmitHarness, accept_with_hello,
    block_actions_envelope, call, mention_envelope, send_and_await_ack, wait_until, ws_listener,
};
use task_source_slack::server::Server;

fn server(shared: &Shared) -> (Server<FakeFactory>, SubmitHarness) {
    let harness = SubmitHarness::new();
    let srv = Server::new(
        FakeFactory {
            shared: shared.clone(),
        },
        harness.client.clone(),
    );
    (srv, harness)
}

/// One-repo config: repository resolution short-circuits, so the only
/// ephemeral in these tests is the draft presentation.
fn init_params() -> Value {
    json!({
        "protocol_version": "0.1.0",
        "config": {
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "thread_context_limit": 3,
            "repos": [{ "name": "web-app" }]
        }
    })
}

/// Canned Web API surface up to the draft presentation. `chat.postMessage`
/// is NOT canned here — each test queues its own sequence (DM record, reply).
fn canned_web_api(shared: &Shared, ws_url: &str) {
    canned_web_api_without_ephemeral(shared, ws_url);
    shared.push_for(
        "chat.postEphemeral",
        Canned::Data(json!({ "ok": true, "message_ts": "9.1" })),
    );
}

/// Like [`canned_web_api`] but without a `chat.postEphemeral` answer, so a
/// test can make that surface fail.
fn canned_web_api_without_ephemeral(shared: &Shared, ws_url: &str) {
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
        Canned::Data(json!({ "ok": true, "channel": { "name": "dev-frontend" } })),
    );
    shared.push_for(
        "conversations.replies",
        Canned::Data(json!({ "ok": true, "messages": [] })),
    );
    shared.push_for(
        "chat.getPermalink",
        Canned::Data(json!({
            "ok": true,
            "permalink": "https://ws.slack.test/archives/C1/p1002"
        })),
    );
    shared.push_for("chat.update", Canned::Data(json!({ "ok": true })));
}

/// Published agent content: the actual reply wrapped in log-ish noise, which
/// `result/publish` must trim away.
const PUBLISHED_CONTENT: &str = "\
2026-07-15T10:00:00Z agent session started
[INFO] repository cloned
調査しました。原因は環境変数 FOO の欠落です。
`.env` に FOO を追加してください。
DEBUG: shutting down
";

/// The reply text expected out of [`PUBLISHED_CONTENT`].
const EXPECTED_REPLY: &str =
    "調査しました。原因は環境変数 FOO の欠落です。\n`.env` に FOO を追加してください。";

/// [`EXPECTED_REPLY`] as actually posted: mechanically prefixed with a
/// mention of the asker (`U_OTHER`, per [`mention_envelope`]).
fn expected_posted_reply() -> String {
    format!("<@U_OTHER> {EXPECTED_REPLY}")
}

/// Drive initialize → mention → submit → result/publish, returning the
/// server (kept alive: dropping it aborts the runtime).
async fn publish_draft_flow(
    shared: &Shared,
    listener: &tokio::net::TcpListener,
) -> (
    Server<FakeFactory>,
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) {
    let (mut srv, mut harness) = server(shared);
    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;
    let task = harness.next_task().await;
    assert_eq!(task["id"], "C1:100.2");

    call(
        &mut srv,
        3,
        "result/publish",
        json!({ "task_id": "C1:100.2", "content": PUBLISHED_CONTENT, "format": "markdown" }),
    )
    .await;
    (srv, ws)
}

/// The recorded requests for one Web API `method`.
fn requests_for(shared: &Shared, method: &str) -> Vec<Recorded> {
    shared
        .requests()
        .into_iter()
        .filter(|r| r.method == method)
        .collect()
}

/// The approve/reject buttons of the draft presentation, from the recorded
/// ephemeral: `(draft_id, approve button, reject button)`.
fn draft_buttons(shared: &Shared) -> (String, Value, Value) {
    let ephemerals = requests_for(shared, "chat.postEphemeral");
    let blocks = ephemerals
        .last()
        .expect("a draft ephemeral was posted")
        .body
        .as_ref()
        .unwrap()["blocks"]
        .clone();
    let elements = blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "actions")
        .expect("an actions block")["elements"]
        .as_array()
        .unwrap()
        .clone();
    let approve = elements
        .iter()
        .find(|b| b["action_id"] == "approve_reply")
        .expect("an approve button")
        .clone();
    let reject = elements
        .iter()
        .find(|b| b["action_id"] == "reject_reply")
        .expect("a reject button")
        .clone();
    let draft_id = approve["value"].as_str().unwrap().to_string();
    (draft_id, approve, reject)
}

#[tokio::test]
async fn publish_presents_the_draft_in_thread_and_self_dm() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    let (_srv, _ws) = publish_draft_flow(&shared, &listener).await;

    // Surface 1: the operator-only ephemeral inside the mention's thread.
    let ephemerals = requests_for(&shared, "chat.postEphemeral");
    assert_eq!(ephemerals.len(), 1);
    let body = ephemerals[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["user"], "U_ME");
    assert_eq!(body["thread_ts"], "100.0");
    let blocks_text = body["blocks"].to_string();
    assert!(blocks_text.contains(EXPECTED_REPLY.lines().next().unwrap()));
    assert!(
        !blocks_text.contains("agent session started"),
        "log noise must be trimmed: {blocks_text}"
    );
    assert!(blocks_text.contains("アリス"), "{blocks_text}");
    assert!(
        blocks_text.contains("https://ws.slack.test/archives/C1/p1002"),
        "{blocks_text}"
    );

    // Both buttons carry the same draft id; approve requires confirmation.
    let (draft_id, approve, reject) = draft_buttons(&shared);
    assert!(!draft_id.is_empty());
    assert_eq!(reject["value"], json!(draft_id));
    assert!(approve["confirm"].is_object(), "{approve}");

    // Surface 2: the self-DM record, unfurling off.
    let messages = requests_for(&shared, "chat.postMessage");
    assert_eq!(messages.len(), 1);
    let body = messages[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "D_SELF");
    assert_eq!(body["unfurl_links"], false);
    assert!(body["blocks"].to_string().contains(&draft_id));
}

#[tokio::test]
async fn approve_posts_the_reply_and_finalizes_both_views_once() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    // First postMessage: the self-DM record. Then (sticky): the reply.
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "777.7" })),
    );
    let (mut srv, mut ws) = publish_draft_flow(&shared, &listener).await;
    let (draft_id, ..) = draft_buttons(&shared);

    // Approve from the in-thread ephemeral.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e2", "approve_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the approved reply post", || {
        requests_for(&shared, "chat.postMessage").len() == 2
    })
    .await;

    // The reply went to the mention's thread, as plain text, verbatim.
    let reply = &requests_for(&shared, "chat.postMessage")[1];
    let body = reply.body.as_ref().unwrap();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["thread_ts"], "100.0");
    assert_eq!(body["text"], expected_posted_reply());
    assert!(body["blocks"].is_null(), "{body}");

    // The pressed in-thread ephemeral was deleted outright…
    wait_until("the ephemeral deletion + record update", || {
        !shared.posted_urls().is_empty() && !requests_for(&shared, "chat.update").is_empty()
    })
    .await;
    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["delete_original"], true);
    // …and the self-DM record was updated in place (carrying the ✅ evidence).
    let updates = requests_for(&shared, "chat.update");
    let body = updates[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "D_SELF");
    assert_eq!(body["ts"], "555.1");
    assert!(body["blocks"].to_string().contains("送信済み"));

    // The posted auto-reply comes back as a message event from U_ME and must
    // NOT become a new task (loop break, #105 filter row 2).
    send_and_await_ack(
        &mut ws,
        json!({
            "type": "events_api",
            "envelope_id": "e3",
            "payload": { "event": {
                "type": "message",
                "channel": "C1",
                "user": "U_ME",
                "text": EXPECTED_REPLY,
                "ts": "200.1",
                "thread_ts": "100.0"
            }}
        }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let result = call(&mut srv, 4, "tasks/fetch", json!({ "trigger": {} })).await;
    assert_eq!(result["tasks"], json!([]), "fetch stays an empty stub");

    // A second press is the double-send guard: a "handled" notice, no
    // second chat.postMessage.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e4", "approve_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the already-handled notice", || {
        shared.posted_urls().len() >= 2
    })
    .await;
    let posted = shared.posted_urls();
    let notice = &posted.last().unwrap().body;
    assert_eq!(notice["replace_original"], false);
    assert!(
        notice["text"].as_str().unwrap().contains("処理済み"),
        "{notice}"
    );
    assert_eq!(
        requests_for(&shared, "chat.postMessage").len(),
        2,
        "no double send"
    );
}

#[tokio::test]
async fn reject_finalizes_without_sending() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    let (_srv, mut ws) = publish_draft_flow(&shared, &listener).await;
    let (draft_id, ..) = draft_buttons(&shared);

    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e2", "reject_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the final view rewrites", || {
        !shared.posted_urls().is_empty() && !requests_for(&shared, "chat.update").is_empty()
    })
    .await;

    // Nothing was posted beyond the self-DM record.
    assert_eq!(requests_for(&shared, "chat.postMessage").len(), 1);
    // The in-thread ephemeral was deleted; the ❌ evidence lives on the DM record.
    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["delete_original"], true);
    let updates = requests_for(&shared, "chat.update");
    assert!(
        updates[0].body.as_ref().unwrap()["blocks"]
            .to_string()
            .contains("却下済み")
    );
}

#[tokio::test]
async fn send_failure_keeps_the_draft_retryable() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    // DM record → reply attempt 1 fails (archived) → retry succeeds.
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": false, "error": "is_archived" })),
    );
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "777.7" })),
    );
    let (_srv, mut ws) = publish_draft_flow(&shared, &listener).await;
    let (draft_id, ..) = draft_buttons(&shared);

    // First approve: the send fails → an error notice, the draft stays
    // pending, and nothing is finalized.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e2", "approve_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the send-failure notice", || {
        !shared.posted_urls().is_empty()
    })
    .await;
    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["replace_original"], false);
    let text = posted[0].body["text"].as_str().unwrap();
    assert!(text.contains("失敗"), "{text}");
    assert!(
        requests_for(&shared, "chat.update").is_empty(),
        "no finalize on failure"
    );

    // Second press: the retry goes through and finalizes.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e3", "approve_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the retried reply post", || {
        requests_for(&shared, "chat.postMessage").len() == 3
            && !requests_for(&shared, "chat.update").is_empty()
    })
    .await;
    let reply = &requests_for(&shared, "chat.postMessage")[2];
    assert_eq!(
        reply.body.as_ref().unwrap()["text"],
        expected_posted_reply()
    );
}

#[tokio::test]
async fn stale_button_press_gets_an_expiry_notice() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let (mut srv, _harness) = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;

    // A press whose draft nobody knows (e.g. after a restart).
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e1", "approve_reply", "ffff-1", "C1"),
    )
    .await;
    wait_until("the expiry notice", || !shared.posted_urls().is_empty()).await;

    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["replace_original"], false);
    let text = posted[0].body["text"].as_str().unwrap();
    assert!(text.contains("期限切れ"), "{text}");
    assert!(
        requests_for(&shared, "chat.postMessage").is_empty(),
        "nothing is ever sent for a stale press"
    );
}

#[tokio::test]
async fn press_from_the_self_dm_record_skips_the_redundant_update() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "777.7" })),
    );
    let (_srv, mut ws) = publish_draft_flow(&shared, &listener).await;
    let (draft_id, ..) = draft_buttons(&shared);

    // Approve from the self-DM record: its response_url rewrite already
    // covers the record, so no chat.update on top.
    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e2", "approve_reply", &draft_id, "D_SELF"),
    )
    .await;
    wait_until("the final view rewrite", || {
        !shared.posted_urls().is_empty()
    })
    .await;

    assert_eq!(requests_for(&shared, "chat.postMessage").len(), 2);
    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["replace_original"], true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        requests_for(&shared, "chat.update").is_empty(),
        "the DM press must not also chat.update the same message"
    );
}

#[tokio::test]
async fn press_replaces_the_ephemeral_when_no_dm_record_exists() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    // The self-DM record fails to post → the in-thread ephemeral is the ONLY
    // surface carrying the outcome, so a press must not erase it.
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": false, "error": "channel_not_found" })),
    );
    let (_srv, mut ws) = publish_draft_flow(&shared, &listener).await;
    let (draft_id, ..) = draft_buttons(&shared);

    send_and_await_ack(
        &mut ws,
        block_actions_envelope("e2", "reject_reply", &draft_id, "C1"),
    )
    .await;
    wait_until("the sole-surface finalization", || {
        !shared.posted_urls().is_empty()
    })
    .await;

    // With no durable record, the ephemeral is replaced in place (❌ visible),
    // not deleted — otherwise the rejection would leave no trace anywhere.
    let posted = shared.posted_urls();
    assert_eq!(posted[0].body["replace_original"], true);
    assert!(posted[0].body.get("delete_original").is_none());
    assert!(posted[0].body["blocks"].to_string().contains("却下済み"));
    // No self-DM record existed, so nothing was chat.update'd.
    assert!(requests_for(&shared, "chat.update").is_empty());
}

#[tokio::test]
async fn one_failed_presentation_surface_is_tolerated() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    // The thread ephemeral fails; the self-DM record must still go out and
    // result/publish must still succeed.
    canned_web_api_without_ephemeral(&shared, &url);
    shared.push_for(
        "chat.postEphemeral",
        Canned::Data(json!({ "ok": false, "error": "channel_not_found" })),
    );
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    let (_srv, _ws) = publish_draft_flow(&shared, &listener).await;

    let messages = requests_for(&shared, "chat.postMessage");
    assert_eq!(messages.len(), 1);
    let body = messages[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "D_SELF");
    assert!(
        body["blocks"].to_string().contains("approve_reply"),
        "the record still carries the buttons"
    );
}

#[tokio::test]
async fn empty_publish_fails_without_consuming_the_pending_entry() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    shared.push_for(
        "chat.postMessage",
        Canned::Data(json!({ "ok": true, "ts": "555.1" })),
    );
    let (mut srv, mut harness) = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, mention_envelope("e1", "100.2")).await;
    harness.next_task().await;

    // An empty result is rejected…
    let line = json!({
        "jsonrpc": "2.0", "id": 3, "method": "result/publish",
        "params": { "task_id": "C1:100.2", "content": "   \n\n" }
    });
    let reply = srv.handle_line(&line.to_string()).await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("empty"),
        "{response}"
    );

    // …but must NOT have consumed the coordinates: a retry with real
    // content still presents the draft.
    call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "C1:100.2", "content": PUBLISHED_CONTENT }),
    )
    .await;
    assert_eq!(requests_for(&shared, "chat.postEphemeral").len(), 1);
}
