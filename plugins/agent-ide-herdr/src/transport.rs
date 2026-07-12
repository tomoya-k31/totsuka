//! herdr Socket API transport: the seam between the plugin's adapter logic and
//! the herdr Unix socket.
//!
//! herdr speaks **NDJSON** (one JSON message per line), **not** JSON-RPC:
//! requests carry an `id`, responses echo it with `result`/`error`, and after an
//! `events.subscribe` the same connection is pushed unsolicited event messages.
//! [`SocketTransport`] multiplexes all three over one connection: a reader task
//! routes id-correlated responses to waiting [`call`](HerdrTransport::call)s and
//! everything else to a broadcast [`events`](HerdrTransport::events) stream.
//!
//! [`HerdrAgent`](crate::agent::HerdrAgent) is generic over [`HerdrTransport`]
//! so the adapter is tested against a fake without a real herdr.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, broadcast, oneshot};

use crate::error::HerdrError;

/// Capacity of the herdr event broadcast. Slow subscribers that lag beyond this
/// drop the oldest events (herdr state is re-derivable via `session.snapshot`).
const EVENT_BUFFER: usize = 256;

/// A herdr Socket API client.
pub trait HerdrTransport: Clone + Send + Sync + 'static {
    /// Send a request (`{id, method, params}`) and await its correlated
    /// response, returning the `result` payload.
    fn call(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, HerdrError>> + Send;

    /// Send an `events.subscribe` request for `subscriptions` (an array of
    /// `{type, pane_id, …}`). Events then arrive on [`events`](Self::events).
    fn subscribe_events(
        &self,
        subscriptions: Value,
    ) -> impl Future<Output = Result<(), HerdrError>> + Send;

    /// A receiver for the pushed herdr event stream (all subscriptions share one
    /// connection; consumers filter by `pane_id`).
    fn events(&self) -> broadcast::Receiver<Value>;
}

/// The production transport: an NDJSON client over the herdr Unix socket.
#[derive(Clone)]
pub struct SocketTransport {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<OwnedWriteHalf>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, HerdrError>>>>,
    events: broadcast::Sender<Value>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl SocketTransport {
    /// Connect to the herdr socket at `path`, spawning the reader task.
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, HerdrError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| HerdrError::Connect {
                path: path.display().to_string(),
                source,
            })?;
        let (read_half, write_half) = stream.into_split();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let inner = Arc::new(Inner {
            writer: Mutex::new(write_half),
            pending: Mutex::new(HashMap::new()),
            events: events.clone(),
            next_id: AtomicU64::new(1),
            timeout,
        });
        spawn_reader(read_half, inner.clone());
        Ok(Self { inner })
    }

    async fn write_line(&self, value: &Value) -> Result<(), HerdrError> {
        let line = serde_json::to_string(value).map_err(|e| HerdrError::Io(e.to_string()))?;
        let mut writer = self.inner.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| HerdrError::Io(e.to_string()))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| HerdrError::Io(e.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|e| HerdrError::Io(e.to_string()))
    }
}

impl HerdrTransport for SocketTransport {
    async fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        let id = format!("req_{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id.clone(), tx);

        let request = json!({ "id": id, "method": method, "params": params });
        if let Err(e) = self.write_line(&request).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(self.inner.timeout, rx).await {
            Ok(Ok(result)) => result,
            // Sender dropped without replying → the connection closed.
            Ok(Err(_)) => Err(HerdrError::Io("herdr connection closed".into())),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(HerdrError::Timeout(method.to_string()))
            }
        }
    }

    async fn subscribe_events(&self, subscriptions: Value) -> Result<(), HerdrError> {
        self.call(
            "events.subscribe",
            json!({ "subscriptions": subscriptions }),
        )
        .await
        .map(|_ack| ())
    }

    fn events(&self) -> broadcast::Receiver<Value> {
        self.inner.events.subscribe()
    }
}

/// Reader task: parse NDJSON from herdr, routing `id`-correlated responses to
/// waiting calls and everything else (events) to the broadcast. On EOF it fails
/// all pending calls so they surface a closed connection rather than hang.
fn spawn_reader(read_half: tokio::net::unix::OwnedReadHalf, inner: Arc<Inner>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(read_half).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break, // EOF: herdr closed the socket
                // A single unreadable line (e.g. invalid UTF-8) must not tear
                // down the whole connection; skip it and keep serving.
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unreadable line from herdr");
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                tracing::warn!("ignoring non-JSON line from herdr");
                continue;
            };
            // A response echoes the request `id` and carries `result`/`error`;
            // anything else is a pushed event.
            let is_response = value.get("id").and_then(Value::as_str).is_some()
                && (value.get("result").is_some() || value.get("error").is_some());
            if is_response {
                deliver_response(&inner, value).await;
            } else {
                let _ = inner.events.send(value);
            }
        }
        // Socket closed: fail every pending call.
        let mut pending = inner.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(HerdrError::Io("herdr closed the connection".into())));
        }
    });
}

/// Resolve a response to its waiting [`call`](HerdrTransport::call).
async fn deliver_response(inner: &Arc<Inner>, value: Value) {
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(tx) = inner.pending.lock().await.remove(id) else {
        return; // no waiter (already timed out): drop it
    };
    let outcome = if let Some(err) = value.get("error") {
        let code = err
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string();
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Err(HerdrError::Protocol { code, message })
    } else {
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    };
    let _ = tx.send(outcome);
}
