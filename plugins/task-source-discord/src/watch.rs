//! Channel watch: every top-level post in a watched channel becomes a task.
//!
//! The vocabulary, the author gate and the backfill window are
//! [`plugin_sdk::watch`]'s ([ADR-0068]); this is the Discord half — the filter
//! table and the [`Task`] it produces.
//!
//! # Why there is no "is this a thread reply" row
//!
//! On Discord a thread **is a channel**, with its own id. A reply inside a
//! thread carries that thread's `channel_id`, not the parent's, so it never
//! matches a watched channel id in the first place — the exclusion the Slack
//! source needs an explicit row for is structural here. The one thing that
//! *would* slip through is a post in the watched channel that is a UI "reply"
//! to another message, and that is an ordinary top-level post: it belongs to
//! the channel, and treating it as a clip is right.
//!
//! [ADR-0068]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0068-channel-watch-trigger.md

use plugin_protocol::Task;
use plugin_protocol::methods::WorkflowInfo;
use plugin_sdk::WatchTrigger;

use crate::discord_api::DiscordMessage;

/// How many characters of the post go into the task title.
const TITLE_SNIPPET_CHARS: usize = 60;

/// One watched channel: the shared trigger plus the two profile-derived
/// values the Orchestrator sends alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedChannel {
    /// Channel id, author gate, pinned repo — the part every source shares.
    pub trigger: WatchTrigger,
    /// `task_id_prefix` for this workflow's profile.
    task_id_prefix: Option<String>,
    /// `instructions_kind` for this workflow's profile.
    instructions_kind: Option<String>,
}

/// Every watched channel, plus the two identities the gate needs.
#[derive(Debug, Clone, Default)]
pub struct WatchTriggers {
    channels: Vec<WatchedChannel>,
    /// The operator's own user id — who may trigger, by default.
    operator: String,
    /// The bot's own user id, from the token guard. Belt and braces beside
    /// the `bot` flag: this plugin's own result posts must never come back
    /// as clips, and one check for that is one too few.
    self_id: String,
}

impl WatchTriggers {
    /// Pair the resolved triggers with their workflows' profile-derived
    /// values. `triggers` comes from [`plugin_sdk::resolve_watch_triggers`],
    /// which already refused anything malformed.
    pub fn new(
        triggers: Vec<WatchTrigger>,
        workflows: &[WorkflowInfo],
        operator: &str,
        self_id: &str,
    ) -> Self {
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
            self_id: self_id.to_string(),
        }
    }

    /// Whether any channel is watched.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The watched channels, for the startup name check and the backfill.
    pub fn channels(&self) -> &[WatchedChannel] {
        &self.channels
    }

    /// Run one message through the watch filter table, live or backfilled.
    ///
    /// **One table for both paths.** ADR-0068 requires the author gate on the
    /// recovery path too, and a second copy is how the two drift apart.
    ///
    /// `Some` means the message is a watch task. Dedup is **not** done here:
    /// the Orchestrator's ingest is idempotent on `(source, id)`, and a
    /// re-submitted post is a `duplicate` ack that changes nothing — which is
    /// exactly what lets the backfill re-read its whole window every start.
    pub fn admit(&self, message: &DiscordMessage) -> Option<(&WatchedChannel, Task)> {
        // 1. a channel this config watches. A thread reply carries the
        //    thread's own id, so it stops here without a rule of its own.
        let watched = self
            .channels
            .iter()
            .find(|w| w.trigger.channel == message.channel_id)?;

        // 2. ordinary human posts only: no bots, no webhooks, no system
        //    messages. This is what keeps the loop closed — a watch result is
        //    posted by this very bot.
        if !message.is_human_post() {
            return None;
        }
        let author = message.author_id.as_deref()?;
        if author == self.self_id {
            return None;
        }

        // 3. the author gate (ADR-0068): the operator, plus whoever `from`
        //    names.
        if !watched.trigger.allows(author, &self.operator) {
            tracing::debug!(
                channel = %message.channel_id,
                author,
                workflow = %watched.trigger.workflow,
                "post in a watched channel by an author the trigger does not allow; ignoring"
            );
            return None;
        }

        Some((watched, self.build_task(watched, message)))
    }

    /// Normalize an admitted message into the common [`Task`] schema.
    fn build_task(&self, watched: &WatchedChannel, message: &DiscordMessage) -> Task {
        let snippet: String = message
            .content
            .replace('\n', " ")
            .chars()
            .take(TITLE_SNIPPET_CHARS)
            .collect();
        // The task id is the post: one post, one task, forever. A prefix from
        // the profile keeps it from colliding with anything else keyed on the
        // same pair.
        let id = match &watched.task_id_prefix {
            Some(prefix) => format!("{prefix}:{}:{}", message.channel_id, message.id),
            None => format!("{}:{}", message.channel_id, message.id),
        };
        Task {
            id,
            source: String::new(), // filled by the caller, which knows the instance name
            title: format!("Discord #{}: {snippet}", watched.trigger.channel_name),
            body: Some(format!(
                "## Discord への投稿\n\n- チャンネル: #{}\n- 投稿者: <@{}>\n- 本文:\n\n> {}\n",
                watched.trigger.channel_name,
                message.author_id.as_deref().unwrap_or("unknown"),
                message.content.replace('\n', "\n> ")
            )),
            repo_hint: Some(watched.trigger.repo.clone()),
            labels: Vec::new(),
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            // Deliberately absent: the id already names this single post, so
            // a second delivery of it collides on the id and stops there.
            // That is the at-most-once the watch wants.
            message_key: None,
            instructions: Some(instructions_for(watched.instructions_kind.as_deref())),
        }
    }
}

/// The agent-facing directions for a watch task.
///
/// A watch task's real instructions come from the workflow's
/// `initial_prompt`, which the operator writes; this only says what the
/// *source* needs back, which is a report carrying whatever URL the run
/// produced — the plugin has no other way to learn one exists.
fn instructions_for(kind: Option<&str>) -> String {
    let deliverable = match kind {
        Some("triage") => "the issue you filed",
        Some("implement") => "the pull request you opened",
        _ => "whatever you produced",
    };
    format!(
        "This task came from a post in a watched Discord channel. Follow the instructions at \
         the top of the task. When you are done, output a short report for the channel. The \
         report MUST contain the URL of {deliverable} — the orchestrator does not create it, \
         so this report is the only way the channel learns it exists. Write the report in the \
         same language as the post. Output the report only, with no preamble and no \
         commentary."
    )
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
        WatchTriggers::new(vec![watch], &workflows, "U_OP", "U_BOT")
    }

    fn post(author: &str, channel: &str) -> DiscordMessage {
        DiscordMessage {
            id: "M1".into(),
            channel_id: channel.into(),
            author_id: Some(author.into()),
            author_is_bot: false,
            content: "https://example.com".into(),
            kind: 0,
        }
    }

    #[test]
    fn the_operators_own_post_becomes_a_task_pinned_to_the_channels_repo() {
        let watch = triggers(&[]);
        let (watched, task) = watch
            .admit(&post("U_OP", "C_CLIP"))
            .expect("the operator is always allowed");
        assert_eq!(watched.trigger.workflow, "clip");
        assert_eq!(task.id, "impl:C_CLIP:M1");
        assert_eq!(task.repo_hint.as_deref(), Some("docs"));
        // One post, one task: no per-delivery key to reopen it with.
        assert_eq!(task.message_key, None);
        assert!(task.body.unwrap().contains("https://example.com"));
    }

    #[test]
    fn another_persons_post_needs_the_from_allowlist() {
        assert!(triggers(&[]).admit(&post("U_OTHER", "C_CLIP")).is_none());
        assert!(
            triggers(&["U_OTHER"])
                .admit(&post("U_OTHER", "C_CLIP"))
                .is_some()
        );
    }

    #[test]
    fn an_unwatched_channel_is_ignored() {
        // Also the thread case: a reply inside a thread carries the thread's
        // own id, which is not a watched channel.
        assert!(triggers(&[]).admit(&post("U_OP", "C_OTHER")).is_none());
        assert!(triggers(&[]).admit(&post("U_OP", "M1")).is_none());
    }

    #[test]
    fn the_plugins_own_result_post_does_not_come_back_as_a_clip() {
        let watch = triggers(&[]);
        // Belt: the bot flag.
        let mut flagged = post("U_BOT", "C_CLIP");
        flagged.author_is_bot = true;
        assert!(watch.admit(&flagged).is_none());
        // Braces: the same post with the flag somehow absent.
        assert!(
            watch.admit(&post("U_BOT", "C_CLIP")).is_none(),
            "the bot's own id must be refused even without the flag"
        );
    }

    #[test]
    fn system_messages_are_not_posts() {
        let mut joined = post("U_OP", "C_CLIP");
        joined.kind = 7; // USER_JOIN
        assert!(watch_admits(&joined).is_none());

        let mut thread_created = post("U_OP", "C_CLIP");
        thread_created.kind = 18; // THREAD_CREATED
        assert!(watch_admits(&thread_created).is_none());
    }

    fn watch_admits(message: &DiscordMessage) -> Option<Task> {
        triggers(&[]).admit(message).map(|(_, task)| task.clone())
    }

    #[test]
    fn the_instructions_name_the_deliverable_the_channel_cannot_otherwise_learn() {
        assert!(instructions_for(Some("implement")).contains("pull request"));
        assert!(instructions_for(Some("triage")).contains("issue"));
        // An unrecognised profile still asks for a report rather than guessing
        // a deliverable that does not exist.
        assert!(instructions_for(None).contains("report"));
        assert!(instructions_for(Some("design")).contains("report"));
    }
}
