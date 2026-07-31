//! Socket Mode client: the resident WebSocket that turns Slack's push events
//! into a stream the mention pipeline consumes.
//!
//! Lifecycle per connection: `apps.connections.open` (App-Level Token) → WSS
//! connect → `hello` → read envelopes. `events_api` / `interactive` envelopes
//! are **acked immediately on receipt** — Slack redelivers an envelope not
//! acked within ~3s, so the ack never waits on processing; the payload is
//! handed to the pipeline through an *unbounded* channel after the ack is on
//! the wire (a bounded channel could park the reader on a slow consumer and
//! stall every later ack). A `disconnect` message (Slack refreshing the
//! endpoint) or a dropped connection triggers a reconnect; sessions that die
//! before Slack's `hello` count as failures and back off exponentially.
//! Silence beyond `idle_timeout` is treated as a dead TCP path (Slack pings
//! every few seconds, so a healthy connection is never quiet that long).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::error::SlackError;
use crate::slack_api::SlackApi;
use crate::transport::{SlackTransport, capped_backoff};

/// A normalized event out of the Socket Mode stream.
#[derive(Debug, Clone)]
pub enum SocketEvent {
    /// An Events API `message` event — the envelope payload's `event` object
    /// (channel / user / text / ts / thread_ts / subtype / bot_id …).
    Message(Value),
    /// A Block Kit interaction — the full `block_actions` payload (actions /
    /// user / container / response_url …).
    BlockActions(Value),
    /// An Events API `reaction_added` event (#319) — the payload's `event`
    /// object (`user` / `reaction` / `item: {type, channel, ts}` /
    /// `item_user` / `event_ts`).
    ///
    /// **The message body is not in here.** `item.channel` + `item.ts` have to
    /// be re-fetched before the event can be assessed, which is why this stays
    /// a raw payload rather than a parsed struct.
    ///
    /// `reaction_removed` is deliberately not subscribed to: removing a
    /// reaction is not a cancel signal (that would add a second way to stop a
    /// running agent, competing with the approval flow).
    Reaction(Value),
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
    /// No frame for this long → the TCP path is presumed silently dead and
    /// the session reconnects. Slack pings every few seconds; a healthy
    /// connection never goes this quiet.
    pub idle_timeout: Duration,
}

impl Default for SocketModeOptions {
    fn default() -> Self {
        Self {
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(32),
            warn_after: 5,
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// Why one WebSocket session ended.
enum SessionEnd {
    /// A healthy session ended (Slack `disconnect` refresh, close frame, or
    /// clean EOF *after* `hello`) — reconnect without backoff growth.
    Refresh,
    /// The connection failed, died silently, or closed before proving itself
    /// with a `hello` — counts toward backoff.
    Failed(String),
    /// The event receiver is gone: the plugin is shutting down.
    Shutdown,
}

/// Run the Socket Mode loop over `api` (shared with the mention pipeline,
/// hence the `Arc`), delivering normalized events into the returned receiver
/// until the receiver is dropped or a fatal credential/configuration error
/// stops the loop (the error is returned by the task). Transient failures —
/// connection drops, `disconnect` refreshes, network errors on
/// `apps.connections.open` — reconnect forever with capped exponential
/// backoff.
pub fn spawn<T: SlackTransport + 'static>(
    api: Arc<SlackApi<T>>,
    options: SocketModeOptions,
) -> (
    mpsc::UnboundedReceiver<SocketEvent>,
    tokio::task::JoinHandle<Result<(), SlackError>>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async move { run(api, options, tx).await });
    (rx, handle)
}

async fn run<T: SlackTransport>(
    api: Arc<SlackApi<T>>,
    options: SocketModeOptions,
    tx: mpsc::UnboundedSender<SocketEvent>,
) -> Result<(), SlackError> {
    let mut consecutive_failures: u32 = 0;
    loop {
        if tx.is_closed() {
            return Ok(()); // the pipeline is gone; nothing to deliver to
        }
        let url = match api.apps_connections_open().await {
            Ok(url) => url,
            // A bad/underscoped xapp token never fixes itself — stop with the
            // guidance (already logged by the API layer) instead of retrying
            // forever. `apps.connections.open` treats every API error as
            // credential-class, so only network failures reach the arm below.
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

        match session(&url, &tx, options.idle_timeout).await {
            SessionEnd::Shutdown => return Ok(()),
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
    let delay = capped_backoff(
        options.backoff_base,
        options.backoff_max,
        n.saturating_sub(1),
    );
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
/// ended. Never returns while the connection is healthy and the receiver
/// lives.
async fn session(
    url: &str,
    tx: &mpsc::UnboundedSender<SocketEvent>,
    idle_timeout: Duration,
) -> SessionEnd {
    let (mut stream, _) = match connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => return SessionEnd::Failed(format!("websocket connect failed: {e}")),
    };

    // `hello` proves the endpoint actually spoke Socket Mode; a session dying
    // before it (e.g. a proxy accepting then closing) must not be treated as
    // a healthy refresh, or a broken endpoint becomes a tight reconnect loop.
    let mut healthy = false;
    // A clean close after hello reconnects without backoff.
    let end_of_stream = |healthy: bool| {
        if healthy {
            SessionEnd::Refresh
        } else {
            SessionEnd::Failed("connection closed before hello".into())
        }
    };

    loop {
        let next = tokio::select! {
            // Without this, a receiver dropped during a quiet session leaves
            // the loop parked in `stream.next()` forever (shutdown hang).
            _ = tx.closed() => return SessionEnd::Shutdown,
            next = tokio::time::timeout(idle_timeout, stream.next()) => next,
        };
        let message = match next {
            Err(_elapsed) => {
                return SessionEnd::Failed(format!(
                    "no traffic for {idle_timeout:?} (silent connection loss presumed)"
                ));
            }
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => return SessionEnd::Failed(format!("websocket read failed: {e}")),
            Ok(None) => return end_of_stream(healthy),
        };
        let text = match message {
            WsMessage::Text(text) => text,
            WsMessage::Close(_) => return end_of_stream(healthy),
            // tungstenite answers pings itself during read/flush.
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
                healthy = true;
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
                if let Some(event) = normalize(value)
                    && tx.send(event).is_err()
                {
                    return SessionEnd::Shutdown; // receiver dropped
                }
            }
        }
    }
}

/// Normalize an acked envelope into a [`SocketEvent`]; `None` for envelope
/// types the plugin does not consume (slash commands, non-message events, …).
/// Takes the envelope by value: the interesting subtree is moved out, not
/// deep-cloned, on this per-event hot path.
fn normalize(mut envelope: Value) -> Option<SocketEvent> {
    let envelope_type = envelope.get("type").and_then(Value::as_str)?;
    match envelope_type {
        "events_api" => {
            let event = envelope.get_mut("payload")?.get_mut("event")?.take();
            match event.get("type").and_then(Value::as_str) {
                Some("message") => Some(SocketEvent::Message(event)),
                Some("reaction_added") => Some(SocketEvent::Reaction(event)),
                _ => None,
            }
        }
        "interactive" => {
            let payload = envelope.get_mut("payload")?.take();
            if payload.get("type").and_then(Value::as_str) == Some("block_actions") {
                Some(SocketEvent::BlockActions(payload))
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
        let Some(SocketEvent::Message(event)) = normalize(envelope) else {
            panic!("expected Message");
        };
        assert_eq!(event["channel"], "C1");
    }

    #[test]
    fn events_api_reaction_added_normalizes_to_reaction() {
        // #319: this envelope used to be the example of a *dropped* event.
        let envelope = json!({
            "type": "events_api",
            "envelope_id": "e2",
            "payload": { "event": {
                "type": "reaction_added",
                "user": "U_ME",
                "reaction": "eyes",
                "item": { "type": "message", "channel": "C1", "ts": "1.1" },
                "item_user": "U_OTHER",
                "event_ts": "2.2"
            }}
        });
        let Some(SocketEvent::Reaction(event)) = normalize(envelope) else {
            panic!("expected Reaction");
        };
        assert_eq!(event["reaction"], "eyes");
        assert_eq!(event["item"]["ts"], "1.1");
    }

    #[test]
    fn unconsumed_events_api_types_are_dropped() {
        // `reaction_removed` is deliberately not subscribed to, and events
        // like `team_join` are simply not ours. Both must stay dropped.
        for event_type in ["reaction_removed", "team_join"] {
            let envelope = json!({
                "type": "events_api",
                "payload": { "event": { "type": event_type } }
            });
            assert!(
                normalize(envelope).is_none(),
                "{event_type} should be dropped"
            );
        }
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
        let Some(SocketEvent::BlockActions(payload)) = normalize(envelope) else {
            panic!("expected BlockActions");
        };
        assert_eq!(payload["actions"][0]["action_id"], "approve_reply");
        assert!(payload["response_url"].is_string());
    }

    #[test]
    fn other_envelope_types_are_ignored() {
        assert!(normalize(json!({ "type": "slash_commands", "payload": {} })).is_none());
        assert!(
            normalize(json!({
                "type": "interactive",
                "payload": { "type": "view_submission" }
            }))
            .is_none()
        );
        assert!(normalize(json!({ "type": "hello" })).is_none());
    }
}
