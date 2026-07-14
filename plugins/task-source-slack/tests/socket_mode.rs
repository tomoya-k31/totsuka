//! Socket Mode client behavior against a real (local) WebSocket server:
//! immediate envelope acks, event normalization and delivery, reconnection
//! after `disconnect` / dropped connections without event loss, and the fatal
//! App-Level Token path.

mod common;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{WebSocketStream, accept_async};

use common::{Canned, Shared, transport};
use task_source_slack::slack_api::SlackApi;
use task_source_slack::socket_mode::{SocketEvent, SocketModeOptions, spawn};

/// Test-speed reconnect timing.
fn options() -> SocketModeOptions {
    SocketModeOptions {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..SocketModeOptions::default()
    }
}

/// A listener plus the `ws://` URL Socket Mode should be pointed at.
async fn ws_listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// Accept one WebSocket connection and greet it with `hello`.
async fn accept_with_hello(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (socket, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(socket).await.unwrap();
    ws.send(WsMessage::text(
        json!({ "type": "hello", "num_connections": 1 }).to_string(),
    ))
    .await
    .unwrap();
    ws
}

/// Send `envelope` and wait for its ack, returning the acked envelope id.
async fn send_and_await_ack(ws: &mut WebSocketStream<TcpStream>, envelope: Value) -> String {
    ws.send(WsMessage::text(envelope.to_string()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ack within 2s")
        .expect("stream open")
        .expect("readable frame");
    let ack: Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
    ack["envelope_id"]
        .as_str()
        .expect("ack has envelope_id")
        .to_string()
}

fn message_envelope(id: &str, text: &str) -> Value {
    json!({
        "type": "events_api",
        "envelope_id": id,
        "payload": { "event": { "type": "message", "channel": "C1", "text": text, "ts": "1.0" } }
    })
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<SocketEvent>) -> SocketEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event within 5s")
        .expect("channel open")
}

#[tokio::test]
async fn envelopes_are_acked_before_the_consumer_reads_them() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let (mut rx, _handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );
    let mut ws = accept_with_hello(&listener).await;

    // Both acks arrive while nothing has consumed the event channel yet:
    // acking never waits on downstream processing.
    let acked = send_and_await_ack(&mut ws, message_envelope("e1", "hi")).await;
    assert_eq!(acked, "e1");
    let acked = send_and_await_ack(
        &mut ws,
        json!({
            "type": "interactive",
            "envelope_id": "e2",
            "payload": {
                "type": "block_actions",
                "actions": [{ "action_id": "approve_reply", "value": "d-1" }]
            }
        }),
    )
    .await;
    assert_eq!(acked, "e2");

    // Now consume: both events arrive, normalized, in order.
    let SocketEvent::Message(event) = next_event(&mut rx).await else {
        panic!("expected Message first");
    };
    assert_eq!(event["text"], "hi");
    let SocketEvent::BlockActions(payload) = next_event(&mut rx).await else {
        panic!("expected BlockActions second");
    };
    assert_eq!(payload["actions"][0]["action_id"], "approve_reply");
}

#[tokio::test]
async fn non_message_envelopes_are_acked_but_not_delivered() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let (mut rx, _handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );
    let mut ws = accept_with_hello(&listener).await;

    // A reaction event is acked (Slack must not redeliver) yet filtered out.
    send_and_await_ack(
        &mut ws,
        json!({
            "type": "events_api",
            "envelope_id": "e-reaction",
            "payload": { "event": { "type": "reaction_added" } }
        }),
    )
    .await;
    // The next *message* envelope is the first thing the consumer sees.
    send_and_await_ack(&mut ws, message_envelope("e-msg", "after")).await;
    let SocketEvent::Message(event) = next_event(&mut rx).await else {
        panic!("expected Message");
    };
    assert_eq!(event["text"], "after");
}

#[tokio::test]
async fn disconnect_message_reconnects_without_losing_events() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": url.clone() })));
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let (mut rx, _handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );

    // First connection: one event, then Slack asks us to refresh.
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, message_envelope("e1", "before refresh")).await;
    ws.send(WsMessage::text(
        json!({ "type": "disconnect", "reason": "refresh_requested" }).to_string(),
    ))
    .await
    .unwrap();

    // Second connection: the client came back and events keep flowing.
    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, message_envelope("e2", "after refresh")).await;

    let SocketEvent::Message(first) = next_event(&mut rx).await else {
        panic!("expected first Message");
    };
    assert_eq!(first["text"], "before refresh");
    let SocketEvent::Message(second) = next_event(&mut rx).await else {
        panic!("expected second Message");
    };
    assert_eq!(second["text"], "after refresh");

    // apps.connections.open was called once per connection.
    assert_eq!(shared.requests().len(), 2);
}

#[tokio::test]
async fn dropped_connection_and_transport_errors_back_off_then_recover() {
    let (listener, url) = ws_listener().await;
    // Round 1: a URL nobody listens on (connect fails). Round 2: a transport
    // failure on apps.connections.open itself. Round 3: a healthy endpoint.
    let unreachable = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("ws://{}", l.local_addr().unwrap())
        // listener dropped here → connection refused
    };
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": unreachable })));
    shared.push(Canned::Network);
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let (mut rx, _handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );

    let mut ws = accept_with_hello(&listener).await;
    send_and_await_ack(&mut ws, message_envelope("e1", "recovered")).await;
    let SocketEvent::Message(event) = next_event(&mut rx).await else {
        panic!("expected Message");
    };
    assert_eq!(event["text"], "recovered");
    assert_eq!(shared.requests().len(), 3, "one open call per attempt");
}

#[tokio::test]
async fn silent_connection_is_detected_by_the_idle_timeout() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": url.clone() })));
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let mut opts = options();
    opts.idle_timeout = Duration::from_millis(200);
    let (mut rx, _handle) = spawn(std::sync::Arc::new(SlackApi::new(transport(&shared))), opts);

    // First connection goes silent after hello (dead TCP path simulation:
    // the server just never writes again).
    let ws_silent = accept_with_hello(&listener).await;

    // The client must give up on the quiet session and reconnect.
    let mut ws = accept_with_hello(&listener).await;
    drop(ws_silent);
    send_and_await_ack(&mut ws, message_envelope("e1", "after silence")).await;
    let SocketEvent::Message(event) = next_event(&mut rx).await else {
        panic!("expected Message");
    };
    assert_eq!(event["text"], "after silence");
}

#[tokio::test]
async fn dropping_the_receiver_stops_an_idle_session() {
    let (listener, url) = ws_listener().await;
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "url": url })));

    let (rx, handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );
    let _ws = accept_with_hello(&listener).await;

    // No traffic at all: the loop is parked reading. Dropping the receiver
    // must still end the task (shutdown), not hang forever.
    drop(rx);
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("loop stops on receiver drop")
        .expect("task not panicked");
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn permanent_configuration_errors_stop_the_loop() {
    // missing_scope / not_allowed_token_type never fix themselves: the loop
    // must fail fast with guidance instead of retrying forever.
    for code in ["missing_scope", "not_allowed_token_type"] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({ "ok": false, "error": code })));

        let (_rx, handle) = spawn(
            std::sync::Arc::new(SlackApi::new(transport(&shared))),
            options(),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("loop stops")
            .expect("task not panicked");
        let err = result.expect_err("permanent config failure is fatal");
        assert!(err.to_string().contains(code), "{err}");
        assert_eq!(shared.requests().len(), 1, "{code}: no retries");
    }
}

#[tokio::test]
async fn bad_app_token_stops_the_loop_with_guidance() {
    let shared = Shared::default();
    shared.push(Canned::Data(
        json!({ "ok": false, "error": "invalid_auth" }),
    ));

    let (mut rx, handle) = spawn(
        std::sync::Arc::new(SlackApi::new(transport(&shared))),
        options(),
    );
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("loop stops")
        .expect("task not panicked");
    let err = result.expect_err("credential failure is fatal");
    let message = err.to_string();
    assert!(message.contains("App-Level Token"), "{message}");
    assert!(message.contains("xapp-"), "{message}");

    // The event channel closed with the loop.
    assert!(rx.recv().await.is_none());
}
