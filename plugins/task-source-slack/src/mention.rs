//! Mention detection: the ordered filter table deciding which `message`
//! events become tasks, plus idempotent dedup of Slack's redeliveries.
//!
//! Filter order (first hit wins, per issue #105):
//!
//! 1. `subtype` / `bot_id` present → ignore (edits, deletions, system and
//!    bot posts)
//! 2. sender is the operator → ignore (self posts; breaks the loop after an
//!    approved auto-reply)
//! 3. the operator's own DM channel → ignore (defense in depth; since
//!    ADR-0074 nothing is posted there, and row 2 already covers it)
//! 4. the text names neither the operator (`<@target_user_id>`) nor a user
//!    group they belong to (`<!subteam^S…>`) → ignore
//! 5. no workflow answers mentions → ignore, **without spending the dedup
//!    key**, so a channel watch covering this channel can still claim the
//!    message (#617)
//! 6. `channel:ts` already processed → ignore (redelivery dedup; bounded
//!    LRU, lost on restart — the orchestrator's ingest is idempotent too)

use std::collections::{HashSet, VecDeque};

use serde_json::Value;

use crate::gateway_contract::{MentionTags, extract_subteam_ids};
use crate::reaction::MentionRoute;
use crate::slack_api::{SlackFile, parse_files};

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
    /// `reaction:` label, or it stops matching the `mention = true` workflow
    /// that is meant to handle it.
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
    /// `implement` both carry. `None` on the plain mention path, whose
    /// workflow carries no `instructions_kind`.
    pub instructions_kind: Option<String>,
    /// The workflow this task belongs to (0.6.0, #554), named on
    /// `task/submit`.
    ///
    /// Filled from the reaction's trigger, or — on the mention path — from
    /// the workflow that declared `trigger = { mention = true }` (ADR-0080;
    /// before that it was the first workflow requiring no reaction, which a
    /// workflow could become by omission).
    /// `None` means no workflow claims this mention, and the task is dropped
    /// rather than submitted somewhere arbitrary.
    pub workflow: Option<String>,
    /// The repository this task is pinned to by its trigger (`trigger.repo`):
    /// a channel watch (#617), or a group-scoped mention route (ADR-0081).
    /// `Some` short-circuits the whole resolution path — no `task/lookup`, no
    /// classifier, no in-thread picker — because the repository is settled in
    /// config, not per message.
    ///
    /// `None` on the catch-all mention and reaction paths, which resolve.
    pub repo_pin: Option<String>,
    /// Whether the result is posted by the bot rather than as the operator.
    ///
    /// **Carried separately from [`repo_pin`](Self::repo_pin) on purpose.**
    /// It used to be derived from it, on the reasoning that "only a watch
    /// pins a repository" — true until a group mention route could pin one
    /// too (ADR-0081). Left derived, adding `repo` to a mention route would
    /// have silently moved that route's replies from the operator's name to
    /// the bot's, which is the one thing this plugin exists not to do.
    pub post_as_bot: bool,
    /// Files attached to the message, **metadata only** ([`SlackFile`]): the
    /// plugin has no `files:read` scope, so it names them and hands over the
    /// permalink rather than the content. That link is not a dead end — an
    /// agent with a Slack tool of its own fetches the file from it. Empty for
    /// a message with no attachment.
    pub files: Vec<SlackFile>,
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
    /// `<@U…>` and `<@U…|label>` are both valid mention encodings. Shared
    /// with the Event Gateway's pre-filter (#656) so the two cannot drift:
    /// a tag this says is not a mention is one the gateway never publishes,
    /// and that record does not exist for anyone to notice.
    tags: MentionTags,
    /// The user groups the operator belongs to (#658). Resolved once at
    /// startup from `usergroups.list`; empty until then, and empty for good
    /// when the token lacks `usergroups:read` — in which case personal
    /// mentions keep working and a startup warning says group ones will not.
    subteams: HashSet<String>,
    self_dm_channel: Option<String>,
    /// Where a mention goes, in the order the routes are tried (ADR-0081).
    /// Empty means no workflow answers mentions, and they are dropped rather
    /// than submitted to a workflow nobody named.
    ///
    /// The list arrives pre-ordered from
    /// [`ReactionTriggers::resolve`](crate::reaction::ReactionTriggers::resolve),
    /// so this module does not re-implement "specific wins, ties by
    /// definition order" — it walks the list.
    mention_routes: Vec<MentionRoute>,
    processed: HashSet<String>,
    processed_order: VecDeque<String>,
}

impl MentionFilter {
    /// A filter for mentions of `target_user_id`.
    pub fn new(target_user_id: &str, mention_routes: Vec<MentionRoute>) -> Self {
        Self {
            target_user_id: target_user_id.to_string(),
            tags: MentionTags::new(target_user_id),
            subteams: HashSet::new(),
            self_dm_channel: None,
            mention_routes,
            processed: HashSet::new(),
            processed_order: VecDeque::new(),
        }
    }

    /// Register the resolved self-DM channel (filter row 3).
    pub fn set_self_dm_channel(&mut self, channel: String) {
        self.self_dm_channel = Some(channel);
    }

    /// Register the user groups the operator belongs to (filter row 4, #658).
    ///
    /// Membership is checked here rather than at the Event Gateway because the
    /// edge cannot know it without a second copy of the answer, and a stale
    /// copy drops group mentions silently (ADR-0072 decision 8).
    pub fn set_subteams(&mut self, subteams: impl IntoIterator<Item = String>) {
        self.subteams = subteams.into_iter().collect();
    }

    /// The operator's own user id — the identity the reaction trigger
    /// requires the *reacting* user to match (#319).
    pub(crate) fn target_user_id(&self) -> &str {
        &self.target_user_id
    }

    /// Filter row 3, exposed so the reaction trigger applies the same
    /// exclusion before spending an API call re-fetching the message.
    ///
    /// Largely belt-and-braces since ADR-0074: nothing posts into the
    /// operator's own DM any more, and every message there is theirs, which
    /// row 2 already excludes. Kept because dropping it would buy nothing and
    /// cost every operator a re-install (the scope it needs is granted).
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

    /// The user groups `text` names that the operator belongs to, in the
    /// order the text names them.
    ///
    /// **Which ones, not whether** (ADR-0081): the answer picks the route and
    /// then rides into the task id, so collapsing it to a bool here would
    /// throw away the only thing that tells `@oncall` from `@design`.
    ///
    /// The extraction is shared with the Event Gateway's pre-filter
    /// ([`crate::gateway_contract::extract_subteam_ids`]) so the two cannot
    /// disagree about what counts as a group tag — the gateway publishes on
    /// *any* group id, and this decides which of those are the operator's.
    fn my_subteams_in(&self, text: &str) -> Vec<String> {
        if self.subteams.is_empty() {
            return Vec::new();
        }
        extract_subteam_ids(text)
            .into_iter()
            .filter(|id| self.subteams.contains(id))
            .collect()
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
        // 3. the operator's own DM channel
        if self.self_dm_channel.as_deref() == Some(channel) {
            return None;
        }
        // 4. named, personally or through a group the operator is in.
        //
        // `<!here>` / `<!channel>` / `<!everyone>` are out of scope by
        // decision 8 of ADR-0072 — they are "to whoever is here" rather than a
        // name, and turning them into tasks makes noise dominant. Nothing here
        // matches them: neither predicate looks at a broadcast tag.
        let text = text_of("text").unwrap_or("");
        let named_groups = self.my_subteams_in(text);
        if !self.tags.matches(text) && named_groups.is_empty() {
            return None;
        }
        // 4b. which workflow this mention belongs to (ADR-0081).
        //
        // The routes are pre-ordered, so first-match here *is* "the more
        // specific route wins, ties by definition order". A personal mention
        // naming no group has an empty `named_groups`, which only the
        // catch-all claims.
        let route = self
            .mention_routes
            .iter()
            .find(|r| r.claims(&named_groups))
            .cloned();
        // 5. no workflow answers mentions.
        //
        // **Before the dedup key is spent**, because spending it here would
        // be unrecoverable: the message would be dropped for having no
        // workflow *and* be invisible to the channel watch, which may well
        // claim it (#617). "A mention outranks a watch" ranks two candidates;
        // it cannot rank one that does not exist. Reposting would not help
        // either — the key is `{channel}:{ts}`, so the burnt key belongs to
        // that message forever.
        let Some(route) = route else {
            tracing::warn!(
                channel,
                ts,
                groups = named_groups.join(","),
                "a mention arrived but no workflow claims it → add a `[[workflows]]` entry \
                 whose `projects` resolve to slack, with `trigger = {{ mention = true }}` (add \
                 `to_group` to answer only some groups). Leaving it for a channel watch if one \
                 covers this channel"
            );
            return None;
        };
        // 6. redelivery dedup
        if !self.remember(format!("{channel}:{ts}")) {
            return None;
        }

        // Asked once: the prefix and the instruction set are two sides of one
        // decision and must not be derived from different answers.
        let claimed = route.claimed_group(&named_groups);
        Some(Mention {
            channel: channel.to_string(),
            user: user.to_string(),
            text: text.to_string(),
            ts: ts.to_string(),
            thread_ts: text_of("thread_ts").map(str::to_string),
            // A mention never carries one: the label is what routes a task to
            // a `reaction`-triggered workflow, and a mention belongs to a
            // `mention = true` workflow.
            reaction: None,
            // The catch-all keeps `None`, so its task **is** the conversation
            // (ADR-0015) and a second message in the thread continues it. A
            // group route carries the matched group id, which makes its task
            // a per-message sibling instead — the same shape a reaction
            // produces, and the reason a mid-run group mention is not lost to
            // a hand-over (ADR-0081).
            task_id_prefix: route.task_id_prefix_for(claimed),
            instructions_kind: route.instructions_kind_for(claimed),
            workflow: Some(route.workflow.clone()),
            // A route may pin its repository (`trigger.repo`), which skips
            // resolution entirely — no `task/lookup`, no classifier, no
            // picker. Absent, the mention resolves as it always did.
            repo_pin: route.repo.clone(),
            // …but a pinned repository no longer implies the bot posts the
            // result. Only a channel watch does (ADR-0081 decision 7); a
            // group mention is still answered as the operator, which is the
            // whole premise of this plugin.
            post_as_bot: false,
            files: parse_files(event),
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
            workflow: Some("slack-reply".into()),
            repo_pin: None,
            post_as_bot: false,
            channel: "C1".into(),
            user: "U_OTHER".into(),
            text: "方針はこれでいこう".into(),
            ts: ts.into(),
            thread_ts: thread_ts.map(str::to_string),
            reaction: Some("hammer".into()),
            task_id_prefix: prefix.map(str::to_string),
            instructions_kind: None,
            files: Vec::new(),
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
        let mut f = MentionFilter::new("U_ME", vec![MentionRoute::catch_all("slack-reply")]);
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

    /// A filter routing `S0ONCALL` to its own workflow, everything else to
    /// the catch-all. The operator belongs to both groups.
    fn routed_filter() -> MentionFilter {
        let mut f = MentionFilter::new(
            "U_ME",
            vec![
                MentionRoute {
                    workflow: "slack-oncall".into(),
                    to_group: vec!["S0ONCALL".into()],
                    task_id_prefix: Some("books".into()),
                    instructions_kind: Some("triage".into()),
                    repo: Some("web-app".into()),
                },
                MentionRoute::catch_all("slack-reply"),
            ],
        );
        f.set_subteams(["S0ONCALL".to_string(), "S0GUILD".to_string()]);
        f
    }

    fn group_event(text: &str, ts: &str) -> Value {
        json!({
            "type": "message", "channel": "C1", "user": "U_OTHER",
            "text": text, "ts": ts
        })
    }

    /// A claimed group takes its own workflow, and carries that route's
    /// profile-derived fields and pinned repository with it.
    #[test]
    fn a_claimed_group_mention_takes_its_own_workflow() {
        let m = routed_filter()
            .assess(&group_event("<!subteam^S0ONCALL> 障害です", "200.1"))
            .expect("a claimed group mention is a task");
        assert_eq!(m.workflow.as_deref(), Some("slack-oncall"));
        assert_eq!(m.instructions_kind.as_deref(), Some("triage"));
        assert_eq!(m.repo_pin.as_deref(), Some("web-app"));
        // The prefix carries the group, so this task is a sibling of the
        // conversation rather than the conversation itself.
        assert_eq!(m.task_id(), "books:S0ONCALL:C1:200.1");
    }

    /// **A pinned repository must not change who answers.** Before ADR-0081
    /// `post_as` was derived from `repo_pin`, so this route would have started
    /// replying as the bot because it names a repo.
    #[test]
    fn a_pinned_route_is_still_answered_as_the_operator() {
        let m = routed_filter()
            .assess(&group_event("<!subteam^S0ONCALL> 見て", "200.2"))
            .expect("a task");
        assert!(
            !m.post_as_bot,
            "a group mention is answered as the operator"
        );
    }

    /// A group the operator is in but no route claims falls to the catch-all
    /// — adding one route must not silently stop the other groups.
    #[test]
    fn an_unclaimed_group_still_reaches_the_catch_all() {
        let m = routed_filter()
            .assess(&group_event("<!subteam^S0GUILD> 相談です", "200.3"))
            .expect("a task");
        assert_eq!(m.workflow.as_deref(), Some("slack-reply"));
        // The catch-all keeps the conversation id: no prefix, no pin.
        assert_eq!(m.task_id(), "C1:200.3");
        assert_eq!(m.repo_pin, None);
    }

    /// A personal mention names no group, so it reaches the catch-all even
    /// when group routes are configured.
    #[test]
    fn a_personal_mention_is_unaffected_by_group_routes() {
        let m = routed_filter()
            .assess(&group_event("<@U_ME> これどう思う", "200.4"))
            .expect("a task");
        assert_eq!(m.workflow.as_deref(), Some("slack-reply"));
        assert_eq!(m.task_id(), "C1:200.4");
    }

    /// Naming the operator *and* a claimed group takes the group route: the
    /// more specific one wins (ADR-0081 decision 2).
    #[test]
    fn a_group_route_outranks_a_personal_mention_in_the_same_message() {
        let m = routed_filter()
            .assess(&group_event("<@U_ME> <!subteam^S0ONCALL> 緊急", "200.5"))
            .expect("a task");
        assert_eq!(m.workflow.as_deref(), Some("slack-oncall"));
    }

    /// The catch-all keeps the reply instructions whatever its `profile`
    /// says, because its task **is** the conversation.
    ///
    /// Before ADR-0081 the mention path hard-coded both this and the prefix to
    /// `None`. Honouring `instructions_kind` here while still keying on the
    /// conversation would produce the one incoherent state: a thread task that
    /// opens a branch and a PR.
    #[test]
    fn the_catch_all_keeps_the_reply_instructions() {
        let mut f = MentionFilter::new(
            "U_ME",
            vec![MentionRoute {
                workflow: "slack-reply".into(),
                to_group: Vec::new(),
                task_id_prefix: Some("impl".into()),
                instructions_kind: Some("implement".into()),
                repo: None,
            }],
        );
        f.set_subteams(["S0GUILD".to_string()]);
        let m = f
            .assess(&group_event("<@U_ME> これ見て", "400.1"))
            .expect("a task");
        assert_eq!(m.task_id_prefix, None);
        assert_eq!(m.instructions_kind, None, "the two must move together");
        assert_eq!(m.task_id(), "C1:400.1");
    }

    /// …and a group route takes both, for the same reason in reverse.
    #[test]
    fn a_group_route_takes_both_prefix_and_instructions() {
        let m = routed_filter()
            .assess(&group_event("<!subteam^S0ONCALL> 障害", "400.2"))
            .expect("a task");
        assert_eq!(m.instructions_kind.as_deref(), Some("triage"));
        assert_eq!(m.task_id(), "books:S0ONCALL:C1:400.2");
    }

    /// A group the operator does **not** belong to claims nothing, even when
    /// a route names it — membership is the outer gate and stays that way.
    #[test]
    fn a_group_the_operator_is_not_in_is_still_ignored() {
        let mut f = MentionFilter::new(
            "U_ME",
            vec![MentionRoute {
                workflow: "slack-oncall".into(),
                to_group: vec!["S0OUTSIDE".into()],
                task_id_prefix: None,
                instructions_kind: None,
                repo: None,
            }],
        );
        f.set_subteams(["S0ONCALL".to_string()]);
        assert!(
            f.assess(&group_event("<!subteam^S0OUTSIDE> よろしく", "200.6"))
                .is_none()
        );
    }

    /// A filter that knows the operator belongs to one group.
    fn filter_in_group() -> MentionFilter {
        let mut f = filter();
        f.set_subteams(["S0MINE".to_string()]);
        f
    }

    fn said(text: &str) -> Value {
        let mut event = mention_event();
        event["text"] = json!(text);
        event
    }

    /// Being named through a group is being named (#658).
    #[test]
    fn a_mention_of_a_group_the_operator_is_in_becomes_a_task() {
        for text in [
            "<!subteam^S0MINE> 障害対応お願いします",
            "<!subteam^S0MINE|@team-a> 障害対応お願いします",
            // Alongside someone else's personal mention: the group tag is
            // still addressed to the operator.
            "<@U_OTHER> と <!subteam^S0MINE> で見てください",
        ] {
            assert!(
                filter_in_group().assess(&said(text)).is_some(),
                "`{text}` should have become a task"
            );
        }
    }

    /// …and only that group. A workspace has many, and picking up every one
    /// would make a busy channel unusable.
    #[test]
    fn a_group_the_operator_is_not_in_is_ignored() {
        assert!(
            filter_in_group()
                .assess(&said("<!subteam^S0THEIRS> よろしく"))
                .is_none()
        );
        // No groups resolved at all (the `usergroups:read` case): personal
        // mentions keep working, group ones do not.
        assert!(filter().assess(&said("<!subteam^S0MINE> hi")).is_none());
        assert!(filter().assess(&mention_event()).is_some());
    }

    /// Broadcasts are out of scope by ADR-0072 decision 8: they are "to
    /// whoever is here", not a name, and taking them would make noise
    /// dominant in exactly the channels worth watching.
    #[test]
    fn broadcasts_are_not_mentions() {
        for text in [
            "<!here> 明日はリリースです",
            "<!channel> 明日はリリースです",
            "<!everyone> 明日はリリースです",
        ] {
            assert!(
                filter_in_group().assess(&said(text)).is_none(),
                "`{text}` must not become a task"
            );
        }
    }

    /// A group mention is still a mention: every earlier filter row applies.
    #[test]
    fn the_earlier_filter_rows_still_outrank_a_group_mention() {
        let mut bot = said("<!subteam^S0MINE> deploy finished");
        bot["bot_id"] = json!("B0DEPLOY");
        assert!(filter_in_group().assess(&bot).is_none());

        let mut edited = said("<!subteam^S0MINE> 直しました");
        edited["subtype"] = json!("message_changed");
        assert!(filter_in_group().assess(&edited).is_none());

        let mut own = said("<!subteam^S0MINE> 自分の投稿");
        own["user"] = json!("U_ME");
        assert!(filter_in_group().assess(&own).is_none());

        // And the dedup: one message, one task, whichever predicate matched.
        let mut once = filter_in_group();
        assert!(once.assess(&said("<!subteam^S0MINE> hi")).is_some());
        assert!(once.assess(&said("<!subteam^S0MINE> hi")).is_none());
    }

    #[test]
    fn a_fresh_mention_passes_with_its_coordinates() {
        let mention = filter().assess(&mention_event()).expect("a mention");
        assert_eq!(mention.task_id(), "C1:100.0");
        assert_eq!(mention.message_key(), "C1:100.1");
        assert_eq!(mention.reply_ts(), "100.0");
        assert_eq!(mention.user, "U_OTHER");
    }

    /// A file upload with a mention as its comment: the `files` array rides
    /// the same `message` event, and dropping it is what left the agent
    /// answering "md ファイルにしました" with no file in sight.
    #[test]
    fn a_mention_carrying_a_file_keeps_its_metadata() {
        let mut event = mention_event();
        event.as_object_mut().unwrap().insert(
            "files".into(),
            json!([{"name": "auth-flow.md", "mimetype": "text/plain", "size": 2867}]),
        );
        let mention = filter().assess(&event).expect("a mention");
        assert_eq!(mention.files.len(), 1);
        assert_eq!(mention.files[0].name, "auth-flow.md");
    }

    /// The overwhelmingly common case: no `files` key at all.
    #[test]
    fn a_plain_mention_carries_no_files() {
        let mention = filter().assess(&mention_event()).expect("a mention");
        assert!(mention.files.is_empty());
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
