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
    /// The emoji that started this task, when a reaction did (#396). Always
    /// `None` on the mention path — a mention-derived task must carry no
    /// `reaction:` label, or it stops matching the catch-all workflow that is
    /// meant to handle it.
    pub reaction: Option<String>,
    /// The task-id prefix the matched workflow's profile asks for (#397).
    ///
    /// Set from `task_id_prefix` in the trigger the Orchestrator sent. `None`
    /// keeps the plain conversation id, which is what `answer` — and every
    /// mention — uses.
    pub task_id_prefix: Option<String>,
    /// Which instruction set the matched workflow's profile asks for (#398),
    /// from `instructions_kind` in the trigger. This is what the pipeline
    /// branches on (#450) — **not** the prefix, which `triage` and
    /// `implement` both carry. `None` on the plain mention path, where the
    /// workflow is matched Orchestrator-side after submit.
    pub instructions_kind: Option<String>,
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
        match &self.task_id_prefix {
            // A prefixed task is a *sibling* of the conversation, not the
            // conversation (#397): it keys on the **reacted** message, so
            // reacting to two different messages in one thread starts two
            // tasks. Without the prefix these would collide with the thread's
            // `answer` task on `UNIQUE(source, source_task_id)`.
            Some(prefix) => format!("{prefix}:{}:{}", self.channel, self.ts),
            None => self.conversation_id(),
        }
    }

    /// The **conversation's** id, prefix or no prefix.
    ///
    /// What `task/lookup` must be asked with: a prefixed task's own id is by
    /// construction new, so looking *that* up always answers "unknown" and the
    /// repository the answering task already settled would be resolved from
    /// scratch — an LLM call, or a picker in front of someone who already
    /// chose (#397).
    pub fn conversation_id(&self) -> String {
        format!("{}:{}", self.channel, self.reply_ts())
    }

    /// Whether this message is the thread's root, or stands outside a thread.
    ///
    /// Decides how much context a prefixed task gets (#393 D6): reacting to the
    /// root means "implement what this thread concluded" and takes the whole
    /// conversation; reacting to one reply means "implement this" and takes
    /// only that message. A standalone message is its own whole conversation,
    /// so the two cases collapse and need no separate branch.
    pub fn is_thread_root(&self) -> bool {
        self.thread_ts.as_deref().is_none_or(|root| root == self.ts)
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
    pub(crate) fn target_user_id(&self) -> &str {
        &self.target_user_id
    }

    /// Filter row 3, exposed so the reaction trigger applies the same
    /// exclusion before spending an API call re-fetching the message.
    pub(crate) fn is_self_dm_channel(&self, channel: &str) -> bool {
        self.self_dm_channel.as_deref() == Some(channel)
    }

    /// Whether `key` was already processed, **without** recording it.
    ///
    /// The reaction trigger (#319) needs this because its work is split
    /// across an API call: it can skip a known duplicate before paying for
    /// the round trip, while still deferring [`remember`](Self::remember)
    /// until the message actually converted.
    pub(crate) fn already_processed(&self, key: &str) -> bool {
        self.processed.contains(key)
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
            // A mention never carries one: the label is what routes a task to
            // a `reaction`-triggered workflow, and a mention belongs to the
            // catch-all.
            reaction: None,
            // …and the catch-all is `answer`, whose task *is* the conversation
            // (ADR-0015). A prefix here would open a second task per message.
            task_id_prefix: None,
            // Which workflow a plain mention matches is decided
            // Orchestrator-side *after* submit, so the plugin cannot know the
            // kind here. `None` selects the reply instructions, which is what
            // the catch-all `answer` wants.
            instructions_kind: None,
        })
    }

    /// Record `key` as processed; `false` when it already was.
    ///
    /// Reachable from the reaction trigger (#319) because it shares this one
    /// set: a message reached by both a mention and an `:eyes:` reaction must
    /// become **one** task, so both paths have to dedup against the same keys.
    pub(crate) fn remember(&mut self, key: String) -> bool {
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

    fn reacted(ts: &str, thread_ts: Option<&str>, prefix: Option<&str>) -> Mention {
        Mention {
            channel: "C1".into(),
            user: "U_OTHER".into(),
            text: "方針はこれでいこう".into(),
            ts: ts.into(),
            thread_ts: thread_ts.map(str::to_string),
            reaction: Some("hammer".into()),
            task_id_prefix: prefix.map(str::to_string),
            instructions_kind: None,
        }
    }

    /// A prefixed task keys on the **reacted** message, so two reactions in one
    /// thread start two tasks (#397).
    ///
    /// The unprefixed id is the conversation's, which is why an `answer` task
    /// and an `impl:` task on the same thread do not collide on
    /// `UNIQUE(source, source_task_id)` — the collision this prefix exists to
    /// avoid.
    #[test]
    fn a_prefixed_task_keys_on_the_reacted_message_not_the_conversation() {
        let root = reacted("100.0", Some("100.0"), Some("impl"));
        assert_eq!(root.task_id(), "impl:C1:100.0");
        assert_eq!(root.conversation_id(), "C1:100.0");

        // A reply inside the same thread: a different task, same conversation.
        let reply = reacted("100.5", Some("100.0"), Some("impl"));
        assert_eq!(reply.task_id(), "impl:C1:100.5");
        assert_eq!(reply.conversation_id(), "C1:100.0");
        assert_ne!(root.task_id(), reply.task_id());
    }

    /// Without a prefix the id is the conversation's, unchanged from before
    /// #397 — that is what makes a follow-up mention continue one task
    /// (ADR-0015) rather than open a second.
    #[test]
    fn an_unprefixed_task_still_takes_the_conversation_id() {
        let reply = reacted("100.5", Some("100.0"), None);
        assert_eq!(reply.task_id(), "C1:100.0");
        assert_eq!(reply.task_id(), reply.conversation_id());
    }

    /// Root vs reply decides how much context a prefixed task gets (#393 D6).
    /// A message outside any thread is its own whole conversation, so the two
    /// cases collapse and need no separate branch.
    #[test]
    fn thread_root_and_standalone_messages_are_both_roots() {
        assert!(reacted("100.0", Some("100.0"), Some("impl")).is_thread_root());
        assert!(reacted("100.0", None, Some("impl")).is_thread_root());
        assert!(!reacted("100.5", Some("100.0"), Some("impl")).is_thread_root());
    }
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
