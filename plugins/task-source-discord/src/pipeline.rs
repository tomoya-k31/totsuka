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
    let oldest = snowflake_for(cutoff.as_millis() as u64);

    for watched in triggers.channels() {
        let channel = &watched.trigger.channel;
        let messages = match api.channel_messages(channel, limits.count).await {
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
        // should be submitted in the order they were written. The age bound
        // is applied here rather than in the request — see
        // `channel_messages` for why `after` cannot express it.
        for message in messages
            .iter()
            .rev()
            .filter(|m| crate::discord_api::is_at_or_after(&m.id, &oldest))
        {
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
        //
        // **The pending entry stays.** `duplicate` means the task exists —
        // very possibly still running — and taking its coordinates here would
        // leave its `result/publish` with nowhere to post. The entry we just
        // wrote is byte-identical to the one already there anyway: both are
        // derived from the same message.
        SubmitOutcome::Duplicate => None,
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
        // A thread already started from this message answers 400, and that is
        // the expected steady state — the post below addresses it either way.
        // Anything else (no Create Public Threads, no access) is a real
        // problem, and logging it at debug alongside the routine case is how
        // it would go unnoticed until someone asked why nothing posts.
        if e.is_credential() {
            tracing::warn!(
                task_id,
                error = %e,
                "could not start a thread for the result → the bot needs Create Public \
                 Threads in the watched channel; trying to post anyway"
            );
        } else {
            tracing::debug!(
                task_id,
                error = %e,
                "could not start a thread for the result; posting into the existing one"
            );
        }
    }
    let body = clamp_to_discord_limit(&format!("<@{}> {text}", post.author_id));
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

/// Discord rejects a message whose `content` exceeds 2000 characters.
const MESSAGE_CONTENT_LIMIT: usize = 2000;

/// Trim `body` to what Discord will accept, leaving a visible marker.
///
/// Truncating beats failing: an over-long report otherwise 400s and the
/// channel learns **nothing** — not even that a run happened. The deliverable
/// URL the report is required to carry sits near its start, so what is lost is
/// the tail rather than the point. Counted in `chars`, not bytes, because
/// Discord counts characters and a byte cut could also split one in half.
fn clamp_to_discord_limit(body: &str) -> String {
    if body.chars().count() <= MESSAGE_CONTENT_LIMIT {
        return body.to_string();
    }
    const MARKER: &str = "\n…（長すぎるため以降は省略）";
    let keep = MESSAGE_CONTENT_LIMIT - MARKER.chars().count();
    let mut out: String = body.chars().take(keep).collect();
    out.push_str(MARKER);
    out
}

/// Classify one Gateway frame and, for a message, start its submission.
///
/// **The submission is spawned, never awaited here.** `task/submit` waits for
/// the Orchestrator's persist-before-ack and retries with backoff, so one call
/// can take well over two minutes — while Discord expects a heartbeat about
/// every 41 seconds. Awaiting it inside the connection's `select!` loop would
/// starve the heartbeat branch and **guarantee** a disconnect under exactly
/// the conditions (a busy or slow Orchestrator) where staying connected
/// matters most. The Slack source spawns for the same reason; here the loop
/// also owns the heartbeat, so the cost of getting it wrong is higher.
pub fn handle_frame<S>(
    frame: &Value,
    seq: &mut Option<u64>,
    config: &Arc<DiscordConfig>,
    triggers: &WatchTriggers,
    submitter: &S,
    state: &SharedState,
) -> crate::gateway::Step
where
    S: Submitter + Clone + 'static,
{
    let step = crate::gateway::step(frame, seq);
    if let crate::gateway::Step::Message(payload) = &step {
        let message = crate::discord_api::parse_message_payload(payload);
        // Cheap pre-check on the loop thread: only an admitted message is
        // worth a task, and `admit` does no I/O.
        if triggers.admit(&message).is_some() {
            let (config, triggers, submitter, state) = (
                Arc::clone(config),
                triggers.clone(),
                submitter.clone(),
                state.clone(),
            );
            tokio::spawn(async move {
                submit_message(&config, &triggers, &submitter, &state, &message).await;
            });
        }
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

    /// A `duplicate` ack means the task **exists** — very possibly still
    /// running — so its publish coordinates must survive. Taking them here is
    /// how a backfill would silently break an in-flight task's result.
    #[tokio::test]
    async fn a_duplicate_submission_leaves_the_coordinates_in_place() {
        use crate::watch::WatchTriggers;
        use plugin_sdk::{SubmitOutcome, Submitter};

        struct AlwaysDuplicate;
        impl Submitter for AlwaysDuplicate {
            async fn submit(&self, _task: plugin_protocol::Task, _wf: &str) -> SubmitOutcome {
                SubmitOutcome::Duplicate
            }
        }

        let trigger = serde_json::json!({
            "channel": "C1", "channel_name": "clip", "repo": "docs",
        });
        let watch = plugin_sdk::WatchTrigger::parse(&trigger, "clip")
            .unwrap()
            .unwrap();
        let workflows = [plugin_protocol::methods::WorkflowInfo {
            workflow: "clip".into(),
            trigger,
            instructions_kind: None,
            task_id_prefix: None,
            options: serde_json::Map::new(),
        }];
        let triggers = WatchTriggers::new(vec![watch], &workflows, "U_OP", "U_BOT");
        let config: DiscordConfig = serde_json::from_value(serde_json::json!({
            "bot_token": "t", "operator_user_id": "U_OP",
        }))
        .unwrap();
        let state = SharedState::default();
        let message = DiscordMessage {
            id: "M1".into(),
            channel_id: "C1".into(),
            author_id: Some("U_OP".into()),
            author_is_bot: false,
            content: "https://example.com".into(),
            kind: 0,
        };

        let raised = submit_message(&config, &triggers, &AlwaysDuplicate, &state, &message).await;
        assert_eq!(raised, None, "a duplicate raises no new task");
        assert_eq!(
            state.pending("C1:M1"),
            Some(PendingPost {
                channel_id: "C1".into(),
                message_id: "M1".into(),
                author_id: "U_OP".into(),
            }),
            "the running task's result still needs somewhere to go"
        );
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

    /// An over-long report must arrive truncated rather than not at all: a
    /// 400 from Discord would leave the channel with no sign a run happened.
    #[test]
    fn an_over_long_body_is_truncated_rather_than_rejected() {
        let short = "<@U1> done: https://example.com/pull/1";
        assert_eq!(
            clamp_to_discord_limit(short),
            short,
            "short bodies untouched"
        );

        let long = "あ".repeat(5000);
        let clamped = clamp_to_discord_limit(&long);
        assert!(
            clamped.chars().count() <= MESSAGE_CONTENT_LIMIT,
            "got {} chars",
            clamped.chars().count()
        );
        assert!(clamped.contains("省略"), "the cut must be visible");
        // Counted in chars, not bytes: a 3-byte character must not be cut in
        // half, and the result must still be valid UTF-8 by construction.
        assert!(clamped.starts_with('あ'));
    }

    /// Exactly at the limit is not over it — an off-by-one here would truncate
    /// every report that happened to land on the boundary.
    #[test]
    fn a_body_exactly_at_the_limit_is_left_alone() {
        let exact = "x".repeat(MESSAGE_CONTENT_LIMIT);
        assert_eq!(clamp_to_discord_limit(&exact), exact);
        let over = "x".repeat(MESSAGE_CONTENT_LIMIT + 1);
        assert_ne!(clamp_to_discord_limit(&over), over);
    }

    #[test]
    fn the_first_heartbeat_is_staggered_within_the_interval() {
        let interval = Duration::from_millis(41_250);
        let first = first_heartbeat_delay(interval);
        assert!(first > Duration::ZERO && first < interval);
    }
}
