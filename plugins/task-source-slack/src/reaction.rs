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
/// "..." }`. The Orchestrator sends them at `initialize` and does **not**
/// re-check them: since 0.6.0 (#554) this type is what decides which workflow
/// a reaction belongs to, and the name travels on `task/submit`. The
/// `reaction:<emoji>` label is still written, but as a record of how the task
/// was raised — dropping it no longer makes the task vanish.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReactionTriggers {
    /// Accepted emoji, normalized (colon-free), each with the task-id prefix
    /// its workflow's profile asks for (#397). Empty disables the trigger,
    /// which is the default.
    emojis: Vec<TriggerEmoji>,
    /// The workflow a plain mention belongs to: the **first** one the
    /// Orchestrator listed that does not require a reaction (0.6.0, #554).
    ///
    /// This is first-match (F-81) run here rather than Orchestrator-side. It
    /// used to be run in both places — the plugin picked the emoji, the
    /// Orchestrator re-derived the workflow from the labels the plugin had
    /// just written — and the duplicate is what forced `reaction` to be a
    /// vocabulary word in `config.toml`'s core schema.
    ///
    /// `None` means no mention workflow is configured, so a mention has
    /// nowhere to go and is dropped before it is built.
    mention_workflow: Option<String>,
}

/// One accepted emoji and what the workflow behind it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggerEmoji {
    /// Normalized emoji name, as `reaction_added` reports it.
    name: String,
    /// The workflow this emoji selects (`[[workflows]].name`), named on
    /// `task/submit` since 0.6.0.
    workflow: String,
    /// `task_id_prefix` from the trigger (#397), or `None` for the plain
    /// conversation id.
    task_id_prefix: Option<String>,
    /// `instructions_kind` from the trigger (#398) — which instruction set
    /// the matched workflow's profile wants (#450).
    instructions_kind: Option<String>,
    /// Bot ids whose posts this emoji may be used on (ADR-0079). Empty —
    /// the default — means human posts only, which is what every trigger did
    /// before the key existed.
    from_bot: Vec<String>,
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
            from_bot,
            // Read below, once, to pick the single mention workflow — this
            // loop only builds the emoji table.
            mention: _,
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
                workflow: workflow.clone(),
                task_id_prefix: task_id_prefix.clone(),
                instructions_kind: instructions_kind.clone(),
                from_bot: from_bot.clone(),
            });
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        // The workflows that declare `mention = true`: that is where a plain
        // mention goes. Two of them is the same failure as two workflows
        // claiming one emoji — first-match would pick one and say nothing —
        // so it is refused rather than resolved.
        //
        // The candidates are *declared* rather than inferred from the absence
        // of a `reaction` (server.rs `check_trigger_kind`), so a workflow can
        // no longer arrive here by omission.
        let mention_candidates: Vec<&String> = triggers
            .iter()
            .filter(|t| t.mention)
            .map(|t| &t.workflow)
            .collect();
        if mention_candidates.len() > 1 {
            errors.push(format!(
                "workflows {} all have `trigger = {{ mention = true }}` → a mention selects \
                 one workflow; leave it on the one that should answer mentions and give the \
                 others a `reaction` trigger, or merge them",
                mention_candidates
                    .iter()
                    .map(|w| format!("`{w}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return Err(errors);
        }
        let mention_workflow = mention_candidates.first().map(|w| (*w).clone());

        Ok(Self {
            emojis,
            mention_workflow,
        })
    }

    /// Whether any emoji is configured.
    pub fn is_empty(&self) -> bool {
        self.emojis.is_empty()
    }

    /// The workflow a plain mention belongs to, if one is configured.
    pub fn mention_workflow(&self) -> Option<&str> {
        self.mention_workflow.as_deref()
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
    /// `trigger.from_bot`: the bot ids whose posts this emoji may be used on.
    ///
    /// Empty is the default and means **human posts only** — the behaviour
    /// every reaction trigger had before this key existed. Declaring it per
    /// trigger rather than once per `[slack]` is deliberate: a global list
    /// would open *every* emoji to that bot at once, so adding one automated
    /// entry point would quietly change what the operator's existing emoji do.
    pub from_bot: Vec<String>,
    /// `trigger.mention`: whether a mention addressed to the operator starts
    /// this workflow.
    ///
    /// **This is a declaration, not a description of the trigger's shape.**
    /// The mention workflow used to be "the one with no `reaction`", which
    /// meant a workflow could become it by omission — including by a
    /// misspelling that left its real trigger empty. Reading a key the
    /// operator wrote is the whole point of the change.
    ///
    /// Who counts as a mention target is unchanged and lives elsewhere:
    /// `[slack] target_user_id` and the user groups `usergroups.list` resolves
    /// for them. The key carries no ids, so it cannot drift from either.
    pub mention: bool,
}

/// Where a reaction points: the coordinates needed to re-fetch the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTarget {
    /// Channel holding the reacted-to message.
    pub channel: String,
    /// Timestamp of the reacted-to message.
    pub ts: String,
    /// The emoji to announce as a `reaction:` label.
    ///
    /// Informational since 0.6.0: the Orchestrator no longer re-derives the
    /// workflow from it (the plugin names the workflow on `task/submit`), so
    /// the label is now what it always read as — a record of how the task
    /// started, for `totsuka status` and the logs.
    pub reaction: String,
    /// The workflow this emoji selects, named on `task/submit` (#554).
    pub workflow: String,
    /// The task-id prefix the matched workflow's profile asks for (#397), from
    /// `task_id_prefix` in the trigger. `None` keeps the conversation id.
    pub task_id_prefix: Option<String>,
    /// The instruction set the matched workflow's profile asks for (#398),
    /// carried to the [`Mention`] so the pipeline can pick by kind (#450).
    pub instructions_kind: Option<String>,
    /// The bot ids this emoji's workflow admits, copied from its trigger.
    ///
    /// It has to travel here rather than being consulted at the event half:
    /// `reaction_added` does not say who wrote the message, so whether the
    /// author is an admitted bot is only answerable once the body has been
    /// re-fetched — which is [`to_mention`]'s half.
    pub from_bot: Vec<String>,
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

    /// Whether a post by `bot_id` may be reacted into a task under this
    /// trigger. Ids are compared exactly: they are Slack-issued, not typed by
    /// a human, so case-folding would only widen the set.
    fn admits_bot(&self, bot_id: &str) -> bool {
        self.from_bot.iter().any(|id| id == bot_id)
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
        workflow: entry.workflow.clone(),
        task_id_prefix: entry.task_id_prefix.clone(),
        instructions_kind: entry.instructions_kind.clone(),
        from_bot: entry.from_bot.clone(),
    })
}

/// The message half of the filter, plus the conversion into the shape the
/// mention pipeline consumes.
///
/// `None` when the reacted-to message is an edit, a deletion or a system
/// message — the same exclusion mention detection applies, for the same
/// reason: there is no request in them, only a record that something changed.
///
/// # Bot posts (ADR-0079)
///
/// A bot post is excluded **unless the trigger named its `bot_id` in
/// `from_bot`**. The relaxation is deliberately on this axis and no other:
/// the gesture that starts the task is still a reaction the operator put
/// there themselves ([ADR-0025] decision 1 is untouched), so an admitted bot
/// cannot start anything by posting. What it can do is be *pointed at*.
///
/// The check is a `bot_id` match rather than a channel or a text pattern
/// because `bot_id` is Slack-issued and appears on the message itself — a
/// channel allowlist would admit every bot in that channel, and a text
/// pattern is written by whoever posts.
///
/// An admitted bot's own post carries `subtype: "bot_message"` (or no
/// subtype). Every **other** subtype stays excluded even for an admitted bot:
/// `message_changed` on a bot post is still an edit.
///
/// # What is **not** excluded
///
/// The message's own author. Reacting to your own note to turn it into a task
/// is a first-class use, so unlike mention detection (which ignores the
/// operator's own posts to avoid looping on an approved auto-reply) this path
/// does not compare `message.user` against the operator.
///
/// [ADR-0025]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0025-reaction-task-trigger.md
pub fn to_mention(target: &ReactionTarget, message: SlackMessage) -> Option<Mention> {
    let admitted_bot = message
        .bot_id
        .as_deref()
        .is_some_and(|id| target.admits_bot(id));
    if admitted_bot {
        if message
            .subtype
            .as_deref()
            .is_some_and(|s| s != "bot_message")
        {
            return None;
        }
    } else if message.subtype.is_some() || message.bot_id.is_some() {
        return None;
    }
    Some(Mention {
        channel: target.channel.clone(),
        // The task's "sender" is whoever wrote the message, not whoever
        // reacted — that is the name the downstream context should show.
        //
        // A bot post has no `user`, so its `bot_id` stands in. Downstream
        // only reads this to resolve a display name, and that lookup already
        // falls back to printing the id when it fails, so a `B…` here degrades
        // to a `B…` in the pane rather than to a broken task.
        user: message.user.or(message.bot_id)?,
        text: message.text,
        ts: message.ts,
        thread_ts: message.thread_ts,
        reaction: Some(target.reaction.clone()),
        workflow: Some(target.workflow.clone()),
        repo_pin: None,
        task_id_prefix: target.task_id_prefix.clone(),
        instructions_kind: target.instructions_kind.clone(),
        // Metadata only — see `SlackFile`. The reacted-to message is the one
        // the operator pointed at, so its attachments are exactly what the
        // body must not omit.
        files: message.files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two workflows both claiming plain mentions is the mention-path twin of
    /// two claiming one emoji: first-match would pick one silently, and since
    /// #554 this plugin — not the Orchestrator — is the one making that pick.
    #[test]
    fn two_mention_workflows_are_refused() {
        let errors = ReactionTriggers::resolve(&[
            WorkflowTrigger {
                workflow: "slack-reply".into(),
                reaction: None,
                task_id_prefix: None,
                instructions_kind: None,
                from_bot: Vec::new(),
                mention: true,
            },
            WorkflowTrigger {
                workflow: "slack-other".into(),
                reaction: None,
                task_id_prefix: None,
                instructions_kind: None,
                from_bot: Vec::new(),
                mention: true,
            },
        ])
        .expect_err("two mention workflows must be refused");
        assert!(
            errors[0].contains("slack-reply") && errors[0].contains("slack-other"),
            "{errors:?}"
        );
    }

    /// One of each resolves: the emoji picks its workflow, the mention picks
    /// the other.
    #[test]
    fn a_reaction_and_a_mention_workflow_resolve_separately() {
        let resolved = ReactionTriggers::resolve(&[
            WorkflowTrigger {
                workflow: "slack-implement".into(),
                reaction: Some("hammer".into()),
                task_id_prefix: Some("impl".into()),
                instructions_kind: Some("implement".into()),
                from_bot: Vec::new(),
                mention: false,
            },
            WorkflowTrigger {
                workflow: "slack-reply".into(),
                reaction: None,
                task_id_prefix: None,
                instructions_kind: None,
                from_bot: Vec::new(),
                mention: true,
            },
        ])
        .expect("one of each is the intended shape");
        assert_eq!(resolved.mention_workflow(), Some("slack-reply"));
        assert_eq!(
            resolved.entry("hammer").map(|e| e.workflow.as_str()),
            Some("slack-implement")
        );
    }

    /// A resolved trigger set holding `eyes`.
    fn triggers() -> ReactionTriggers {
        ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "wf".into(),
            reaction: Some("eyes".into()),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
            mention: false,
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

    /// A target whose workflow admits `bots`.
    fn bot_target(bots: &[&str]) -> ReactionTarget {
        ReactionTarget {
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: bots.iter().map(|b| (*b).to_string()).collect(),
        }
    }

    fn message() -> SlackMessage {
        SlackMessage {
            user: Some("U_OTHER".to_string()),
            text: "please look at this".to_string(),
            ts: "1.0".to_string(),
            thread_ts: None,
            subtype: None,
            bot_id: None,
            files: Vec::new(),
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
            from_bot: Vec::new(),
            mention: false,
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
                from_bot: Vec::new(),
                mention: false,
            },
            WorkflowTrigger {
                workflow: "slack-implement".into(),
                reaction: Some("hammer".into()),
                task_id_prefix: Some("impl".into()),
                instructions_kind: None,
                from_bot: Vec::new(),
                mention: false,
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
            from_bot: Vec::new(),
            mention: false,
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
                from_bot: Vec::new(),
                mention: false,
            },
            WorkflowTrigger {
                workflow: "b".into(),
                reaction: Some(":eyes:".into()),
                task_id_prefix: None,
                instructions_kind: None,
                from_bot: Vec::new(),
                mention: false,
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
            from_bot: Vec::new(),
            mention: false,
        }])
        .expect_err("a non-name must be rejected");
        assert!(errors[0].contains("wf"), "{errors:?}");
    }

    #[test]
    fn workflows_without_a_reaction_trigger_leave_the_feature_off() {
        // The mention workflow (`trigger = { mention = true }`) and
        // status-triggered workflows must not switch the reaction path on.
        let triggers = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "catch-all".into(),
            reaction: None,
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
            mention: true,
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
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
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
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
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
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "2.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
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
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
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
        assert!(
            to_mention(&target, bot).is_none(),
            "a trigger with an empty `from_bot` admits no bot — the default, and the \
             behaviour every reaction trigger had before the key existed"
        );
    }

    /// A trigger that names the bot admits its post (ADR-0079). The post has
    /// no `user`, which is what a real bot post looks like, so this also
    /// covers the `bot_id` standing in as the sender.
    #[test]
    fn a_named_bots_post_converts_with_the_bot_id_as_the_sender() {
        let target = bot_target(&["B_APPROVALS"]);
        let post = SlackMessage {
            user: None,
            bot_id: Some("B_APPROVALS".to_string()),
            subtype: Some("bot_message".to_string()),
            ..message()
        };
        let mention = to_mention(&target, post).expect("a named bot's post converts");
        assert_eq!(mention.user, "B_APPROVALS");
        assert_eq!(mention.text, "please look at this");
        assert_eq!(mention.task_id(), "C1:1.0");
    }

    /// The allowlist is exact. A second bot in the same channel is refused by
    /// the same trigger — which is the whole point of keying on `bot_id`
    /// rather than on the channel.
    #[test]
    fn a_bot_the_trigger_does_not_name_is_still_refused() {
        let target = bot_target(&["B_APPROVALS"]);
        let other = SlackMessage {
            user: None,
            bot_id: Some("B_SOMETHING_ELSE".to_string()),
            subtype: Some("bot_message".to_string()),
            ..message()
        };
        assert!(to_mention(&target, other).is_none());
    }

    /// Admitting a bot widens *which authors* count, not *which events* do.
    /// An edit is still an edit.
    #[test]
    fn an_admitted_bots_edit_is_still_refused() {
        let target = bot_target(&["B_APPROVALS"]);
        let edited = SlackMessage {
            user: None,
            bot_id: Some("B_APPROVALS".to_string()),
            subtype: Some("message_changed".to_string()),
            ..message()
        };
        assert!(to_mention(&target, edited).is_none());
    }

    /// The allowlist travels from the workflow trigger through `resolve` and
    /// `reaction_target` to the target `to_mention` reads. Without this the
    /// unit tests above would pass against a target nothing ever builds.
    #[test]
    fn the_allowlist_reaches_the_target_the_event_half_builds() {
        let triggers = ReactionTriggers::resolve(&[WorkflowTrigger {
            workflow: "wf".into(),
            reaction: Some("eyes".into()),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: vec!["B_APPROVALS".to_string()],
            mention: false,
        }])
        .expect("valid");
        let target = reaction_target(&event("U_ME", "eyes", "message"), "U_ME", &triggers)
            .expect("the operator reacted with a trigger emoji");
        assert_eq!(target.from_bot, vec!["B_APPROVALS".to_string()]);

        let post = SlackMessage {
            user: None,
            bot_id: Some("B_APPROVALS".to_string()),
            subtype: Some("bot_message".to_string()),
            ..message()
        };
        assert!(to_mention(&target, post).is_some());
    }

    #[test]
    fn a_message_without_an_author_is_rejected() {
        let target = ReactionTarget {
            workflow: "slack-implement".into(),
            channel: "C1".to_string(),
            ts: "1.0".to_string(),
            reaction: "eyes".into(),
            task_id_prefix: None,
            instructions_kind: None,
            from_bot: Vec::new(),
        };
        let authorless = SlackMessage {
            user: None,
            ..message()
        };
        assert!(to_mention(&target, authorless).is_none());
    }
}
