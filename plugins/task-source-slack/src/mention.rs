//! Mention detection: the ordered filter table deciding which `message`
//! events become tasks, plus idempotent dedup of Slack's redeliveries.
//!
//! Filter order (first hit wins, per issue #105):
//!
//! 1. `subtype` / `bot_id` present → ignore (edits, deletions, system and
//!    bot posts)
//! 2. sender is the operator → ignore (self posts; breaks the loop after an
//!    approved auto-reply)
//! 3. the self-DM record channel → ignore (defense in depth against
//!    re-detecting our own records)
//! 4. no `<@target_user_id>` in the text → ignore (mentions only)
//! 5. `channel:ts` already processed → ignore (redelivery dedup; bounded
//!    LRU, lost on restart — the orchestrator's ingest is idempotent too)

use std::collections::{HashSet, VecDeque};

use serde_json::Value;

/// Bound on the processed-id set. Old entries fall out FIFO; a redelivery
/// arriving after 1024 newer mentions is caught by the orchestrator's
/// idempotent ingest instead.
const PROCESSED_CAP: usize = 1024;

/// A message event that passed every filter: a fresh mention of the operator.
#[derive(Debug, Clone)]
pub struct Mention {
    /// Channel the mention was posted in.
    pub channel: String,
    /// Sender user id.
    pub user: String,
    /// Message text (contains the `<@…>` tag).
    pub text: String,
    /// Message timestamp (with `channel`, the stable task id).
    pub ts: String,
    /// Enclosing thread, when the mention was posted inside one.
    pub thread_ts: Option<String>,
}

impl Mention {
    /// The task id — **the conversation**, not this message (`#242`).
    ///
    /// `{channel}:{reply_ts}`, so every mention in one Slack thread names the
    /// same task and the orchestrator continues it (same worktree, same
    /// branch, same agent session) instead of opening a second one. A
    /// top-level mention has no `thread_ts`, so its `reply_ts` is its own
    /// `ts` — its task id is unchanged from before #242, which is why no
    /// existing data had to migrate.
    pub fn task_id(&self) -> String {
        format!("{}:{}", self.channel, self.reply_ts())
    }

    /// This one delivery's identity (`{channel}:{ts}`), which the
    /// orchestrator dedups re-deliveries on (`Task.message_key`, #242). It is
    /// what [`task_id`](Self::task_id) used to be — the split is exactly the
    /// point: a conversation now has many messages.
    pub fn message_key(&self) -> String {
        format!("{}:{}", self.channel, self.ts)
    }

    /// Where an approved reply goes: the enclosing thread, or a new thread
    /// rooted at the mention itself.
    pub fn reply_ts(&self) -> &str {
        self.thread_ts.as_deref().unwrap_or(&self.ts)
    }
}

/// The stateful filter: knows the operator, the self-DM channel, and what has
/// been processed already.
pub struct MentionFilter {
    target_user_id: String,
    /// `<@U…>` and `<@U…|label>` are both valid mention encodings.
    tag_closed: String,
    tag_labeled: String,
    self_dm_channel: Option<String>,
    processed: HashSet<String>,
    processed_order: VecDeque<String>,
}

impl MentionFilter {
    /// A filter for mentions of `target_user_id`.
    pub fn new(target_user_id: &str) -> Self {
        Self {
            target_user_id: target_user_id.to_string(),
            tag_closed: format!("<@{target_user_id}>"),
            tag_labeled: format!("<@{target_user_id}|"),
            self_dm_channel: None,
            processed: HashSet::new(),
            processed_order: VecDeque::new(),
        }
    }

    /// Register the resolved self-DM channel (filter row 3).
    pub fn set_self_dm_channel(&mut self, channel: String) {
        self.self_dm_channel = Some(channel);
    }

    /// The operator's own user id — the identity the reaction trigger
    /// requires the *reacting* user to match (#319).
    pub fn target_user_id(&self) -> &str {
        &self.target_user_id
    }

    /// Filter row 3, exposed so the reaction trigger applies the same
    /// exclusion before spending an API call re-fetching the message.
    pub fn is_self_dm_channel(&self, channel: &str) -> bool {
        self.self_dm_channel.as_deref() == Some(channel)
    }

    /// Run one raw `message` event through the filter table. `Some` means a
    /// fresh mention (and the event is now remembered as processed).
    pub fn assess(&mut self, event: &Value) -> Option<Mention> {
        let text_of = |field: &str| event.get(field).and_then(Value::as_str);

        // 1. edits / deletions / system messages / bot posts
        if event.get("subtype").is_some() || event.get("bot_id").is_some() {
            return None;
        }
        // A message without sender/channel/ts is nothing we can act on.
        let user = text_of("user")?;
        let channel = text_of("channel")?;
        let ts = text_of("ts")?;
        // 2. self posts (includes our own approved auto-replies)
        if user == self.target_user_id {
            return None;
        }
        // 3. the self-DM record channel
        if self.self_dm_channel.as_deref() == Some(channel) {
            return None;
        }
        // 4. mentions only
        let text = text_of("text").unwrap_or("");
        if !text.contains(&self.tag_closed) && !text.contains(&self.tag_labeled) {
            return None;
        }
        // 5. redelivery dedup
        if !self.remember(format!("{channel}:{ts}")) {
            return None;
        }

        Some(Mention {
            channel: channel.to_string(),
            user: user.to_string(),
            text: text.to_string(),
            ts: ts.to_string(),
            thread_ts: text_of("thread_ts").map(str::to_string),
        })
    }

    /// Record `key` as processed; `false` when it already was.
    ///
    /// Public because the reaction trigger (#319) shares this one set: a
    /// message reached by both a mention and an `:eyes:` reaction must become
    /// **one** task, so both paths have to dedup against the same keys.
    pub fn remember(&mut self, key: String) -> bool {
        if self.processed.contains(&key) {
            return false;
        }
        if self.processed_order.len() >= PROCESSED_CAP
            && let Some(evicted) = self.processed_order.pop_front()
        {
            self.processed.remove(&evicted);
        }
        self.processed.insert(key.clone());
        self.processed_order.push_back(key);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filter() -> MentionFilter {
        let mut f = MentionFilter::new("U_ME");
        f.set_self_dm_channel("D_SELF".to_string());
        f
    }

    fn mention_event() -> Value {
        json!({
            "type": "message",
            "channel": "C1",
            "user": "U_OTHER",
            "text": "<@U_ME> このバグ直せますか",
            "ts": "100.1",
            "thread_ts": "100.0"
        })
    }

    #[test]
    fn a_fresh_mention_passes_with_its_coordinates() {
        let mention = filter().assess(&mention_event()).expect("a mention");
        assert_eq!(mention.task_id(), "C1:100.0");
        assert_eq!(mention.message_key(), "C1:100.1");
        assert_eq!(mention.reply_ts(), "100.0");
        assert_eq!(mention.user, "U_OTHER");
    }

    #[test]
    fn top_level_mention_replies_into_its_own_thread() {
        let mut event = mention_event();
        event.as_object_mut().unwrap().remove("thread_ts");
        let mention = filter().assess(&event).expect("a mention");
        assert_eq!(mention.reply_ts(), "100.1");
    }

    #[test]
    fn an_in_thread_mention_is_a_message_of_the_thread_s_conversation() {
        // #242: the task id names the *thread*, the message key names this
        // one delivery. A reply therefore lands on the conversation the
        // thread already opened instead of starting a second one.
        let mention = filter().assess(&mention_event()).expect("a mention");
        assert_eq!(mention.task_id(), "C1:100.0");
        assert_eq!(mention.message_key(), "C1:100.1");
        assert_ne!(mention.task_id(), mention.message_key());
    }

    #[test]
    fn a_top_level_mention_is_the_first_message_of_its_own_conversation() {
        // No thread yet: the conversation is rooted at this mention, so task
        // id == message key. That equality is why #242 needed no data
        // migration — a first mention's task id is what it always was.
        let mut event = mention_event();
        event.as_object_mut().unwrap().remove("thread_ts");
        let mention = filter().assess(&event).expect("a mention");
        assert_eq!(mention.task_id(), "C1:100.1");
        assert_eq!(mention.task_id(), mention.message_key());
    }

    #[test]
    fn labeled_mention_tag_matches() {
        let mut event = mention_event();
        event["text"] = json!("<@U_ME|tomoya> check this");
        assert!(filter().assess(&event).is_some());
    }

    #[test]
    fn subtype_and_bot_posts_are_ignored() {
        let mut event = mention_event();
        event["subtype"] = json!("message_changed");
        assert!(filter().assess(&event).is_none());

        let mut event = mention_event();
        event["bot_id"] = json!("B1");
        assert!(filter().assess(&event).is_none());
    }

    #[test]
    fn own_posts_are_ignored() {
        let mut event = mention_event();
        event["user"] = json!("U_ME");
        // Even though the text mentions U_ME (e.g. quoting), never loop.
        assert!(filter().assess(&event).is_none());
    }

    #[test]
    fn self_dm_channel_is_ignored() {
        let mut event = mention_event();
        event["channel"] = json!("D_SELF");
        assert!(filter().assess(&event).is_none());
    }

    #[test]
    fn non_mentions_and_lookalike_ids_are_ignored() {
        let mut event = mention_event();
        event["text"] = json!("no mention here");
        assert!(filter().assess(&event).is_none());

        // <@U_MEX> must not match <@U_ME>.
        let mut event = mention_event();
        event["text"] = json!("<@U_MEX> hi");
        assert!(filter().assess(&event).is_none());
    }

    #[test]
    fn duplicate_delivery_yields_one_mention() {
        let mut f = filter();
        assert!(f.assess(&mention_event()).is_some());
        assert!(f.assess(&mention_event()).is_none(), "redelivery deduped");

        // A different ts is a different mention.
        let mut event = mention_event();
        event["ts"] = json!("100.2");
        assert!(f.assess(&event).is_some());
    }

    #[test]
    fn processed_set_is_bounded() {
        let mut f = filter();
        for i in 0..(PROCESSED_CAP + 10) {
            let mut event = mention_event();
            event["ts"] = json!(format!("{i}.0"));
            assert!(f.assess(&event).is_some(), "{i}");
        }
        assert!(f.processed.len() <= PROCESSED_CAP);
        assert_eq!(f.processed.len(), f.processed_order.len());
    }

    #[test]
    fn events_missing_coordinates_are_ignored() {
        for field in ["user", "channel", "ts"] {
            let mut event = mention_event();
            event.as_object_mut().unwrap().remove(field);
            assert!(filter().assess(&event).is_none(), "{field}");
        }
    }
}
