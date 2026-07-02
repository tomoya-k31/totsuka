//! `serve_uds` graceful shutdown: on cancellation it must stop accepting,
//! let in-flight responses finish (bounded by the drain deadline), and
//! return — even when idle keep-alive connections are still open. spec §5:
//! 新規受付停止 → in-flight を deadline 付きで drain → 即 exit.

use std::time::Duration;

use agent_adapter::listener::{bind_uds, serve_uds};
use axum::routing::get;
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

async fn send_request(stream: &mut UnixStream, path: &str) {
    let req = format!("GET {path} HTTP/1.1\r\nhost: localhost\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
}

/// Read until the peer closes the connection; returns everything received.
async fn read_to_eof(stream: &mut UnixStream) -> String {
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn cancel_returns_promptly_despite_idle_keepalive_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("adapter.sock");
    let listener = bind_uds(&sock).await.unwrap();
    let app = Router::new().route("/ok", get(|| async { "ok" }));

    let shutdown = CancellationToken::new();
    let server = tokio::spawn(serve_uds(
        listener,
        app,
        shutdown.clone(),
        Duration::from_millis(500),
    ));

    // Open a keep-alive connection and complete one request so the
    // connection is established and then sits idle.
    let mut client = UnixStream::connect(&sock).await.unwrap();
    send_request(&mut client, "/ok").await;
    let mut first = [0u8; 512];
    let n = client.read(&mut first).await.unwrap();
    assert!(String::from_utf8_lossy(&first[..n]).contains("200 OK"));

    shutdown.cancel();

    // The server must return within the drain deadline (+ scheduling slack),
    // not hang on the idle connection.
    let joined = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("serve_uds must return promptly after cancel");
    joined.expect("join").expect("serve_uds result");
}

#[tokio::test]
async fn inflight_request_completes_during_drain() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("adapter.sock");
    let listener = bind_uds(&sock).await.unwrap();

    // Handler signals when the request is in flight, then takes a while.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(started_tx)));
    let app = Router::new().route(
        "/slow",
        get(move || {
            let started_tx = started_tx.clone();
            async move {
                if let Some(tx) = started_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                "done"
            }
        }),
    );

    let shutdown = CancellationToken::new();
    let server = tokio::spawn(serve_uds(
        listener,
        app,
        shutdown.clone(),
        Duration::from_secs(2),
    ));

    let mut client = UnixStream::connect(&sock).await.unwrap();
    send_request(&mut client, "/slow").await;

    // Provably ordered: cancel only once the handler is running.
    started_rx.await.unwrap();
    shutdown.cancel();

    // The in-flight response must still arrive, then the connection closes.
    let body = tokio::time::timeout(Duration::from_secs(2), read_to_eof(&mut client))
        .await
        .expect("in-flight response must complete during drain");
    assert!(body.contains("200 OK"), "got: {body}");
    assert!(body.contains("done"), "got: {body}");

    let joined = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("serve_uds must return after drain");
    joined.expect("join").expect("serve_uds result");
}
