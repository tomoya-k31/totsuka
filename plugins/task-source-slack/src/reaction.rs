//! Reaction detection (#319): which `reaction_added` events become tasks.
//!
//! A reaction the **operator** adds starts a task the same way a mention
//! does. The two paths converge deliberately: this module's output is a
//! [`Mention`], so everything downstream — enrichment, repository resolution,
//! the approval flow — is shared code with no reaction-shaped branch in it.
//!
//! The filter runs in two halves because a `reaction_added` payload carries no
//! message body:
//!
//! 1. [`reaction_target`] — the checks that need only the event (who reacted,
//!    which emoji, what kind of item). Cheap, and they gate the API call.
//! 2. [`to_mention`] — the checks that need the re-fetched message (`subtype`
//!    / `bot_id`), plus the conversion.
//!
//! The self-DM exclusion and the dedup live on
//! [`MentionFilter`](crate::mention::MentionFilter), which owns that state and
//! shares it with this path.
//!
//! # Why the operator-only rule is an invariant
//!
//! Accepting anyone else's reaction would let a colleague start work on the
//! operator's machine by adding an emoji — a remote execution trigger in
//! everything but name. `reaction_added` reports `user` as a Slack-issued id,
//! not a client-supplied string, so the check cannot be spoofed. **There is
//! deliberately no config to relax it**; opening it up needs its own ADR, not
//! a settings key.

use serde_json::Value;

use crate::mention::Mention;
use crate::slack_api::SlackMessage;

/// Where a reaction points: the coordinates needed to re-fetch the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTarget {
    /// Channel holding the reacted-to message.
    pub channel: String,
    /// Timestamp of the reacted-to message.
    pub ts: String,
}

impl ReactionTarget {
    /// The dedup key, in the same `{channel}:{ts}` shape a mention uses, so
    /// one message reached both ways is still one task.
    pub fn dedup_key(&self) -> String {
        format!("{}:{}", self.channel, self.ts)
    }
}

/// The event-only half of the filter. `Some` means "the operator reacted with
/// a trigger emoji to a message" — not yet that the message is usable.
///
/// `triggers` is the normalized (colon-free) emoji set; an empty set disables
/// the trigger, which is the default.
pub fn reaction_target(
    event: &Value,
    target_user_id: &str,
    triggers: &[String],
) -> Option<ReactionTarget> {
    let text_of = |field: &str| event.get(field).and_then(Value::as_str);

    // 1. the operator only — the invariant this module exists to hold.
    if text_of("user")? != target_user_id {
        return None;
    }
    // 2. a configured trigger emoji. Slack reports `reaction` without colons.
    let reaction = text_of("reaction")?;
    if !triggers.iter().any(|name| name == reaction) {
        return None;
    }
    // 3. messages only — `file` and `file_comment` reactions arrive on the
    //    same event and have no message to build a task from.
    let item = event.get("item")?;
    let item_str = |field: &str| item.get(field).and_then(Value::as_str);
    if item_str("type")? != "message" {
        return None;
    }
    Some(ReactionTarget {
        channel: item_str("channel")?.to_string(),
        ts: item_str("ts")?.to_string(),
    })
}

/// The message half of the filter, plus the conversion into the shape the
/// mention pipeline consumes.
///
/// `None` when the reacted-to message is an edit, a deletion, a system
/// message or a bot post — the same exclusion mention detection applies, for
/// the same reason: there is no human-authored request in them.
///
/// Note what is **not** excluded: the message's own author. Reacting to your
/// own note to turn it into a task is a first-class use, so unlike mention
/// detection (which ignores the operator's own posts to avoid looping on an
/// approved auto-reply) this path does not look at `message.user` at all.
pub fn to_mention(target: &ReactionTarget, message: SlackMessage) -> Option<Mention> {
    if message.subtype.is_some() || message.bot_id.is_some() {
        return None;
    }
    Some(Mention {
        channel: target.channel.clone(),
        // The task's "sender" is whoever wrote the message, not whoever
        // reacted — that is the name the downstream context should show.
        user: message.user?,
        text: message.text,
        ts: message.ts,
        thread_ts: message.thread_ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn triggers() -> Vec<String> {
        vec!["eyes".to_string()]
    }

    fn event(user: &str, reaction: &str, item_type: &str) -> Value {
        json!({
            "type": "reaction_added",
            "user": user,
            "reaction": reaction,
            "item": { "type": item_type, "channel": "C1", "ts": "1.0" },
            "item_user": "U_OTHER",
            "event_ts": "2.0"
        })
    }

    fn message() -> SlackMessage {
        SlackMessage {
            user: Some("U_OTHER".to_string()),
            text: "please look at this".to_string(),
            ts: "1.0".to_string(),
            thread_ts: None,
            subtype: None,
            bot_id: None,
        }
    }

    #[test]
    fn the_operators_trigger_reaction_on_a_message_is_a_target() {
        let target = reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers())
            .expect("accepted");
        assert_eq!(target.channel, "C1");
        assert_eq!(target.ts, "1.0");
        assert_eq!(target.dedup_key(), "C1:1.0");
    }

    /// **The regression guard for the whole feature's safety story. Do not
    /// delete this test.** Accepting someone else's reaction turns an emoji
    /// into a remote execution trigger on the operator's machine.
    #[test]
    fn another_users_reaction_is_never_accepted() {
        assert!(
            reaction_target(
                &event("U_SOMEONE_ELSE", "eyes", "message"),
                "U_ME",
                &triggers()
            )
            .is_none()
        );
    }

    #[test]
    fn emoji_outside_the_trigger_set_is_ignored() {
        assert!(reaction_target(&event("U_ME", "tada", "message"), "U_ME", &triggers()).is_none());
    }

    /// The default is an empty set: no config, no trigger, no behavior change
    /// for an install that has not opted in.
    #[test]
    fn an_empty_trigger_set_accepts_nothing() {
        assert!(reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &[]).is_none());
    }

    #[test]
    fn reactions_on_non_messages_are_ignored() {
        for item_type in ["file", "file_comment"] {
            assert!(
                reaction_target(&event("U_ME", "eyes", item_type), "U_ME", &triggers()).is_none(),
                "{item_type} should be ignored"
            );
        }
    }

    #[test]
    fn a_malformed_event_is_ignored_rather_than_panicking() {
        for bad in [
            json!({ "reaction": "eyes", "item": { "type": "message", "channel": "C1", "ts": "1.0" } }),
            json!({ "user": "U_ME", "item": { "type": "message", "channel": "C1", "ts": "1.0" } }),
            json!({ "user": "U_ME", "reaction": "eyes" }),
            json!({ "user": "U_ME", "reaction": "eyes", "item": { "type": "message", "ts": "1.0" } }),
        ] {
            assert!(
                reaction_target(&bad, "U_ME", &triggers()).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_plain_message_converts_with_the_authors_identity() {
        let target = ReactionTarget {
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
        };
        let mention = to_mention(&target, message()).expect("converted");
        // The reacting user is the operator; the mention's `user` is the
        // message's author, which is the name downstream context shows.
        assert_eq!(mention.user, "U_OTHER");
        assert_eq!(mention.text, "please look at this");
        assert_eq!(mention.channel, "C1");
        assert_eq!(mention.ts, "1.0");
        assert_eq!(mention.task_id(), "C1:1.0");
    }

    /// Reacting to your own note to file it as a task is a first-class use —
    /// the opposite of the mention path, which ignores the operator's own
    /// posts to avoid looping on an approved auto-reply.
    #[test]
    fn the_operators_own_message_converts() {
        let target = ReactionTarget {
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
        };
        let own = SlackMessage {
            user: Some("U_ME".to_string()),
            ..message()
        };
        let mention = to_mention(&target, own).expect("own posts are valid targets");
        assert_eq!(mention.user, "U_ME");
    }

    #[test]
    fn threaded_messages_keep_their_thread_and_join_the_threads_task() {
        let target = ReactionTarget {
            channel: "C1".to_string(),
            ts: "2.0".to_string(),
        };
        let reply = SlackMessage {
            ts: "2.0".to_string(),
            thread_ts: Some("1.0".to_string()),
            ..message()
        };
        let mention = to_mention(&target, reply).expect("converted");
        assert_eq!(mention.thread_ts.as_deref(), Some("1.0"));
        // ADR-0015: the task is the conversation, so a reaction inside a
        // thread lands on that thread's task, not a new one.
        assert_eq!(mention.task_id(), "C1:1.0");
        assert_eq!(mention.message_key(), "C1:2.0");
    }

    #[test]
    fn edits_system_messages_and_bot_posts_are_rejected() {
        let target = ReactionTarget {
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
        };
        let edited = SlackMessage {
            subtype: Some("message_changed".to_string()),
            ..message()
        };
        assert!(to_mention(&target, edited).is_none());
        let bot = SlackMessage {
            bot_id: Some("B1".to_string()),
            ..message()
        };
        assert!(to_mention(&target, bot).is_none());
    }

    #[test]
    fn a_message_without_an_author_is_rejected() {
        let target = ReactionTarget {
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
        };
        let authorless = SlackMessage {
            user: None,
            ..message()
        };
        assert!(to_mention(&target, authorless).is_none());
    }
}
