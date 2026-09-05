//! The resident Gateway loop: connect, identify or resume, heartbeat, and
//! feed `MESSAGE_CREATE` to the watch table.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use plugin_sdk::{BackfillLimits, Submitter};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use crate::config::DiscordConfig;
use crate::discord_api::DiscordApi;
use crate::error::{DiscordError, gateway_close_failure};
use crate::gateway::{self, ResumeState, Step};
use crate::pipeline::{self, SharedState};
use crate::transport::{DiscordTransport, capped_backoff};

/// The gateway URL used for a fresh connection. `resume_gateway_url` from
/// `READY` replaces it for a resume, and only for a resume.
const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// First reconnect backoff step; doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// Reconnect backoff ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Consecutive failures before the reconnect log moves from info to warn.
const WARN_AFTER: u32 = 3;

/// Start the resident runtime. It ends only when the task is aborted or a
/// permanent failure stops it.
pub fn spawn<T, S>(
    api: Arc<DiscordApi<T>>,
    config: Arc<DiscordConfig>,
    triggers: crate::watch::WatchTriggers,
    limits: BackfillLimits,
    state: SharedState,
    submitter: S,
) -> tokio::task::JoinHandle<()>
where
    T: DiscordTransport + Send + Sync + 'static,
    S: Submitter + 'static,
{
    tokio::spawn(async move {
        // Both startup steps run before the first connection, so a post made
        // during a restart is recovered ahead of anything arriving live.
        pipeline::verify_watched_names(api.as_ref(), &triggers).await;
        pipeline::backfill(
            api.as_ref(),
            &config,
            &triggers,
            &limits,
            &submitter,
            &state,
        )
        .await;

        let mut consecutive_failures: u32 = 0;
        let mut resume_state: Option<ResumeState> = None;
        loop {
            // Cloned rather than borrowed: the session both reads the point
            // it should resume from and writes back the one it reached, and
            // those cannot be the same borrow.
            let resume_from = resume_state.clone();
            let outcome = session(
                &config,
                &triggers,
                &submitter,
                &state,
                resume_from.as_ref(),
                &mut resume_state,
            )
            .await;
            match outcome {
                SessionEnd::Permanent(e) => {
                    tracing::error!("discord gateway stopped: {e}");
                    return;
                }
                SessionEnd::Resumable(reason) => {
                    consecutive_failures = 0;
                    tracing::info!("discord gateway: {reason}; resuming");
                }
                SessionEnd::Restart(reason) => {
                    // The resume window is gone, so the events missed since
                    // are only recoverable through history.
                    resume_state = None;
                    consecutive_failures += 1;
                    let delay =
                        capped_backoff(FIRST_BACKOFF, MAX_BACKOFF, consecutive_failures - 1);
                    if consecutive_failures >= WARN_AFTER {
                        tracing::warn!(consecutive_failures, ?delay, "discord gateway: {reason}");
                    } else {
                        tracing::info!(consecutive_failures, ?delay, "discord gateway: {reason}");
                    }
                    tokio::time::sleep(delay).await;
                    pipeline::backfill(
                        api.as_ref(),
                        &config,
                        &triggers,
                        &limits,
                        &submitter,
                        &state,
                    )
                    .await;
                }
            }
        }
    })
}

/// How one Gateway session ended.
enum SessionEnd {
    /// Unfixable — stop, do not reconnect.
    Permanent(DiscordError),
    /// The session survives; reconnect and `RESUME`.
    Resumable(String),
    /// The session is gone; reconnect fresh and backfill.
    Restart(String),
}

/// One Gateway session: connect, hand over frames, and report how it ended.
async fn session<S: Submitter>(
    config: &DiscordConfig,
    triggers: &crate::watch::WatchTriggers,
    submitter: &S,
    state: &SharedState,
    resume_from: Option<&ResumeState>,
    resume_out: &mut Option<ResumeState>,
) -> SessionEnd {
    let url = resume_from
        .map(|r| r.resume_gateway_url.clone())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| GATEWAY_URL.to_string());

    let (mut socket, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(pair) => pair,
        Err(e) => return SessionEnd::Restart(format!("could not connect: {e}")),
    };

    let mut seq: Option<u64> = resume_from.map(|r| r.seq);
    let mut heartbeat_every: Option<Duration> = None;
    let mut next_beat = Box::pin(tokio::time::sleep(Duration::from_secs(3600)));

    loop {
        tokio::select! {
            _ = &mut next_beat, if heartbeat_every.is_some() => {
                let interval = heartbeat_every.expect("guarded by the `if`");
                if socket.send(Message::Text(gateway::heartbeat(seq).to_string().into())).await.is_err() {
                    return SessionEnd::Resumable("heartbeat could not be sent".into());
                }
                next_beat = Box::pin(tokio::time::sleep(interval));
            }
            frame = socket.next() => {
                let frame = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(close))) => {
                        let code = close.map(|c| u16::from(c.code)).unwrap_or(1006);
                        return if gateway::close_code_is_permanent(code) {
                            SessionEnd::Permanent(gateway_close_failure(code))
                        } else {
                            // 4007 (bad seq) and 4009 (timed out) invalidate
                            // the session even though reconnecting is fine.
                            let restart = matches!(code, 4007 | 4009);
                            let reason = format!("closed with {code}");
                            if restart { SessionEnd::Restart(reason) } else { SessionEnd::Resumable(reason) }
                        };
                    }
                    Some(Ok(_)) => continue, // binary/ping/pong: nothing to read
                    Some(Err(e)) => return SessionEnd::Resumable(format!("socket error: {e}")),
                    None => return SessionEnd::Resumable("socket closed".into()),
                };
                let Ok(value) = serde_json::from_str::<Value>(&frame) else {
                    tracing::warn!("discord gateway sent a frame that is not JSON; ignoring");
                    continue;
                };

                match pipeline::handle_frame(&value, &mut seq, config, triggers, submitter, state).await {
                    Step::Hello { interval } => {
                        heartbeat_every = Some(interval);
                        next_beat = Box::pin(tokio::time::sleep(
                            pipeline::first_heartbeat_delay(interval),
                        ));
                        let opening = match resume_from {
                            Some(r) => gateway::resume(&config.bot_token, r),
                            None => gateway::identify(&config.bot_token),
                        };
                        if socket.send(Message::Text(opening.to_string().into())).await.is_err() {
                            return SessionEnd::Restart("handshake could not be sent".into());
                        }
                    }
                    Step::Ready(ready) => {
                        tracing::info!(session = %ready.session_id, "discord gateway ready");
                        *resume_out = Some(ready);
                    }
                    Step::Resumed => tracing::info!("discord gateway resumed"),
                    Step::Reconnect { resumable } => {
                        return if resumable {
                            SessionEnd::Resumable("asked to reconnect".into())
                        } else {
                            SessionEnd::Restart("session invalidated".into())
                        };
                    }
                    // The sequence advanced inside `handle_frame`; keep the
                    // saved resume point in step so a later resume replays
                    // from where this session actually got to.
                    Step::Message(_) | Step::Idle => {
                        if let (Some(saved), Some(n)) = (resume_out.as_mut(), seq) {
                            saved.seq = n;
                        }
                    }
                }
            }
        }
    }
}
