//! herdr Socket API transport: the seam between the plugin's adapter logic and
//! the herdr Unix socket.
//!
//! herdr speaks **NDJSON** (one JSON message per line), **not** JSON-RPC:
//! requests carry a string `id`, responses echo it with `result`/`error`.
//! The connection model (verified against herdr 0.7.1/0.7.4, #124) is
//! **one request per connection**: the server closes the socket after every
//! response, so [`SocketTransport`] opens a fresh connection per
//! [`call`](HerdrTransport::call). The single exception is `events.subscribe`,
//! which keeps its connection open and pushes `{event, data}` envelope lines;
//! [`subscribe_events`](HerdrTransport::subscribe_events) holds that dedicated
//! connection and forwards pushed events to the broadcast
//! [`events`](HerdrTransport::events) stream.
//!
//! Decode failures are answered with an error whose `id` is `""` (herdr does
//! not echo the request id) — with one request in flight per connection that
//! error is unambiguously ours, so `call` accepts it.
//!
//! [`HerdrAgent`](crate::agent::HerdrAgent) is generic over [`HerdrTransport`]
//! so the adapter is tested against a fake without a real herdr.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast;

use crate::error::HerdrError;

/// Capacity of the herdr event broadcast. Slow subscribers that lag beyond this
/// drop the oldest events (state is re-derived via `pane.get` on lag).
const EVENT_BUFFER: usize = 256;

/// Synthetic event broadcast when an `events.subscribe` connection closes, so
/// the state streams behind it can resync instead of waiting forever for events
/// that will never arrive. It is emitted once **per subscribed pane**, shaped
/// like a herdr envelope (`{event, data: {pane_id}}`), because the broadcast is
/// shared: only the panes on the dead connection may act on it.
pub const SUBSCRIPTION_CLOSED_EVENT: &str = "__herdr_subscription_closed";

/// A herdr Socket API client.
pub trait HerdrTransport: Clone + Send + Sync + 'static {
    /// Send a request (`{id, method, params}`) and await its response,
    /// returning the `result` payload.
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

    /// A receiver for the pushed herdr event stream (all subscriptions share
    /// one broadcast; consumers filter by `pane_id`).
    fn events(&self) -> broadcast::Receiver<Value>;
}

/// The production transport: an NDJSON client over the herdr Unix socket,
/// opening one connection per request (see the module docs for why).
#[derive(Clone)]
pub struct SocketTransport {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    timeout: Duration,
    events: broadcast::Sender<Value>,
    next_id: AtomicU64,
}

impl SocketTransport {
    /// Build a transport for the herdr socket at `path` and verify it is
    /// reachable with a one-shot `ping` (there is no persistent connection to
    /// hold, so the probe is the only meaningful "connect").
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, HerdrError> {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let transport = Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                timeout,
                events,
                next_id: AtomicU64::new(1),
            }),
        };
        transport.call("ping", json!({})).await?;
        Ok(transport)
    }

    fn next_id(&self) -> String {
        format!("req_{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn open(&self) -> Result<UnixStream, HerdrError> {
        UnixStream::connect(&self.inner.path)
            .await
            .map_err(|source| HerdrError::Connect {
                path: self.inner.path.display().to_string(),
                source,
            })
    }
}

impl HerdrTransport for SocketTransport {
    async fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        let id = self.next_id();
        let request = json!({ "id": id, "method": method, "params": params });
        let exchange = async {
            let stream = self.open().await?;
            let (read_half, mut write_half) = stream.into_split();
            write_line(&mut write_half, &request).await?;
            let mut lines = BufReader::new(read_half).lines();
            loop {
                let line = lines
                    .next_line()
                    .await
                    .map_err(|e| HerdrError::Io(e.to_string()))?
                    .ok_or_else(|| {
                        HerdrError::Io("herdr closed the connection without responding".into())
                    })?;
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    tracing::warn!("ignoring non-JSON line from herdr");
                    continue;
                };
                let response_id = value.get("id").and_then(Value::as_str);
                // One request in flight on this connection: our echoed id, or
                // an error with an empty id (herdr does not echo the id when
                // it fails to decode the request).
                let is_ours = response_id == Some(id.as_str())
                    || (response_id == Some("") && value.get("error").is_some());
                if is_ours {
                    return response_outcome(value);
                }
                tracing::warn!(method, "skipping an uncorrelated line from herdr");
            }
        };
        match tokio::time::timeout(self.inner.timeout, exchange).await {
            Ok(outcome) => outcome,
            Err(_) => Err(HerdrError::Timeout(method.to_string())),
        }
    }

    async fn subscribe_events(&self, subscriptions: Value) -> Result<(), HerdrError> {
        let id = self.next_id();
        // Remembered for the close notice: only these panes are affected when
        // this connection dies, and the broadcast is shared with every other
        // subscription in the process.
        let panes = subscribed_panes(&subscriptions);
        let request = json!({
            "id": id,
            "method": "events.subscribe",
            "params": { "subscriptions": subscriptions },
        });
        let stream = self.open().await?;
        let (read_half, mut write_half) = stream.into_split();
        write_line(&mut write_half, &request).await?;
        let mut lines = BufReader::new(read_half).lines();
        // The first line is the ACK; herdr then keeps this connection open and
        // pushes event envelopes on it.
        let ack = tokio::time::timeout(self.inner.timeout, lines.next_line())
            .await
            .map_err(|_| HerdrError::Timeout("events.subscribe".to_string()))
            .and_then(|r| r.map_err(|e| HerdrError::Io(e.to_string())))?
            .ok_or_else(|| HerdrError::Io("herdr closed the subscription before ACK".into()))?;
        let ack: Value =
            serde_json::from_str(&ack).map_err(|e| HerdrError::InvalidResponse(e.to_string()))?;
        response_outcome(ack)?;

        let events = self.inner.events.clone();
        // Keep the write half alive with the reader: dropping it would
        // half-close the socket and herdr may tear the subscription down.
        tokio::spawn(async move {
            let _write_half = write_half;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let Ok(value) = serde_json::from_str::<Value>(&line) else {
                            tracing::warn!("ignoring a non-JSON event line from herdr");
                            continue;
                        };
                        let _ = events.send(value);
                    }
                    // A single unreadable line (e.g. invalid UTF-8) must not
                    // tear down the subscription; skip it and keep reading.
                    Err(e) => {
                        tracing::warn!(error = %e, "skipping an unreadable event line from herdr");
                        continue;
                    }
                    // EOF: herdr closed the subscription. Tell this
                    // connection's panes so they resync rather than wait
                    // forever — and only them.
                    Ok(None) => {
                        for pane_id in &panes {
                            let _ = events.send(json!({
                                "event": SUBSCRIPTION_CLOSED_EVENT,
                                "data": { "pane_id": pane_id },
                            }));
                        }
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    fn events(&self) -> broadcast::Receiver<Value> {
        self.inner.events.subscribe()
    }
}

async fn write_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
) -> Result<(), HerdrError> {
    let mut line = serde_json::to_string(value).map_err(|e| HerdrError::Io(e.to_string()))?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| HerdrError::Io(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| HerdrError::Io(e.to_string()))
}

/// The distinct `pane_id`s an `events.subscribe` payload asks about.
fn subscribed_panes(subscriptions: &Value) -> Vec<String> {
    let mut panes: Vec<String> = subscriptions
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("pane_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    panes.sort();
    panes.dedup();
    panes
}

/// Turn a raw response into the `result` payload or a typed error.
fn response_outcome(value: Value) -> Result<Value, HerdrError> {
    if let Some(err) = value.get("error") {
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
        return Err(HerdrError::Protocol { code, message });
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}
