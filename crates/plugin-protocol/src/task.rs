//! The normalized [`Task`] schema shared across task sources (F-01).
//!
//! Every task source plugin maps its native items (GitHub Issues, Notion pages,
//! …) onto this common shape so the Orchestrator is source-agnostic.

use serde::{Deserialize, Serialize};

/// A task in the normalized common schema (F-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Source's own stable identifier for the **conversation** (Issue number,
    /// Notion page id, Slack `"{channel}:{thread_ts}"`).
    ///
    /// This is the task's identity: two deliveries carrying the same `id` are
    /// the same task, and the second one continues the first rather than
    /// starting a new one. Use [`message_key`](Self::message_key) to identify
    /// an individual delivery within that conversation.
    pub id: String,
    /// Source plugin instance name that produced this task (e.g. `github`).
    pub source: String,
    /// Short title.
    pub title: String,
    /// Full description / body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Repository hint from the source (e.g. the issue's repo, a Notion prop),
    /// used before falling back to LLM selection (F-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_hint: Option<String>,
    /// Labels/tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Priority; higher runs first.
    #[serde(default)]
    pub priority: i64,
    /// Source-side status (column/property value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// URL to the task in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Assignee, if any (used for ingest gating, F-08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// 0.2.4: identity of **this delivery**, as distinct from
    /// [`id`](Self::id) — the conversation it belongs to (#242). A Slack
    /// thread reply carries the thread's `id` and its own message's
    /// `"{channel}:{ts}"` here; a GitHub issue comment would carry the comment
    /// id.
    ///
    /// `None` means "this delivery *is* the whole task", and the Orchestrator
    /// falls back to `id` — so sources where one message equals one task
    /// (GitHub issues, Notion pages) need no change at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_key: Option<String>,
    /// 0.1.5: task-source-owned agent instructions (e.g. reply-crafting
    /// directions and style), separated from the human-visible `body` so hosts
    /// can deliver them out-of-band (e.g. invisible prompt-context injection).
    /// Agents that don't understand the field just see them absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// 0.7.2 (#646): a **short, human-readable** name for this task, for the
    /// identifiers an `agent_ide` gives what it creates
    /// ([`IdentifierPolicy`](crate::identifier::IdentifierPolicy)).
    ///
    /// [`id`](Self::id) cannot serve: it is the source's own key, and the
    /// shapes that reach totsuka (a Slack `channel:ts`, a base64 GitHub node
    /// id, a Notion UUID) identify nothing to a person, least of all
    /// truncated. This is the source's chance to say what a person would call
    /// the task — `web-42` for a GitHub issue, `dev-support` for a Slack
    /// thread (the channel's name, not its id).
    ///
    /// Three properties make one usable, and the source owns all three:
    ///
    /// - **Short.** It shares a budget with the task number and the digest, and
    ///   it is the part that gets cut. Around 20 characters survive in the
    ///   tightest tool (herdr's 32).
    /// - **Most-identifying part first.** Truncation keeps the head, so put the
    ///   repository or channel before the number or timestamp.
    /// - **Not unique.** Uniqueness is the digest's job. A handle that repeats
    ///   costs nothing.
    ///
    /// `None` is a normal answer, not a gap — a source with nothing short and
    /// stable to offer should leave it unset rather than invent one. Notion
    /// does: a page id is a UUID and a title is prose, so putting the title
    /// here would make a renameable string part of an identifier. Discord's
    /// *ids* are equally unreadable snowflakes, but its trigger carries a
    /// channel name, which is exactly the kind of thing this field wants. Punctuation, case and length are
    /// normalized by the policy, so a source writes the string it would show a
    /// human and nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// 0.7.5 (#734): the **existing branch** this task's work belongs on, when
    /// the source knows one — a pull request's head branch, say. The pair of
    /// [`repo_hint`](Self::repo_hint): that one says *where*, this one says
    /// *on what*.
    ///
    /// A source only ever states the branch. **What the Orchestrator does with
    /// it depends on the workflow's profile, which a source does not know:**
    /// a writable stage gets its worktree *on* the branch and commits there; a
    /// read-only stage gets one **detached at the branch's head**, because a
    /// read-only worktree found on a named branch is read as "the agent ran
    /// git" and failed for it (ADR-0045). One field, so that distinction is
    /// made in exactly one place.
    ///
    /// The name is resolved against `origin` only. A source must leave this
    /// unset for a branch that does not live there (a fork's head): the same
    /// name may exist on `origin` and mean something unrelated.
    ///
    /// Unlike `repo_hint`, this is **not advisory**. A hinted branch that
    /// cannot be found fails the task rather than falling back to the default
    /// branch — starting somewhere else is how a second pull request for the
    /// same work gets opened.
    ///
    /// `None` is the normal case and means exactly what it did before the
    /// field existed: a detached worktree at the default branch, named by the
    /// agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_omits_empty_optionals() {
        let task = Task {
            id: "42".into(),
            source: "github".into(),
            title: "Fix bug".into(),
            body: None,
            repo_hint: Some("totsuka".into()),
            labels: vec![],
            priority: 0,
            status: Some("実装待ち".into()),
            url: None,
            assignee: None,
            message_key: None,
            instructions: None,
            handle: None,
            branch_hint: None,
        };
        // Parse to a JSON object and assert on keys (robust against values
        // that might contain field-name substrings).
        let value: serde_json::Value = serde_json::to_value(&task).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("body"), "empty optionals omitted");
        assert!(!obj.contains_key("labels"));
        assert!(!obj.contains_key("message_key"));
        assert!(!obj.contains_key("instructions"));
        assert!(obj.contains_key("repo_hint"));
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);
    }

    /// `message_key` (0.2.4) round-trips when set and is absent from old wire.
    /// The pairing is the point: `id` names the conversation, `message_key`
    /// names one delivery inside it, and they differ for every reply after the
    /// first.
    #[test]
    fn message_key_is_additive_and_distinct_from_id() {
        let task = Task {
            id: "C0123456789:1718000000.000100".into(),
            source: "slack".into(),
            title: "追いメンション".into(),
            body: None,
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: Some("C0123456789:1718000000.000300".into()),
            instructions: None,
            handle: None,
            branch_hint: None,
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(
            value["message_key"],
            serde_json::json!("C0123456789:1718000000.000300")
        );
        assert_ne!(value["message_key"], value["id"]);
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);
        // Old wire without the field still deserializes — the shape every
        // one-message-per-task source (GitHub, Notion) keeps sending.
        let old: Task =
            serde_json::from_str(r#"{"id":"1","source":"github","title":"t"}"#).unwrap();
        assert!(old.message_key.is_none());
    }

    /// `instructions` (0.1.5) round-trips when set and is absent from old wire.
    #[test]
    fn instructions_are_additive() {
        let task = Task {
            id: "C1:1.0".into(),
            source: "slack".into(),
            title: "reply".into(),
            body: Some("## メンション\n…".into()),
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: None,
            instructions: Some("返信案を日本語で作成してください。".into()),
            handle: None,
            branch_hint: None,
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(
            value["instructions"],
            serde_json::json!("返信案を日本語で作成してください。")
        );
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);
        // Old wire without the field still deserializes.
        let old: Task =
            serde_json::from_str(r#"{"id":"1","source":"github","title":"t"}"#).unwrap();
        assert!(old.instructions.is_none());
    }

    /// `branch_hint` (0.7.5) round-trips when set, stays off the wire when
    /// unset, and is absent from every older source's tasks.
    #[test]
    fn branch_hint_is_additive() {
        let task = Task {
            id: "PR_kwDO1".into(),
            source: "github".into(),
            title: "chore(deps): update setup-uv".into(),
            body: None,
            repo_hint: Some("zenn-blog".into()),
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: None,
            instructions: None,
            handle: None,
            branch_hint: Some("renovate/setup-uv-10.x".into()),
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(
            value["branch_hint"],
            serde_json::json!("renovate/setup-uv-10.x")
        );
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);

        let unset = Task {
            branch_hint: None,
            ..task
        };
        let value = serde_json::to_value(&unset).unwrap();
        assert!(!value.as_object().unwrap().contains_key("branch_hint"));
        let old: Task =
            serde_json::from_str(r#"{"id":"1","source":"github","title":"t"}"#).unwrap();
        assert!(old.branch_hint.is_none());
    }
}
