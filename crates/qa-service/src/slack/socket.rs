//! Slack Socket Mode WebSocket loop. Connection lifecycle:
//!   1. POST apps.connections.open → returns WSS URL
//!   2. Open WS, await `hello`
//!   3. For each `events_api` envelope: ACK first, then forward event
//!   4. On `disconnect` or transport error: reconnect with exp backoff
//!
//! ACK-before-forward avoids Slack retry storms when downstream is slow.

use crate::error::QaError;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use totsuka_core::Secret;

use super::envelope::{parse, SlackEnvelope, SlackEvent};

pub struct SocketModeConfig {
    pub app_token: Secret<String>,
    pub apps_connections_endpoint: String,
}

impl SocketModeConfig {
    pub fn new(app_token: Secret<String>) -> Self {
        Self {
            app_token,
            apps_connections_endpoint: "https://slack.com/api/apps.connections.open".into(),
        }
    }
}

pub async fn fetch_socket_url(client: &Client, cfg: &SocketModeConfig) -> Result<String, QaError> {
    let resp = client
        .post(&cfg.apps_connections_endpoint)
        .header("authorization", format!("Bearer {}", cfg.app_token.expose()))
        .header("content-type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    if !v["ok"].as_bool().unwrap_or(false) {
        return Err(QaError::Slack(format!(
            "apps.connections.open: {}",
            v["error"].as_str().unwrap_or("unknown")
        )));
    }
    v["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| QaError::Slack("apps.connections.open: missing url".into()))
}

pub async fn run_socket_loop(
    cfg: SocketModeConfig,
    http: Arc<Client>,
    on_event: mpsc::Sender<SlackEvent>,
    shutdown: CancellationToken,
) -> Result<(), QaError> {
    let mut attempt: u32 = 0;
    loop {
        if shutdown.is_cancelled() { return Ok(()); }
        match try_one_connection(&cfg, &http, &on_event, &shutdown).await {
            Ok(()) => {
                attempt = 0;
                tracing::info!("socket-mode disconnected cleanly; reconnecting");
            }
            Err(e) => {
                attempt = (attempt + 1).min(5);
                let backoff = 2u64.saturating_pow(attempt - 1).min(30);
                tracing::warn!(error=%e, "socket-mode error; reconnecting in {backoff}s");
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
            }
        }
    }
}

async fn try_one_connection(
    cfg: &SocketModeConfig,
    http: &Arc<Client>,
    on_event: &mpsc::Sender<SlackEvent>,
    shutdown: &CancellationToken,
) -> Result<(), QaError> {
    let url = fetch_socket_url(http, cfg).await?;
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| QaError::WebSocket(format!("connect: {e}")))?;
    let (mut sink, mut stream) = ws.split();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            msg = stream.next() => {
                let Some(msg) = msg else { return Ok(()); };
                let msg = msg.map_err(|e| QaError::WebSocket(format!("recv: {e}")))?;
                match msg {
                    Message::Text(raw) => {
                        let env = match parse(raw.as_str()) {
                            Ok(e) => e,
                            Err(e) => {
                                tracing::debug!(error=%e, "ignoring unrecognised slack envelope");
                                continue;
                            }
                        };
                        match env {
                            SlackEnvelope::Hello => {
                                tracing::info!("socket-mode hello received");
                            }
                            SlackEnvelope::Disconnect { reason } => {
                                tracing::info!(%reason, "socket-mode disconnect requested");
                                return Ok(());
                            }
                            SlackEnvelope::EventsApi { envelope_id, event } => {
                                // ACK first (Slack will retry within 3s otherwise).
                                let ack = serde_json::json!({ "envelope_id": envelope_id });
                                sink.send(Message::Text(ack.to_string().into()))
                                    .await
                                    .map_err(|e| QaError::WebSocket(format!("ack: {e}")))?;
                                // Drop-oldest semantics: try_send; on full, log.
                                // Closed channel is an error; full channel drops oldest event.
                                if let Err(e) = on_event.try_send(event) {
                                    match e {
                                        TrySendError::Full(_) => {
                                            tracing::warn!(channel="slack_inbound", "channel full; dropping event");
                                        }
                                        TrySendError::Closed(_) => {
                                            return Err(crate::error::QaError::Internal(
                                                "slack_inbound receiver closed".into(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Message::Ping(p) => {
                        sink.send(Message::Pong(p)).await
                            .map_err(|e| QaError::WebSocket(format!("pong: {e}")))?;
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}
