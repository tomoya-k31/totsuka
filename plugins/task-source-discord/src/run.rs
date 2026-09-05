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

/// The gateway host used for a fresh connection. `resume_gateway_url` from
/// `READY` replaces it for a resume, and only for a resume.
const GATEWAY_BASE: &str = "wss://gateway.discord.gg";

/// The query every gateway connection needs — including a resumed one, whose
/// URL Discord hands over bare.
const GATEWAY_QUERY: &str = "/?v=10&encoding=json";

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
    S: Submitter + Clone + 'static,
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
                SessionEnd::Resumable(reason, went_live) => {
                    // Reset only when the session actually went live. A
                    // connection that dies during every handshake is not
                    // progress, and treating it as such produced a reconnect
                    // loop with no wait at all.
                    if went_live {
                        consecutive_failures = 0;
                    } else {
                        consecutive_failures += 1;
                    }
                    let delay = capped_backoff(FIRST_BACKOFF, MAX_BACKOFF, consecutive_failures);
                    tracing::info!(
                        consecutive_failures,
                        ?delay,
                        "discord gateway: {reason}; resuming"
                    );
                    tokio::time::sleep(delay).await;
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
    /// The session survives; reconnect and `RESUME`. Carries whether the
    /// session ever went live, which is what makes a reset of the failure
    /// counter mean "progress" rather than "we tried again".
    Resumable(String, bool),
    /// The session is gone; reconnect fresh and backfill.
    Restart(String),
}

/// The sleep used while no heartbeat interval is known yet. Long enough never
/// to fire before `HELLO` arrives.
const IDLE: Duration = Duration::from_secs(3600);

/// One Gateway session: connect, hand over frames, and report how it ended.
async fn session<S: Submitter + Clone + 'static>(
    config: &Arc<DiscordConfig>,
    triggers: &crate::watch::WatchTriggers,
    submitter: &S,
    state: &SharedState,
    resume_from: Option<&ResumeState>,
    resume_out: &mut Option<ResumeState>,
) -> SessionEnd {
    // `resume_gateway_url` arrives **without a query string**, and a
    // connection with no `v` can be closed as an invalid API version (4012) —
    // which this plugin treats as permanent, so the runtime would stop for
    // good on the first resume. The first connection never shows it.
    let url = resume_from
        .map(|r| r.resume_gateway_url.clone())
        .filter(|url| !url.is_empty())
        .map(|url| format!("{}{GATEWAY_QUERY}", url.trim_end_matches('/')))
        .unwrap_or_else(|| format!("{GATEWAY_BASE}{GATEWAY_QUERY}"));

    let (mut socket, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(pair) => pair,
        Err(e) => return SessionEnd::Restart(format!("could not connect: {e}")),
    };

    let mut seq: Option<u64> = resume_from.map(|r| r.seq);
    let mut heartbeat_every: Option<Duration> = None;
    let mut next_beat = Box::pin(tokio::time::sleep(IDLE));
    // Whether a heartbeat is still waiting for its ack. A socket that stops
    // acking is **half-open**: frames stop arriving but nothing errors, so
    // without this the session would sit silent until something else noticed.
    let mut awaiting_ack = false;
    // Whether this session ever went live. Only then is the failure counter
    // worth resetting — otherwise a connection that dies during the handshake
    // every time looks like progress.
    let mut went_live = false;

    loop {
        tokio::select! {
            _ = &mut next_beat, if heartbeat_every.is_some() => {
                let interval = heartbeat_every.expect("guarded by the `if`");
                if awaiting_ack {
                    return SessionEnd::Resumable(
                        "the previous heartbeat was never acknowledged (half-open socket)".into(),
                        went_live,
                    );
                }
                if socket.send(Message::Text(gateway::heartbeat(seq).to_string().into())).await.is_err() {
                    return SessionEnd::Resumable("heartbeat could not be sent".into(), went_live);
                }
                awaiting_ack = true;
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
                            if restart { SessionEnd::Restart(reason) } else { SessionEnd::Resumable(reason, went_live) }
                        };
                    }
                    Some(Ok(_)) => continue, // binary/ping/pong: nothing to read
                    Some(Err(e)) => return SessionEnd::Resumable(format!("socket error: {e}"), went_live),
                    None => return SessionEnd::Resumable("socket closed".into(), went_live),
                };
                let Ok(value) = serde_json::from_str::<Value>(&frame) else {
                    tracing::warn!("discord gateway sent a frame that is not JSON; ignoring");
                    continue;
                };

                match pipeline::handle_frame(&value, &mut seq, config, triggers, submitter, state) {
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
                        went_live = true;
                        *resume_out = Some(ready);
                    }
                    Step::Resumed => {
                        went_live = true;
                        tracing::info!("discord gateway resumed");
                    }
                    // Discord wants a beat now; the timer's turn has not come.
                    Step::HeartbeatNow => {
                        if socket.send(Message::Text(gateway::heartbeat(seq).to_string().into())).await.is_err() {
                            return SessionEnd::Resumable(
                                "heartbeat could not be sent".into(),
                                went_live,
                            );
                        }
                        awaiting_ack = true;
                    }
                    Step::HeartbeatAck => awaiting_ack = false,
                    Step::Reconnect { resumable } => {
                        return if resumable {
                            SessionEnd::Resumable("asked to reconnect".into(), went_live)
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
