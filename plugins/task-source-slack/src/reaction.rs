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

use crate::config::normalize_reactions;
use crate::mention::Mention;
use crate::slack_api::SlackMessage;

/// Which emoji start a task (#396).
///
/// Reaction triggers are declared as `[[workflows]].trigger = { reaction =
/// "..." }`. The Orchestrator sends them at `initialize` and re-checks the
/// emoji against `reaction:<emoji>` in `Task.labels`, so a task raised this
/// way **must** carry that label or it matches no workflow and is silently
/// dropped after submission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReactionTriggers {
    /// Accepted emoji, normalized (colon-free), each with the task-id prefix
    /// its workflow's profile asks for (#397). Empty disables the trigger,
    /// which is the default.
    emojis: Vec<TriggerEmoji>,
}

/// One accepted emoji and what the workflow behind it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggerEmoji {
    /// Normalized emoji name, as `reaction_added` reports it.
    name: String,
    /// `task_id_prefix` from the trigger (#397), or `None` for the plain
    /// conversation id.
    task_id_prefix: Option<String>,
    /// `instructions_kind` from the trigger (#398) — which instruction set
    /// the matched workflow's profile wants (#450).
    instructions_kind: Option<String>,
}

impl ReactionTriggers {
    /// Build the trigger set from the workflow triggers the Orchestrator
    /// supplied at `initialize`.
    ///
    /// `Err` carries `CONFIG_INVALID` messages. The one failure mode here —
    /// one emoji claimed by two workflows — is not a warning-level
    /// degradation: it leaves the operator with a reaction whose behaviour
    /// depends on which workflow silently won.
    pub fn resolve(triggers: &[WorkflowTrigger]) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();
        let mut emojis: Vec<TriggerEmoji> = Vec::new();
        let mut claimed_by: Vec<(String, String)> = Vec::new(); // (emoji, workflow)

        for WorkflowTrigger {
            workflow,
            reaction,
            task_id_prefix,
            instructions_kind,
        } in triggers
        {
            let Some(raw) = reaction else { continue };
            // `":eyes:"` and `"eyes"` are the same trigger — Slack reports the
            // bare name, and writing the colons in TOML is the natural thing
            // to do. (👀 is `eyes`, 👁 is `eye`: different emoji, not
            // spellings of one.)
            let Some(emoji) = normalize_reactions(std::slice::from_ref(raw)).pop() else {
                errors.push(format!(
                    "workflow `{workflow}` has `trigger = {{ reaction = \"{raw}\" }}`, which is \
                     not an emoji name → write the name as Slack reports it, without colons \
                     (e.g. `eyes` for 👀)"
                ));
                continue;
            };
            if let Some((_, first)) = claimed_by.iter().find(|(name, _)| name == &emoji) {
                errors.push(format!(
                    "workflows `{first}` and `{workflow}` both trigger on `:{emoji}:` → one \
                     emoji selects one workflow; give them different emoji or merge the workflows"
                ));
                continue;
            }
            claimed_by.push((emoji.clone(), workflow.clone()));
            emojis.push(TriggerEmoji {
                name: emoji,
                task_id_prefix: task_id_prefix.clone(),
                instructions_kind: instructions_kind.clone(),
            });
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        if !emojis.is_empty() {
            return Ok(Self { emojis });
        }
        Ok(Self::default())
    }

    /// Whether any emoji is configured.
    pub fn is_empty(&self) -> bool {
        self.emojis.is_empty()
    }

    /// The configured entry for `emoji`, if it is a trigger at all.
    fn entry(&self, emoji: &str) -> Option<&TriggerEmoji> {
        self.emojis.iter().find(|e| e.name == emoji)
    }
}

/// One workflow's reaction trigger as the Orchestrator sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTrigger {
    /// `[[workflows]].name`, used only in error messages.
    pub workflow: String,
    /// `trigger.reaction`, if the workflow has one.
    pub reaction: Option<String>,
    /// `task_id_prefix`, which the Orchestrator derives from the profile
    /// (#397). Absent from an older Orchestrator → the conversation id.
    pub task_id_prefix: Option<String>,
    /// `instructions_kind`, also derived from the profile (#398). This — not
    /// the prefix — is what picks the instruction set (#450): `triage` and
    /// `implement` both carry a prefix, so branching on the prefix told a
    /// triage agent to implement and open a PR.
    pub instructions_kind: Option<String>,
}

/// Where a reaction points: the coordinates needed to re-fetch the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTarget {
    /// Channel holding the reacted-to message.
    pub channel: String,
    /// Timestamp of the reacted-to message.
    pub ts: String,
    /// The emoji to announce as a `reaction:` label. Always present — the
    /// Orchestrator re-checks it against `Task.labels`, so a task raised by a
    /// reaction that arrived without one would match no workflow.
    pub reaction: String,
    /// The task-id prefix the matched workflow's profile asks for (#397), from
    /// `task_id_prefix` in the trigger. `None` keeps the conversation id.
    pub task_id_prefix: Option<String>,
    /// The instruction set the matched workflow's profile asks for (#398),
    /// carried to the [`Mention`] so the pipeline can pick by kind (#450).
    pub instructions_kind: Option<String>,
}

impl ReactionTarget {
    /// The dedup key: **the task this reaction would produce**.
    ///
    /// `{channel}:{ts}` unprefixed — the same shape a mention uses, which is
    /// what makes one message reached both ways a single task (#319) — and
    /// `{prefix}:{channel}:{ts}` when the workflow's profile asks for a prefix
    /// (#397).
    ///
    /// #397 specified `{channel}:{ts}:{emoji}` for this. Keying on the emoji
    /// solves the case it was written for — a `:hammer:` on a message already
    /// answered via `:eyes:` must not be dropped as a redelivery, permanently,
    /// since the LRU only clears on restart — but it *also* separates two
    /// reactions that produce the **same** task, which breaks the shared-dedup
    /// invariant #319 established and costs an extra submit plus an enrich
    /// round trip per message reached both ways.
    ///
    /// Keying on the resulting task id gets both: different tasks are distinct,
    /// same task is one. The emoji only matters here inasmuch as it selects a
    /// workflow with a different prefix — which is exactly when the tasks
    /// differ.
    pub fn dedup_key(&self) -> String {
        match &self.task_id_prefix {
            Some(prefix) => format!("{prefix}:{}:{}", self.channel, self.ts),
            None => format!("{}:{}", self.channel, self.ts),
        }
    }
}

/// The event-only half of the filter. `Some` means "the operator reacted with
/// a trigger emoji to a message" — not yet that the message is usable.
pub fn reaction_target(
    event: &Value,
    target_user_id: &str,
    triggers: &ReactionTriggers,
) -> Option<ReactionTarget> {
    let text_of = |field: &str| event.get(field).and_then(Value::as_str);

    // 1. the operator only — the invariant this module exists to hold.
    if text_of("user")? != target_user_id {
        return None;
    }
    // 2. a configured trigger emoji. Slack reports `reaction` without colons.
    let reaction = text_of("reaction")?;
    let entry = triggers.entry(reaction)?;
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
        reaction: reaction.to_string(),
        task_id_prefix: entry.task_id_prefix.clone(),
        instructions_kind: entry.instructions_kind.clone(),
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
        reaction: Some(target.reaction.clone()),
        task_id_prefix: target.task_id_prefix.clone(),
        instructions_kind: target.instructions_kind.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A resolved trigger set holding `eyes`.
    fn triggers() -> ReactionTriggers {
        ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "wf".into(),
            reaction: Some("eyes".into()),
            task_id_prefix: None,
            instructions_kind: None,
        }])
        .expect("valid")
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

    /// The prefix rides from the workflow trigger to the task id (#397).
    #[test]
    fn a_prefixed_workflow_produces_a_prefixed_task() {
        let triggers = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "slack-implement".into(),
            reaction: Some("hammer".into()),
            task_id_prefix: Some("impl".into()),
            instructions_kind: None,
        }])
        .expect("valid");
        let target = reaction_target(&event("U_ME", "hammer", "message"), "U_ME", &triggers)
            .expect("accepted");
        assert_eq!(target.task_id_prefix.as_deref(), Some("impl"));
        // The dedup key follows the task, so this does not collide with the
        // `:eyes:` answer on the same message.
        assert_eq!(target.dedup_key(), "impl:C1:1.0");

        let mention = to_mention(&target, message()).expect("converted");
        assert_eq!(mention.task_id(), "impl:C1:1.0");
    }

    /// **The case #397 exists for**: a `:hammer:` on a message already answered
    /// via `:eyes:` must start the implement task.
    ///
    /// Keyed on the message alone it would be dropped as a redelivery — and
    /// permanently, since the LRU only clears on restart, so removing and
    /// re-adding the emoji would not recover it either.
    #[test]
    fn a_second_emoji_on_an_answered_message_is_not_deduped_away() {
        let triggers = ReactionTriggers::resolve(&[
            WorkflowTrigger {
                workflow: "slack-reply".into(),
                reaction: Some("eyes".into()),
                task_id_prefix: None,
                instructions_kind: None,
            },
            WorkflowTrigger {
                workflow: "slack-implement".into(),
                reaction: Some("hammer".into()),
                task_id_prefix: Some("impl".into()),
                instructions_kind: None,
            },
        ])
        .expect("valid");
        let eyes = reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers).unwrap();
        let hammer =
            reaction_target(&event("U_ME", "hammer", "message"), "U_ME", &triggers).unwrap();
        assert_ne!(eyes.dedup_key(), hammer.dedup_key());
    }

    /// …and the invariant that must survive it (#319): a message reached by
    /// both a mention and an unprefixed reaction is **one** task, because both
    /// paths land on the same key.
    #[test]
    fn an_unprefixed_reaction_shares_the_mention_paths_key() {
        let target =
            reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers()).unwrap();
        assert_eq!(
            target.dedup_key(),
            "C1:1.0",
            "same shape as a mention's key"
        );
    }

    /// An Orchestrator that sends no `task_id_prefix` — anything before #397 —
    /// keeps producing conversation-id tasks.
    #[test]
    fn no_prefix_from_the_orchestrator_means_the_conversation_id() {
        let target =
            reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers()).unwrap();
        assert_eq!(target.task_id_prefix, None);
        assert_eq!(to_mention(&target, message()).unwrap().task_id(), "C1:1.0");
    }

    #[test]
    fn a_matched_reaction_always_carries_its_label() {
        // The Orchestrator re-checks `reaction:<emoji>` against the task's
        // labels, so the label is mandatory: a task raised by a reaction that
        // arrived without one matches no workflow and vanishes after a
        // successful submit.
        let target = reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers())
            .expect("accepted");
        assert_eq!(target.reaction, "eyes");
    }

    #[test]
    fn colons_are_stripped_from_a_workflow_trigger_emoji() {
        // Slack reports `reaction` bare; `":eyes:"` is the natural TOML
        // spelling. Both must land on the same key or the trigger silently
        // never fires.
        let triggers = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "wf".into(),
            reaction: Some(":eyes:".into()),
            task_id_prefix: None,
            instructions_kind: None,
        }])
        .expect("valid");
        assert!(reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers).is_some());
    }

    #[test]
    fn one_emoji_claimed_by_two_workflows_is_rejected() {
        // First-match would pick one silently, and which one depends on
        // definition order in a file the operator was not thinking about.
        let errors = ReactionTriggers::resolve(&[
            WorkflowTrigger {
                workflow: "a".into(),
                reaction: Some("eyes".into()),
                task_id_prefix: None,
                instructions_kind: None,
            },
            WorkflowTrigger {
                workflow: "b".into(),
                reaction: Some(":eyes:".into()),
                task_id_prefix: None,
                instructions_kind: None,
            },
        ])
        .expect_err("duplicate emoji must be rejected");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains('a') && errors[0].contains('b'),
            "{errors:?}"
        );
    }

    #[test]
    fn a_workflow_reaction_that_normalizes_away_is_rejected() {
        let errors = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "wf".into(),
            reaction: Some("::".into()),
            task_id_prefix: None,
            instructions_kind: None,
        }])
        .expect_err("a non-name must be rejected");
        assert!(errors[0].contains("wf"), "{errors:?}");
    }

    #[test]
    fn workflows_without_a_reaction_trigger_leave_the_feature_off() {
        // The mention catch-all (`trigger = {}`) and status-triggered
        // workflows must not switch the reaction path on.
        let triggers = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "catch-all".into(),
            reaction: None,
            task_id_prefix: None,
            instructions_kind: None,
        }])
        .unwrap();
        assert!(triggers.is_empty());
        assert!(reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers).is_none());
    }

    #[test]
    fn the_operators_trigger_reaction_on_a_message_is_a_target() {
        let target = reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers())
            .expect("accepted");
        assert_eq!(target.channel, "C1");
        assert_eq!(target.ts, "1.0");
        // Unprefixed: the same key a mention on this message would use, so
        // one message reached both ways is still one task (#319).
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
        assert!(
            reaction_target(
                &event("U_ME", "eyes", "message"),
                "U_ME",
                &ReactionTriggers::default()
            )
            .is_none()
        );
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
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
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
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
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
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
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
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
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
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
        };
        let authorless = SlackMessage {
            user: None,
            ..message()
        };
        assert!(to_mention(&target, authorless).is_none());
    }
}
