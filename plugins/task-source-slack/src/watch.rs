//! Channel watch (#617): every top-level post in a watched channel becomes a
//! task, with no mention and no reaction.
//!
//! The vocabulary, the author gate and the backfill shape are
//! [`plugin_sdk::watch`]'s ([ADR-0068]); this module is the Slack half —
//! the filter table over raw `message` events, and the [`Mention`] it hands
//! to the shared enrich → submit path.
//!
//! # Why this needs no new Slack scope
//!
//! The app already subscribes to `message.channels` / `message.groups` as
//! **user events** and holds `channels:history` / `groups:history`, because
//! that is how mentions are detected. Every post in every channel the
//! operator is in has always reached this process; [`MentionFilter`] simply
//! discarded the ones with no `<@…>` tag. Watching a channel reads what was
//! already arriving, so it needs no manifest change and no re-install.
//!
//! [`MentionFilter`]: crate::mention::MentionFilter
//! [ADR-0068]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0068-channel-watch-trigger.md

use plugin_protocol::methods::WorkflowInfo;
use plugin_sdk::WatchTrigger;
use serde_json::Value;

use crate::mention::{Mention, MentionFilter};
use crate::slack_api::SlackMessage;

/// One watched channel: the shared trigger plus the two profile-derived
/// values the Orchestrator sends alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedChannel {
    /// Channel id, author gate, pinned repo — the part every source shares.
    pub trigger: WatchTrigger,
    /// `task_id_prefix` for this workflow's profile (#397), applied by
    /// [`Mention::task_id`].
    task_id_prefix: Option<String>,
    /// `instructions_kind` for this workflow's profile (#398), which decides
    /// which instruction set the task carries (#450).
    instructions_kind: Option<String>,
}

/// Every watched channel, and the operator whose id the author gate compares
/// against.
#[derive(Debug, Clone, Default)]
pub struct WatchTriggers {
    channels: Vec<WatchedChannel>,
    operator: String,
}

impl WatchTriggers {
    /// Pair the resolved triggers with their workflows' profile-derived
    /// values. `triggers` comes from [`plugin_sdk::resolve_watch_triggers`],
    /// which already refused anything malformed.
    pub fn new(triggers: Vec<WatchTrigger>, workflows: &[WorkflowInfo], operator: &str) -> Self {
        let channels = triggers
            .into_iter()
            .map(|trigger| {
                let wf = workflows.iter().find(|w| w.workflow == trigger.workflow);
                WatchedChannel {
                    task_id_prefix: wf.and_then(|w| w.task_id_prefix.clone()),
                    instructions_kind: wf.and_then(|w| w.instructions_kind.clone()),
                    trigger,
                }
            })
            .collect();
        Self {
            channels,
            operator: operator.to_string(),
        }
    }

    /// Whether any channel is watched.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The watched channels, for the startup name check and backfill.
    pub fn channels(&self) -> &[WatchedChannel] {
        &self.channels
    }

    /// Run one raw `message` event through the watch filter table.
    ///
    /// Called **only after [`MentionFilter::assess`] declined the event**, so
    /// a post that is both a watched-channel post and a mention takes the
    /// mention path (#615 decision S1). The table is written out in full
    /// rather than leaning on that ordering: `assess` returns `None` for five
    /// different reasons, and only some of them mean "not a mention".
    ///
    /// `Some` means the event is a fresh watch task, and it has been recorded
    /// in the shared processed set — the same set the mention and reaction
    /// paths use, so one message can only ever become one task.
    pub fn assess(&self, event: &Value, filter: &mut MentionFilter) -> Option<Mention> {
        let text_of = |field: &str| event.get(field).and_then(Value::as_str);
        let channel = text_of("channel")?;
        let watched = self
            .channels
            .iter()
            .find(|w| w.trigger.channel == channel)?;
        self.admit(
            watched,
            &MessagePost {
                user: text_of("user"),
                text: text_of("text").unwrap_or_default(),
                ts: text_of("ts")?,
                thread_ts: text_of("thread_ts"),
                has_subtype: event.get("subtype").is_some(),
                has_bot_id: event.get("bot_id").is_some(),
            },
            filter,
        )
    }

    /// The same table applied to a message read back from
    /// `conversations.history` during the startup backfill.
    ///
    /// Recovery runs the **same** gate as the live path, by construction:
    /// both go through the one private `admit` table. ADR-0068 requires the
    /// author gate on this path too, and a second copy of the table is
    /// exactly how the two would drift apart.
    pub fn assess_history(
        &self,
        watched: &WatchedChannel,
        message: &SlackMessage,
        filter: &mut MentionFilter,
    ) -> Option<Mention> {
        self.admit(
            watched,
            &MessagePost {
                user: message.user.as_deref(),
                text: &message.text,
                ts: &message.ts,
                thread_ts: message.thread_ts.as_deref(),
                has_subtype: message.subtype.is_some(),
                has_bot_id: message.bot_id.is_some(),
            },
            filter,
        )
    }

    /// The watch filter table, shared by the live and backfill paths.
    fn admit(
        &self,
        watched: &WatchedChannel,
        post: &MessagePost<'_>,
        filter: &mut MentionFilter,
    ) -> Option<Mention> {
        let channel = &watched.trigger.channel;

        // 1. edits / deletions / system messages / bot posts.
        //
        // This row is also what keeps the loop closed: a watch result is
        // posted by the **bot** (#615 decision Q13), so it comes back
        // carrying `bot_id` and dies here rather than starting a task about
        // itself.
        if post.has_subtype || post.has_bot_id {
            return None;
        }
        let user = post.user?;

        // 2. top-level posts only. A reply is a comment *on* a clip, not a
        //    new one — without this row, "thanks, nice article" under a
        //    generated document opens a second task (#615 decision Q4).
        if post.thread_ts.is_some_and(|thread_ts| thread_ts != post.ts) {
            return None;
        }

        // 3. the author gate (ADR-0068): the operator, plus whoever `from`
        //    names. **Deliberately the opposite of the mention table's row
        //    2** — there, the operator's own post is excluded to break the
        //    reply loop; here it is the primary case.
        if !watched.trigger.allows(user, &self.operator) {
            tracing::debug!(
                channel,
                user,
                workflow = %watched.trigger.workflow,
                "post in a watched channel by an author the trigger does not allow; ignoring"
            );
            return None;
        }

        // 4. dedup, against the set every trigger path shares — which is also
        //    what keeps a message the backfill already recovered from being
        //    ingested a second time when Socket Mode replays it.
        if !filter.remember(format!("{channel}:{}", post.ts)) {
            return None;
        }

        Some(Mention {
            channel: channel.clone(),
            user: user.to_string(),
            text: post.text.to_string(),
            ts: post.ts.to_string(),
            // Top-level by row 2, so the task id is `{channel}:{ts}` whether
            // or not the profile adds a prefix: one post, one task.
            thread_ts: None,
            reaction: None,
            task_id_prefix: watched.task_id_prefix.clone(),
            instructions_kind: watched.instructions_kind.clone(),
            workflow: Some(watched.trigger.workflow.clone()),
            // The whole point of a watched channel: the repository is settled
            // by config, so no lookup and no classifier runs.
            repo_pin: Some(watched.trigger.repo.clone()),
        })
    }
}

/// The fields the watch table reads, from either a live Socket Mode event or
/// a `conversations.history` message.
struct MessagePost<'a> {
    user: Option<&'a str>,
    text: &'a str,
    ts: &'a str,
    thread_ts: Option<&'a str>,
    has_subtype: bool,
    has_bot_id: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn triggers(from: &[&str]) -> WatchTriggers {
        let mut trigger = json!({
            "channel": "C_CLIP", "channel_name": "clip", "repo": "docs",
        });
        if !from.is_empty() {
            trigger["from"] = json!(from);
        }
        let watch = WatchTrigger::parse(&trigger, "clip").unwrap().unwrap();
        let workflows = [WorkflowInfo {
            workflow: "clip".into(),
            trigger,
            instructions_kind: Some("implement".into()),
            task_id_prefix: Some("impl".into()),
            options: serde_json::Map::new(),
        }];
        WatchTriggers::new(vec![watch], &workflows, "U_OP")
    }

    fn post(user: &str, channel: &str) -> Value {
        json!({ "user": user, "channel": channel, "ts": "1.1", "text": "https://example.com" })
    }

    fn filter() -> MentionFilter {
        MentionFilter::new("U_OP", None)
    }

    #[test]
    fn the_operators_own_post_in_a_watched_channel_becomes_a_task() {
        let mention = triggers(&[])
            .assess(&post("U_OP", "C_CLIP"), &mut filter())
            .expect("the operator is always allowed");
        assert_eq!(mention.workflow.as_deref(), Some("clip"));
        assert_eq!(mention.repo_pin.as_deref(), Some("docs"));
        // One post, one task — prefix and all.
        assert_eq!(mention.task_id(), "impl:C_CLIP:1.1");
        assert_eq!(mention.instructions_kind.as_deref(), Some("implement"));
    }

    #[test]
    fn another_persons_post_needs_the_from_allowlist() {
        assert!(
            triggers(&[])
                .assess(&post("U_OTHER", "C_CLIP"), &mut filter())
                .is_none(),
            "the default gate is the operator alone"
        );
        assert!(
            triggers(&["U_OTHER"])
                .assess(&post("U_OTHER", "C_CLIP"), &mut filter())
                .is_some(),
            "…and `from` is what opens it"
        );
    }

    #[test]
    fn an_unwatched_channel_is_ignored() {
        assert!(
            triggers(&[])
                .assess(&post("U_OP", "C_OTHER"), &mut filter())
                .is_none()
        );
    }

    #[test]
    fn bot_posts_and_edits_are_ignored() {
        // The bot post is this plugin's own published result coming back.
        let mut bot = post("U_OP", "C_CLIP");
        bot["bot_id"] = json!("B123");
        assert!(triggers(&[]).assess(&bot, &mut filter()).is_none());

        let mut edit = post("U_OP", "C_CLIP");
        edit["subtype"] = json!("message_changed");
        assert!(triggers(&[]).assess(&edit, &mut filter()).is_none());
    }

    #[test]
    fn thread_replies_are_ignored_but_a_thread_root_is_not() {
        let mut reply = post("U_OP", "C_CLIP");
        reply["thread_ts"] = json!("0.9");
        assert!(triggers(&[]).assess(&reply, &mut filter()).is_none());

        // A root carries `thread_ts == ts` once it has replies; it is still a
        // top-level post and still the clip.
        let mut root = post("U_OP", "C_CLIP");
        root["thread_ts"] = json!("1.1");
        assert!(triggers(&[]).assess(&root, &mut filter()).is_some());
    }

    #[test]
    fn a_redelivery_is_deduped_against_the_shared_processed_set() {
        let watch = triggers(&[]);
        let mut filter = filter();
        assert!(watch.assess(&post("U_OP", "C_CLIP"), &mut filter).is_some());
        assert!(
            watch.assess(&post("U_OP", "C_CLIP"), &mut filter).is_none(),
            "the second delivery of one message must not open a second task"
        );
    }

    #[test]
    fn a_message_already_handled_as_a_mention_is_not_watched_too() {
        let watch = triggers(&[]);
        let mut filter = filter();
        // The mention path spent the key first.
        assert!(filter.remember("C_CLIP:1.1".into()));
        assert!(
            watch
                .assess(&post("U_OTHER2", "C_CLIP"), &mut filter)
                .is_none()
        );
    }
}
