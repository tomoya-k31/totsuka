//! Socket Mode client: the resident WebSocket that turns Slack's push events
//! into a stream the mention pipeline consumes.
//!
//! Lifecycle per connection: `apps.connections.open` (App-Level Token) → WSS
//! connect → `hello` → read envelopes. `events_api` / `interactive` envelopes
//! are **acked immediately on receipt** — Slack redelivers an envelope not
//! acked within ~3s, so the ack never waits on processing; the payload is
//! handed to the pipeline through an mpsc channel *after* the ack is on the
//! wire. A `disconnect` message (Slack refreshing the endpoint) or a dropped
//! connection triggers a reconnect with capped exponential backoff.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::error::SlackError;
use crate::slack_api::SlackApi;
use crate::transport::SlackTransport;

/// A normalized event out of the Socket Mode stream.
#[derive(Debug, Clone)]
pub enum SocketEvent {
    /// An Events API `message` event — the envelope payload's `event` object
    /// (channel / user / text / ts / thread_ts / subtype / bot_id …).
    Message(Value),
    /// A Block Kit interaction — the full `block_actions` payload (actions /
    /// user / container / response_url …).
    BlockActions(Value),
}

/// Tuning knobs for the connection loop. [`Default`] is production; tests
/// shrink the delays.
#[derive(Debug, Clone)]
pub struct SocketModeOptions {
    /// First reconnect delay; doubles per consecutive failure.
    pub backoff_base: Duration,
    /// Reconnect delay ceiling.
    pub backoff_max: Duration,
    /// Consecutive connection failures after which each further failure is
    /// logged at `warn` (persistent outage), not just `info`.
    pub warn_after: u32,
    /// Buffered events between the reader and the pipeline.
    pub channel_capacity: usize,
}

impl Default for SocketModeOptions {
    fn default() -> Self {
        Self {
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(32),
            warn_after: 5,
            channel_capacity: 256,
        }
    }
}

/// Why one WebSocket session ended (each reason leads to a reconnect).
enum SessionEnd {
    /// Slack asked us to reconnect (`disconnect` message / close frame) or
    /// the stream ended cleanly — reconnect without backoff growth.
    Refresh,
    /// The connection failed — counts toward backoff.
    Failed(String),
}

/// Run the Socket Mode loop over `api`, delivering normalized events into the
/// returned receiver until the receiver is dropped or a fatal
/// credential/identity error stops the loop (the error is returned by the
/// task). Transient failures — connection drops, `disconnect` refreshes,
/// transport errors on `apps.connections.open` — reconnect forever with
/// capped exponential backoff.
pub fn spawn<T: SlackTransport + 'static>(
    api: SlackApi<T>,
    options: SocketModeOptions,
) -> (
    mpsc::Receiver<SocketEvent>,
    tokio::task::JoinHandle<Result<(), SlackError>>,
) {
    let (tx, rx) = mpsc::channel(options.channel_capacity);
    let handle = tokio::spawn(async move { run(api, options, tx).await });
    (rx, handle)
}

async fn run<T: SlackTransport>(
    api: SlackApi<T>,
    options: SocketModeOptions,
    tx: mpsc::Sender<SocketEvent>,
) -> Result<(), SlackError> {
    let mut consecutive_failures: u32 = 0;
    loop {
        if tx.is_closed() {
            return Ok(()); // the pipeline is gone; nothing to deliver to
        }
        let url = match api.apps_connections_open().await {
            Ok(url) => url,
            // A bad xapp token never fixes itself — stop with the guidance
            // (already logged by the API layer) instead of retrying forever.
            Err(e) if e.is_credential() => return Err(e),
            Err(e) => {
                consecutive_failures += 1;
                backoff(
                    &options,
                    consecutive_failures,
                    &format!("apps.connections.open failed: {e}"),
                )
                .await;
                continue;
            }
        };

        match session(&url, &tx).await {
            SessionEnd::Refresh => {
                consecutive_failures = 0;
                tracing::info!("socket mode: reconnecting after endpoint refresh");
            }
            SessionEnd::Failed(reason) => {
                consecutive_failures += 1;
                backoff(&options, consecutive_failures, &reason).await;
            }
        }
    }
}

/// Wait out the capped exponential backoff for failure number `n`, logging at
/// `warn` once the outage looks persistent.
async fn backoff(options: &SocketModeOptions, n: u32, reason: &str) {
    let factor = 2u32.saturating_pow(n.saturating_sub(1));
    let delay = options
        .backoff_base
        .saturating_mul(factor)
        .min(options.backoff_max);
    if n >= options.warn_after {
        tracing::warn!(
            consecutive_failures = n,
            ?delay,
            "socket mode: {reason}; reconnecting"
        );
    } else {
        tracing::info!(
            consecutive_failures = n,
            ?delay,
            "socket mode: {reason}; reconnecting"
        );
    }
    tokio::time::sleep(delay).await;
}

/// One WebSocket session: connect, ack + forward envelopes, and report how it
/// ended. Never returns while the connection is healthy.
async fn session(url: &str, tx: &mpsc::Sender<SocketEvent>) -> SessionEnd {
    let (mut stream, _) = match connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => return SessionEnd::Failed(format!("websocket connect failed: {e}")),
    };

    loop {
        let message = match stream.next().await {
            Some(Ok(m)) => m,
            Some(Err(e)) => return SessionEnd::Failed(format!("websocket read failed: {e}")),
            None => return SessionEnd::Refresh, // clean end of stream
        };
        let text = match message {
            WsMessage::Text(text) => text,
            WsMessage::Close(_) => return SessionEnd::Refresh,
            // tungstenite answers pings during read/flush; nothing to do here.
            _ => continue,
        };
        let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
            tracing::debug!("socket mode: ignoring non-JSON frame");
            continue;
        };

        // Ack FIRST: Slack redelivers envelopes not acked within ~3s, and the
        // ack must never wait on downstream processing.
        if let Some(envelope_id) = value.get("envelope_id").and_then(Value::as_str) {
            let ack = json!({ "envelope_id": envelope_id }).to_string();
            if let Err(e) = stream.send(WsMessage::text(ack)).await {
                return SessionEnd::Failed(format!("websocket ack failed: {e}"));
            }
        }

        match value.get("type").and_then(Value::as_str) {
            Some("hello") => {
                tracing::info!("socket mode: connected (hello)");
            }
            Some("disconnect") => {
                let reason = value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unspecified");
                tracing::info!(reason, "socket mode: server asked to reconnect");
                return SessionEnd::Refresh;
            }
            _ => {
                if let Some(event) = normalize(&value)
                    && tx.send(event).await.is_err()
                {
                    return SessionEnd::Refresh; // receiver dropped; run() exits
                }
            }
        }
    }
}

/// Normalize an acked envelope into a [`SocketEvent`]; `None` for envelope
/// types the plugin does not consume (slash commands, non-message events, …).
fn normalize(envelope: &Value) -> Option<SocketEvent> {
    let payload = envelope.get("payload")?;
    match envelope.get("type").and_then(Value::as_str)? {
        "events_api" => {
            let event = payload.get("event")?;
            if event.get("type").and_then(Value::as_str) == Some("message") {
                Some(SocketEvent::Message(event.clone()))
            } else {
                None
            }
        }
        "interactive" => {
            if payload.get("type").and_then(Value::as_str) == Some("block_actions") {
                Some(SocketEvent::BlockActions(payload.clone()))
            } else {
                None
            }
        }
        other => {
            tracing::debug!(envelope_type = other, "socket mode: ignoring envelope");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_api_message_normalizes_to_message() {
        let envelope = json!({
            "type": "events_api",
            "envelope_id": "e1",
            "payload": { "event": { "type": "message", "text": "hi", "channel": "C1" } }
        });
        let Some(SocketEvent::Message(event)) = normalize(&envelope) else {
            panic!("expected Message");
        };
        assert_eq!(event["channel"], "C1");
    }

    #[test]
    fn non_message_events_are_dropped() {
        let envelope = json!({
            "type": "events_api",
            "payload": { "event": { "type": "reaction_added" } }
        });
        assert!(normalize(&envelope).is_none());
    }

    #[test]
    fn block_actions_normalize_with_full_payload() {
        let envelope = json!({
            "type": "interactive",
            "payload": {
                "type": "block_actions",
                "response_url": "https://hooks.slack.test/r/1",
                "actions": [{ "action_id": "approve_reply" }]
            }
        });
        let Some(SocketEvent::BlockActions(payload)) = normalize(&envelope) else {
            panic!("expected BlockActions");
        };
        assert_eq!(payload["actions"][0]["action_id"], "approve_reply");
        assert!(payload["response_url"].is_string());
    }

    #[test]
    fn other_envelope_types_are_ignored() {
        assert!(normalize(&json!({ "type": "slash_commands", "payload": {} })).is_none());
        assert!(
            normalize(&json!({
                "type": "interactive",
                "payload": { "type": "view_submission" }
            }))
            .is_none()
        );
        assert!(normalize(&json!({ "type": "hello" })).is_none());
    }
}
