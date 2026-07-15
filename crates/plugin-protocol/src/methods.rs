//! Method names and their `params`/`result` types (§11 appendix A).
//!
//! Each RPC method's payload types live here. Method-name string constants are
//! in [`method`]. Direction key: **O→P** Orchestrator calls Plugin; **P→O**
//! Plugin notifies Orchestrator.
//!
//! ## Responsibility boundary (F-86)
//!
//! For `agent_ide` plugins, the agent's responsibility ends **at the commit**.
//! Pushing the branch and opening the pull request is the **Orchestrator's**
//! job (via `gh`/GitHub API), never the plugin's. Plugins must not push or open
//! PRs.

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::manifest::Capabilities;
use crate::task::Task;

/// JSON-RPC method-name constants.
pub mod method {
    // Common (all kinds).
    /// Exchange config + capabilities (O→P).
    pub const INITIALIZE: &str = "initialize";
    /// Request graceful shutdown (O→P).
    pub const SHUTDOWN: &str = "shutdown";
    /// Validate plugin-specific config (O→P, F-59).
    pub const CONFIG_VALIDATE: &str = "config/validate";

    // task_source.
    /// Fetch tasks matching a trigger (O→P).
    pub const TASKS_FETCH: &str = "tasks/fetch";
    /// Transition source-side status (O→P, F-84).
    pub const TASK_UPDATE_STATUS: &str = "task/update_status";
    /// Publish a result back to the source (O→P, F-07).
    pub const RESULT_PUBLISH: &str = "result/publish";

    // agent_ide.
    /// Dispatch a task to the agent (O→P); returns a session id.
    pub const TASK_DISPATCH: &str = "task/dispatch";
    /// Cancel an in-flight task (O→P).
    pub const TASK_CANCEL: &str = "task/cancel";
    /// Re-attach to an existing session (O→P, F-37).
    pub const SESSION_ATTACH: &str = "session/attach";
    /// Subscribe to state/log stream (O→P); plugin replies then streams.
    pub const STATE_SUBSCRIBE: &str = "state/subscribe";
    /// State/log fragment notification (P→O, F-38).
    pub const STATE_NOTIFICATION: &str = "state/notification";

    // notifier.
    /// Deliver an event notification (O→P, notification, F-90).
    pub const NOTIFY: &str = "notify";
}

// ---------------------------------------------------------------------------
// Common
// ---------------------------------------------------------------------------

/// `initialize` params (O→P): resolved plugin config + Orchestrator protocol
/// version. Secret references are already resolved by the Orchestrator (F-65).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// The Orchestrator's protocol version (F-54).
    pub protocol_version: Version,
    /// The plugin's own settings (from `plugins/{name}.toml`) with secret
    /// references already resolved, passed through uninterpreted (F-64/F-65).
    pub config: serde_json::Value,
    /// The repositories the Orchestrator is configured with (`config.toml`
    /// `[[repositories]]`), supplied to **task_source** plugins so they can
    /// resolve repositories source-side without duplicating the list in
    /// their own config (#109). Additive since protocol 0.1.1: absent from
    /// older orchestrators (serde default) and simply ignored by plugins
    /// that do not use it. Empty for non-task_source plugins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<RepoInfo>,
    /// The Orchestrator's `[llm]` (AI Gateway) settings, supplied to
    /// **task_source** plugins as a *default* for source-side classification
    /// so `base_url`/`model` need not be duplicated in their own config
    /// (#119). A plugin's own LLM table always takes precedence. Additive
    /// since protocol 0.1.2, same contract as `repositories`: absent from
    /// older orchestrators, omitted when unset, ignored by plugins that do
    /// not use it. `None` for non-task_source plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmInfo>,
}

/// One orchestrator-configured repository, as supplied to task_source
/// plugins in [`InitializeParams::repositories`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoInfo {
    /// Repository name (`[[repositories]].name` — the id `repo_hint` uses).
    pub name: String,
    /// One-line description (classifier material, F-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Local clone path, `~`/`${ENV}`-expanded by the Orchestrator (best
    /// effort: an unresolvable reference is passed through raw, so treat
    /// the path as optional material).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The Orchestrator's `[llm]` (AI Gateway) settings, as supplied to
/// task_source plugins in [`InitializeParams::llm`] (#119).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmInfo {
    /// OpenAI-compatible base URL (`/chat/completions`).
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// The API key, already resolved by the Orchestrator (F-65) — never a
    /// `keychain:`/`${ENV}` reference. `None` when the Orchestrator's `[llm]`
    /// has no `api_key_ref` (e.g. a keyless local gateway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// `initialize` result (P→O): the plugin's version and declared capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// The plugin's own version.
    pub plugin_version: Version,
    /// Capabilities the plugin actually supports (F-33).
    pub capabilities: Capabilities,
}

/// `config/validate` params (O→P): the plugin config to validate (F-59).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidateParams {
    /// The plugin-specific config to check.
    pub config: serde_json::Value,
}

/// `config/validate` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidateResult {
    /// Whether the config is valid.
    pub valid: bool,
    /// Human-readable problems ("cause + next action"), empty when valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// task_source
// ---------------------------------------------------------------------------

/// `tasks/fetch` params (O→P): the workflow trigger condition, passed raw for
/// the plugin to interpret (e.g. `{ "project_status": "実装待ち" }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TasksFetchParams {
    /// Trigger condition; plugin-defined shape.
    pub trigger: serde_json::Value,
}

/// `tasks/fetch` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TasksFetchResult {
    /// Matching tasks in the common schema (F-01).
    pub tasks: Vec<Task>,
}

/// `task/update_status` params (O→P, F-84).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskUpdateStatusParams {
    /// Source task id.
    pub task_id: String,
    /// Target status value (source-defined).
    pub status: String,
}

/// `result/publish` params (O→P, F-07): write a result back to the source
/// (Issue comment, Notion page body, …). The plugin decides the destination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultPublishParams {
    /// Source task id.
    pub task_id: String,
    /// Content to publish (e.g. a design document, usually Markdown).
    pub content: String,
    /// Content format hint (e.g. `markdown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

// ---------------------------------------------------------------------------
// agent_ide
// ---------------------------------------------------------------------------

/// Execution mode for a dispatch (F-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Design/plan mode: read-oriented, produces a design document (F-36).
    Plan,
    /// Implementation mode: the agent's work ends at the commit (F-86).
    Implement,
}

/// Agent execution state (F-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Not doing anything.
    Idle,
    /// Working.
    Running,
    /// Blocked on a human question (F-35).
    WaitingInput,
    /// Finished successfully.
    Done,
    /// Failed.
    Failed,
}

/// `task/dispatch` params (O→P): run a task in a worktree (F-31).
///
/// The plugin drives the agent up to a **commit**; it must not push or open a
/// PR (F-86).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDispatchParams {
    /// The task to work on.
    pub task: Task,
    /// Absolute path of the prepared git worktree.
    pub worktree_path: String,
    /// Plan or implement.
    pub mode: ExecutionMode,
    /// Optional extra context for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_context: Option<serde_json::Value>,
}

/// `task/dispatch` result (P→O): the session identifier for re-attach (F-37).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDispatchResult {
    /// Conversation/session id to persist and later re-attach.
    pub session_id: String,
}

/// `task/cancel` params (O→P).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCancelParams {
    /// Session id returned by `task/dispatch`.
    pub session_id: String,
}

/// `session/attach` params (O→P): reconnect to an existing session (F-37).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAttachParams {
    /// The session id to re-attach to.
    pub session_id: String,
}

/// `session/attach` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAttachResult {
    /// Whether re-attach succeeded.
    pub attached: bool,
    /// The agent's current state after re-attach.
    pub state: AgentState,
}

/// `state/subscribe` params (O→P).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSubscribeParams {
    /// Session id to stream state/log fragments for.
    pub session_id: String,
}

/// `state/notification` params (P→O, F-38): a state change and/or log fragment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateNotification {
    /// The session this notification is for.
    pub session_id: String,
    /// The agent's state at this point.
    pub state: AgentState,
    /// A log fragment, if any (persisted with the task id by the Orchestrator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_chunk: Option<String>,
}

// ---------------------------------------------------------------------------
// notifier
// ---------------------------------------------------------------------------

/// The kind of event delivered to a notifier (F-90).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifierEvent {
    /// An agent is waiting for human input.
    WaitingInput,
    /// A task finished successfully.
    Done,
    /// A task failed.
    Failed,
    /// Repository selection needs human confirmation (F-14).
    Pending,
}

/// `notify` params (O→P, notification, F-90). Delivery failures must not affect
/// task execution (F-93).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotifyParams {
    /// The event kind.
    pub event: NotifierEvent,
    /// Related task id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The workflow that produced the event, if any. Lets a notifier filter by
    /// workflow × event (F-92). Optional for backward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Notification title.
    pub title: String,
    /// Notification body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Capabilities;

    /// Assert a value survives a JSON round-trip.
    fn round_trip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, value);
    }

    fn sample_task() -> Task {
        Task {
            id: "42".into(),
            source: "github".into(),
            title: "t".into(),
            body: None,
            repo_hint: None,
            labels: vec!["bug".into()],
            priority: 1,
            status: None,
            url: None,
            assignee: None,
        }
    }

    #[test]
    fn common_methods_round_trip() {
        round_trip(&InitializeParams {
            // 0.1.2: carries `repositories` (0.1.1, #109) and `llm` (#119).
            protocol_version: Version::new(0, 1, 2),
            config: serde_json::json!({"socket_path": "/run/herdr.sock"}),
            repositories: vec![RepoInfo {
                name: "web-app".into(),
                summary: Some("customer web app".into()),
                path: Some("/repos/web-app".into()),
            }],
            llm: Some(LlmInfo {
                base_url: "https://openrouter.ai/api/v1".into(),
                model: "anthropic/claude-haiku-4.5".into(),
                api_key: Some("sk-or-resolved".into()),
            }),
        });
        round_trip(&InitializeResult {
            plugin_version: Version::new(1, 0, 0),
            capabilities: Capabilities {
                plan_mode: true,
                ..Default::default()
            },
        });
        round_trip(&ConfigValidateParams {
            config: serde_json::json!({}),
        });
        // The compatibility contract for the additive fields (`repositories`
        // since 0.1.1, `llm` since 0.1.2): absent in old params (default),
        // omitted when unset (an old plugin never sees an unknown field),
        // and ignored by an older plugin when present.
        let old: InitializeParams =
            serde_json::from_str(r#"{"protocol_version":"0.1.0","config":{}}"#).unwrap();
        assert!(old.repositories.is_empty());
        assert!(old.llm.is_none());
        let empty = InitializeParams {
            protocol_version: Version::new(0, 1, 2),
            config: serde_json::json!({}),
            repositories: vec![],
            llm: None,
        };
        let wire = serde_json::to_string(&empty).unwrap();
        assert!(!wire.contains("repositories"));
        assert!(!wire.contains("llm"));
        let ignored: ConfigValidateParams =
            serde_json::from_str(r#"{"config":{},"repositories":[{"name":"x"}]}"#).unwrap();
        assert_eq!(ignored.config, serde_json::json!({}));
        round_trip(&ConfigValidateResult {
            valid: false,
            errors: vec!["missing socket_path → set it".into()],
        });
    }

    #[test]
    fn task_source_methods_round_trip() {
        round_trip(&TasksFetchParams {
            trigger: serde_json::json!({"project_status": "実装待ち"}),
        });
        round_trip(&TasksFetchResult {
            tasks: vec![sample_task()],
        });
        round_trip(&TaskUpdateStatusParams {
            task_id: "42".into(),
            status: "レビュー待ち".into(),
        });
        round_trip(&ResultPublishParams {
            task_id: "42".into(),
            content: "# Design".into(),
            format: Some("markdown".into()),
        });
    }

    #[test]
    fn agent_ide_methods_round_trip() {
        round_trip(&TaskDispatchParams {
            task: sample_task(),
            worktree_path: "/wt/agent-github-42".into(),
            mode: ExecutionMode::Implement,
            extra_context: Some(serde_json::json!({"base": "main"})),
        });
        round_trip(&TaskDispatchResult {
            session_id: "sess-1".into(),
        });
        round_trip(&TaskCancelParams {
            session_id: "sess-1".into(),
        });
        round_trip(&SessionAttachParams {
            session_id: "sess-1".into(),
        });
        round_trip(&SessionAttachResult {
            attached: true,
            state: AgentState::WaitingInput,
        });
        round_trip(&StateSubscribeParams {
            session_id: "sess-1".into(),
        });
        round_trip(&StateNotification {
            session_id: "sess-1".into(),
            state: AgentState::Running,
            log_chunk: Some("compiling...".into()),
        });
    }

    #[test]
    fn notifier_method_round_trips() {
        round_trip(&NotifyParams {
            event: NotifierEvent::WaitingInput,
            task_id: Some("42".into()),
            workflow: Some("implement-issue".into()),
            title: "Input needed".into(),
            body: Some("The agent has a question".into()),
        });
    }

    #[test]
    fn enums_use_snake_case_wire_format() {
        assert_eq!(
            serde_json::to_string(&ExecutionMode::Implement).unwrap(),
            "\"implement\""
        );
        assert_eq!(
            serde_json::to_string(&AgentState::WaitingInput).unwrap(),
            "\"waiting_input\""
        );
        assert_eq!(
            serde_json::to_string(&NotifierEvent::Pending).unwrap(),
            "\"pending\""
        );
    }
}
