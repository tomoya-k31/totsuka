//! The `:eyes:` reaction trigger end to end (#319): a reaction the operator
//! adds becomes the same task a mention would, sharing one dedup set with the
//! mention path, and everyone else's reactions are ignored.

mod common;

use std::time::Duration;

use serde_json::{Value, json};

use common::{
    Canned, FakeFactory, LookupHarness, Shared, SubmitHarness, accept_with_hello, call,
    mention_envelope_in, scratch_state_dir, send_and_await_ack, ws_listener,
};
use task_source_slack::server::Server;

fn server(shared: &Shared) -> (Server<FakeFactory>, SubmitHarness) {
    let harness = SubmitHarness::new();
    let srv = Server::new(
        FakeFactory {
            shared: shared.clone(),
        },
        harness.client.clone(),
        LookupHarness::new().client.clone(),
    );
    (srv, harness)
}

/// Like the mention-flow config, plus the opt-in trigger set. `":eyes:"` is
/// written with colons on purpose: config may spell it either way.
fn init_params() -> Value {
    json!({
        "protocol_version": "0.1.0",
        "config": {
            "state_dir": scratch_state_dir(),
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "thread_context_limit": 3,
            "trigger_reactions": [":eyes:"],
            "repos": [{ "name": "web-app", "summary": "customer web app" }]
        }
    })
}

/// A `reaction_added` envelope from `user` on the message at `ts`.
fn reaction_envelope(envelope_id: &str, user: &str, reaction: &str, ts: &str) -> Value {
    json!({
        "type": "events_api",
        "envelope_id": envelope_id,
        "payload": { "event": {
            "type": "reaction_added",
            "user": user,
            "reaction": reaction,
            "item": { "type": "message", "channel": "C1", "ts": ts },
            "item_user": "U_OTHER",
            "event_ts": "900.0"
        }}
    })
}

fn canned_web_api(shared: &Shared, ws_url: &str) {
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
    // The reacted-to message, re-fetched from `item.channel` + `item.ts`.
    // Note it carries **no** `<@U_ME>` tag: that is the whole point of the
    // reaction trigger.
    shared.push_for(
        "conversations.history",
        Canned::Data(json!({
            "ok": true,
            "messages": [
                { "user": "U_OTHER", "text": "デプロイが失敗してる", "ts": "100.0" }
            ]
        })),
    );
    shared.push_for(
        "conversations.replies",
        Canned::Data(json!({
            "ok": true,
            "messages": [
                { "user": "U_OTHER", "text": "デプロイが失敗してる", "ts": "100.0", "thread_ts": "100.0" },
            ]
        })),
    );
    shared.push_for(
        "chat.getPermalink",
        Canned::Data(json!({
            "ok": true,
            "permalink": "https://ws.slack.test/archives/C1/p1000"
        })),
    );
    shared.push_for(
        "chat.postEphemeral",
        Canned::Data(json!({ "ok": true, "message_ts": "9.1" })),
    );
}

#[tokio::test]
async fn the_operators_reaction_becomes_a_task() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let (mut srv, mut harness) = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, reaction_envelope("e1", "U_ME", "eyes", "100.0")).await;

    let task = harness.next_task().await;
    // Same identity a mention on this message would produce — the two
    // triggers converge rather than opening parallel worlds.
    assert_eq!(task["id"], "C1:100.0");
    assert_eq!(task["message_key"], "C1:100.0");
    assert_eq!(task["source"], "slack");
    let body = task["body"].as_str().unwrap();
    assert!(body.contains("デプロイが失敗してる"), "{body}");
    // The task's sender is the message's author, not the reacting operator.
    let title = task["title"].as_str().unwrap();
    assert!(
        title.starts_with("Slack: アリス in #dev-frontend:"),
        "{title}"
    );
    assert_eq!(task["repo_hint"], "web-app");

    // A redelivery of the same reaction must not submit a second task.
    send_and_await_ack(
        &mut ws,
        reaction_envelope("e1-redelivery", "U_ME", "eyes", "100.0"),
    )
    .await;
    harness.assert_no_task(Duration::from_millis(300)).await;
}

/// **The regression guard for the feature's safety story. Do not delete.**
/// Accepting a colleague's reaction would let them start work on the
/// operator's machine with an emoji.
#[tokio::test]
async fn another_users_reaction_submits_nothing() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let (mut srv, mut harness) = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(
        &mut ws,
        reaction_envelope("e1", "U_SOMEONE_ELSE", "eyes", "100.0"),
    )
    .await;

    harness.assert_no_task(Duration::from_millis(300)).await;
    // Not even the message lookup runs: the check is on the event, before
    // anything is spent on it.
    assert!(
        !shared
            .requests()
            .iter()
            .any(|r| r.method == "conversations.history"),
        "a rejected reaction must not cost an API call"
    );
}

#[tokio::test]
async fn an_emoji_outside_the_trigger_set_submits_nothing() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let (mut srv, mut harness) = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, reaction_envelope("e1", "U_ME", "tada", "100.0")).await;
    harness.assert_no_task(Duration::from_millis(300)).await;
}

/// The dedup set is shared: one message reached by both triggers is one task.
#[tokio::test]
async fn a_mention_and_a_reaction_on_one_message_make_one_task() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    canned_web_api(&shared, &url);
    let (mut srv, mut harness) = server(&shared);

    call(&mut srv, 1, "initialize", init_params()).await;
    let mut ws = accept_with_hello(&listener).await;

    // The mention arrives first and submits.
    send_and_await_ack(&mut ws, mention_envelope_in("e1", "100.0", None)).await;
    let task = harness.next_task().await;
    assert_eq!(task["id"], "C1:100.0");

    // Reacting to the same message afterwards adds nothing.
    send_and_await_ack(&mut ws, reaction_envelope("e2", "U_ME", "eyes", "100.0")).await;
    harness.assert_no_task(Duration::from_millis(300)).await;
}
