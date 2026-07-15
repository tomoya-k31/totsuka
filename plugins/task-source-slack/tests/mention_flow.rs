//! The full mention path over the JSON-RPC boundary with the resident
//! runtime ON: a local WebSocket mock plays Slack's Socket Mode side, the
//! keyed recorded transport plays the Web API, and the test drives
//! `initialize` → mention envelope → `tasks/fetch` (issue #105 acceptance).

mod common;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

use common::{Canned, FakeFactory, Shared};
use plugin_protocol::jsonrpc::Response;
use task_source_slack::server::Server;

fn server(shared: &Shared) -> Server<FakeFactory> {
    Server::new(FakeFactory {
        shared: shared.clone(),
    })
}

async fn call(srv: &mut Server<FakeFactory>, id: i64, method: &str, params: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    let response: Response =
        serde_json::from_str(&reply.line.expect("a response line")).expect("valid response");
    if let Some(error) = &response.error {
        panic!("{method} failed: {}", error.message);
    }
    response.result.unwrap_or(Value::Null)
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

async fn ws_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

async fn accept_with_hello(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (socket, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(socket).await.unwrap();
    ws.send(WsMessage::text(json!({ "type": "hello" }).to_string()))
        .await
        .unwrap();
    ws
}

async fn send_and_await_ack(ws: &mut WebSocketStream<TcpStream>, envelope: Value) {
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

fn mention_envelope(envelope_id: &str, ts: &str) -> Value {
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

/// Poll `tasks/fetch` until it yields tasks (the pipeline is asynchronous).
async fn fetch_until_tasks(srv: &mut Server<FakeFactory>, id: i64) -> Vec<Value> {
    for _ in 0..100 {
        let result = call(srv, id, "tasks/fetch", json!({ "trigger": {} })).await;
        let tasks = result["tasks"].as_array().unwrap().clone();
        if !tasks.is_empty() {
            return tasks;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no task showed up within 5s");
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
