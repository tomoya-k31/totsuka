//! The resident runtime: the Gateway connection, the startup checks, and the
//! watch → `task/submit` path.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use plugin_sdk::{BackfillLimits, SubmitOutcome, Submitter};
use serde_json::Value;

use crate::config::DiscordConfig;
use crate::discord_api::{DiscordApi, DiscordMessage, snowflake_for};
use crate::transport::DiscordTransport;
use crate::watch::WatchTriggers;

/// Where a task's result must be posted, remembered from when it was raised.
///
/// Keyed by task id and held in memory: `result/publish` arrives with a task
/// id and nothing else. Unlike Slack's, these coordinates are **derivable**
/// from the task id — it is literally `{channel}:{message}` — but recording
/// them keeps `result/publish` from having to parse an id format, which is
/// the kind of coupling that breaks the day a prefix is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPost {
    /// The watched channel.
    pub channel_id: String,
    /// The post that raised the task; also the id of a thread started from it.
    pub message_id: String,
    /// The author, so the report can mention them.
    pub author_id: String,
}

/// Bound on the pending index; oldest entries fall out first.
const PENDING_CAP: usize = 1024;

/// The pending index, shared between the runtime and the JSON-RPC server.
#[derive(Clone, Default)]
pub struct SharedState {
    pending: Arc<Mutex<Vec<(String, PendingPost)>>>,
}

impl SharedState {
    /// Remember where `task_id`'s result belongs.
    pub fn insert_pending(&self, task_id: String, post: PendingPost) {
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|(id, _)| id != &task_id);
        if pending.len() >= PENDING_CAP {
            let evicted = pending.remove(0);
            tracing::warn!(
                task_id = %evicted.0,
                "pending-post index full; evicted the oldest entry (its result can no longer \
                 be posted)"
            );
        }
        pending.push((task_id, post));
    }

    /// The coordinates for `task_id`, if still held.
    pub fn pending(&self, task_id: &str) -> Option<PendingPost> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == task_id)
            .map(|(_, post)| post.clone())
    }

    /// Drop `task_id`'s entry — the terminal step once its result is posted.
    pub fn take_pending(&self, task_id: &str) -> Option<PendingPost> {
        let mut pending = self.pending.lock().unwrap();
        let index = pending.iter().position(|(id, _)| id == task_id)?;
        Some(pending.remove(index).1)
    }
}

/// Verify each watched channel's live name, warning on a mismatch.
///
/// Advisory: the watch keys on the id, so a rename breaks nothing — it just
/// means the config now describes the channel by a name it no longer has,
/// which is how a watch quietly ends up pointing somewhere unintended.
pub async fn verify_watched_names<T: DiscordTransport>(
    api: &DiscordApi<T>,
    triggers: &WatchTriggers,
) {
    for watched in triggers.channels() {
        match api.channel_name(&watched.trigger.channel).await {
            Ok(live) => {
                if let Some(warning) = watched.trigger.name_mismatch(&live) {
                    tracing::warn!("{warning}");
                }
            }
            Err(e) => tracing::warn!(
                channel = %watched.trigger.channel,
                error = %e,
                "could not read the watched channel's name; the rename check is skipped for \
                 this run (the watch keys on the id and still works)"
            ),
        }
    }
}

/// Recover the posts made while the plugin was down.
///
/// The Gateway replays a dropped session only inside its resume window; past
/// that, history is the only way back. No cursor is kept: re-submitting a
/// post the ledger already holds is an idempotent `duplicate` ack, so
/// over-fetching costs nothing while under-fetching loses a post silently.
pub async fn backfill<T: DiscordTransport, S: Submitter>(
    api: &DiscordApi<T>,
    config: &DiscordConfig,
    triggers: &WatchTriggers,
    limits: &BackfillLimits,
    submitter: &S,
    state: &SharedState,
) {
    let cutoff = limits
        .cutoff(SystemTime::now())
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let after = snowflake_for(cutoff.as_millis() as u64);

    for watched in triggers.channels() {
        let channel = &watched.trigger.channel;
        let messages = match api.channel_messages(channel, limits.count, &after).await {
            Ok(messages) => messages,
            Err(e) => {
                tracing::warn!(
                    channel = %channel,
                    error = %e,
                    "backfill of a watched channel failed; skipping it (live events are \
                     unaffected, but posts made while the plugin was down are lost)"
                );
                continue;
            }
        };
        // Oldest first: Discord answers newest-first, and recovered posts
        // should be submitted in the order they were written.
        for message in messages.iter().rev() {
            if let Some(task_id) = submit_message(config, triggers, submitter, state, message).await
            {
                tracing::info!(
                    task_id,
                    channel = %channel,
                    "recovered a post made while the plugin was down"
                );
            }
        }
    }
}

/// Run one message through the watch table and submit it if it is admitted.
/// Answers the task id when a task was raised.
pub async fn submit_message<S: Submitter>(
    config: &DiscordConfig,
    triggers: &WatchTriggers,
    submitter: &S,
    state: &SharedState,
    message: &DiscordMessage,
) -> Option<String> {
    let (watched, mut task) = triggers.admit(message)?;
    task.source = config.source_name.clone();
    let task_id = task.id.clone();
    let workflow = watched.trigger.workflow.clone();

    // Registered before the submit so a `result/publish` racing the ack still
    // finds its coordinates.
    state.insert_pending(
        task_id.clone(),
        PendingPost {
            channel_id: message.channel_id.clone(),
            message_id: message.id.clone(),
            author_id: message.author_id.clone().unwrap_or_default(),
        },
    );
    match submitter.submit(task, &workflow).await {
        SubmitOutcome::Accepted => Some(task_id),
        // The steady state for a backfilled post the ledger already has.
        SubmitOutcome::Duplicate => {
            state.take_pending(&task_id);
            None
        }
        SubmitOutcome::Rejected { reason } => {
            tracing::warn!(
                task_id,
                workflow = %workflow,
                "orchestrator rejected the task: {}",
                reason.as_deref().unwrap_or("no reason given")
            );
            state.take_pending(&task_id);
            None
        }
        SubmitOutcome::GaveUp { error } => {
            tracing::error!(
                task_id,
                workflow = %workflow,
                "task submission gave up: {error} → the post remains in Discord, so a later \
                 restart's backfill can pick it up while it is inside the window"
            );
            state.take_pending(&task_id);
            None
        }
    }
}

/// Publish a result back to the channel, under the bot's name.
///
/// The result hangs off the post that caused it: a public thread started from
/// that message keeps the channel readable when several clips are in flight.
/// A thread that already exists answers 400, and that is not a failure — the
/// thread's id equals the message's, so posting proceeds either way.
pub async fn publish_result<T: DiscordTransport>(
    api: &DiscordApi<T>,
    state: &SharedState,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    let text = content.trim();
    if text.is_empty() {
        return Err(format!(
            "task {task_id} published an empty result → nothing to post"
        ));
    }
    // Peek, do not take: the entry is spent only once the post succeeds, so a
    // failed publish can be retried.
    let Some(post) = state.pending(task_id) else {
        return Err(format!(
            "task {task_id} has no pending Discord coordinates (plugin restarted since the \
             post?) → the result cannot be placed; re-post in the watched channel"
        ));
    };

    let thread_name = format!("totsuka: {}", task_id.chars().take(80).collect::<String>());
    if let Err(e) = api
        .start_thread(&post.channel_id, &post.message_id, &thread_name)
        .await
    {
        tracing::debug!(
            task_id,
            error = %e,
            "could not start a thread for the result; posting into the existing one"
        );
    }
    let body = format!("<@{}> {text}", post.author_id);
    // A public thread's id *is* the starter message's id, so this addresses
    // the thread whether it was just created or already existed.
    api.create_message(&post.message_id, &body)
        .await
        .map_err(|e| {
            format!(
                "the watch result could not be posted: {e} → check that the bot can Send \
                 Messages in Threads in the watched channel"
            )
        })?;
    state.take_pending(task_id);
    Ok(())
}

/// Consume Gateway frames forever: track the sequence, hand `MESSAGE_CREATE`
/// to the watch table, and report when the connection must be re-established.
pub async fn handle_frame<S: Submitter>(
    frame: &Value,
    seq: &mut Option<u64>,
    config: &DiscordConfig,
    triggers: &WatchTriggers,
    submitter: &S,
    state: &SharedState,
) -> crate::gateway::Step {
    let step = crate::gateway::step(frame, seq);
    if let crate::gateway::Step::Message(payload) = &step {
        let message = crate::discord_api::parse_message_payload(payload);
        submit_message(config, triggers, submitter, state, &message).await;
    }
    step
}

/// The interval a heartbeat task sleeps between beats, jittered on the first
/// one as Discord asks.
pub fn first_heartbeat_delay(interval: Duration) -> Duration {
    // Discord asks for `interval * jitter` where jitter is in [0, 1); a fixed
    // half avoids a rand dependency and still staggers restarts against each
    // other, which is all the jitter is for.
    interval / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str) -> PendingPost {
        PendingPost {
            channel_id: "C1".into(),
            message_id: id.into(),
            author_id: "U_OP".into(),
        }
    }

    #[test]
    fn pending_coordinates_round_trip_and_are_spent_once() {
        let state = SharedState::default();
        state.insert_pending("t1".into(), post("M1"));
        assert_eq!(state.pending("t1"), Some(post("M1")));
        assert_eq!(state.take_pending("t1"), Some(post("M1")));
        assert_eq!(state.pending("t1"), None, "spent exactly once");
        assert_eq!(state.take_pending("t1"), None);
    }

    #[test]
    fn re_registering_a_task_replaces_rather_than_duplicates() {
        let state = SharedState::default();
        state.insert_pending("t1".into(), post("M1"));
        state.insert_pending("t1".into(), post("M2"));
        assert_eq!(state.pending("t1"), Some(post("M2")));
        assert_eq!(state.take_pending("t1"), Some(post("M2")));
        assert_eq!(state.pending("t1"), None, "only one entry existed");
    }

    #[test]
    fn the_index_is_bounded_and_evicts_the_oldest() {
        let state = SharedState::default();
        for n in 0..(PENDING_CAP + 5) {
            state.insert_pending(format!("t{n}"), post(&format!("M{n}")));
        }
        assert_eq!(state.pending("t0"), None, "the oldest fell out");
        let newest = format!("t{}", PENDING_CAP + 4);
        assert!(state.pending(&newest).is_some());
    }

    #[test]
    fn the_first_heartbeat_is_staggered_within_the_interval() {
        let interval = Duration::from_millis(41_250);
        let first = first_heartbeat_delay(interval);
        assert!(first > Duration::ZERO && first < interval);
    }
}
