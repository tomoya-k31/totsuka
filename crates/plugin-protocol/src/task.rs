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
    /// 0.1.3: conversation-continuation correlation key (Slack:
    /// `"{channel}:{thread_ts}"`). A later task with the same `thread_key` can
    /// resume the earlier task's session. `None` for other sources.
    ///
    /// **Superseded in 0.2.4 by [`id`](Self::id)** (#242), and removed once the
    /// epic lands. Its whole job was to say "these two tasks are one
    /// conversation" — which is now what `id` itself means, so in Slack the two
    /// carry the identical value and the "later task" this field describes no
    /// longer exists: the second delivery is the *same* task. Still accepted on
    /// the wire so plugins can drop it independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
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
            thread_key: None,
            message_key: None,
            instructions: None,
        };
        // Parse to a JSON object and assert on keys (robust against values
        // that might contain field-name substrings).
        let value: serde_json::Value = serde_json::to_value(&task).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("body"), "empty optionals omitted");
        assert!(!obj.contains_key("labels"));
        assert!(!obj.contains_key("thread_key"));
        assert!(!obj.contains_key("message_key"));
        assert!(!obj.contains_key("instructions"));
        assert!(obj.contains_key("repo_hint"));
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);
    }

    /// `thread_key` (0.1.3) round-trips when set and is absent from old wire.
    #[test]
    fn thread_key_is_additive() {
        let task = Task {
            id: "1718000000.000100".into(),
            source: "slack".into(),
            title: "追いメンション".into(),
            body: None,
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            thread_key: Some("C0123456789:1718000000.000100".into()),
            message_key: None,
            instructions: None,
        };
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(
            value["thread_key"],
            serde_json::json!("C0123456789:1718000000.000100")
        );
        let back: Task = serde_json::from_value(value).unwrap();
        assert_eq!(back, task);
        // Old wire without the field still deserializes.
        let old: Task =
            serde_json::from_str(r#"{"id":"1","source":"github","title":"t"}"#).unwrap();
        assert!(old.thread_key.is_none());
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
            thread_key: None,
            message_key: Some("C0123456789:1718000000.000300".into()),
            instructions: None,
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
            thread_key: None,
            message_key: None,
            instructions: Some("返信案を日本語で作成してください。".into()),
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
}
