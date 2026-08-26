//! Method names and their `params`/`result` types (§11 appendix A).
//!
//! Each RPC method's payload types live here. Method-name string constants are
//! in [`method`]. Direction key: **O→P** Orchestrator calls Plugin; **P→O**
//! Plugin notifies Orchestrator.
//!
//! ## Responsibility boundary (F-86)
//!
//! The worktree arrives on a **detached `HEAD`**: naming and creating the
//! branch, committing, pushing and opening the pull request are the agent's,
//! following the repository's own conventions. The orchestrator never pushes.
//! This reverses the earlier boundary (agent stops at the commit, orchestrator
//! pushes) — see ADR-0026.

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
    /// Submit one task for ingestion (P→O request, 0.1.6). The Orchestrator
    /// answers only after the task is durably persisted, so the plugin needs
    /// no buffer of its own (see [`super::TaskSubmitResult`] for the ack
    /// contract).
    pub const TASK_SUBMIT: &str = "task/submit";
    /// Ask whether a conversation is already known, before submitting to it
    /// (P→O request, 0.2.4, #242). Lets a source skip work it only needs to do
    /// for a *new* conversation — repository resolution above all, which may
    /// mean an LLM call or a question put to a human. Read-only and
    /// side-effect-free; a plugin that cannot reach it must degrade to
    /// treating the conversation as new, never to dropping the task.
    pub const TASK_LOOKUP: &str = "task/lookup";
    /// Transition source-side status (O→P, F-84).
    pub const TASK_UPDATE_STATUS: &str = "task/update_status";
    /// Claim a task for exclusive execution before dispatching it
    /// (O→P, 0.6.1, #556). Only sent to plugins whose [`Capabilities`]
    /// declare [`task_claim`](crate::Capabilities::task_claim).
    ///
    /// [`Capabilities`]: crate::Capabilities
    pub const TASK_CLAIM: &str = "task/claim";
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
    /// Capture a pane screen snapshot for timeout diagnostics (O→P, R-10).
    /// Additive since protocol 0.1.3; only called when the plugin declares the
    /// `diagnostics_snapshot` capability.
    pub const DIAGNOSTICS_SNAPSHOT: &str = "diagnostics/snapshot";
    /// Bring a session's pane to the foreground (O→P, F-94: click-to-focus).
    /// Additive since protocol 0.1.4; only called when the plugin declares the
    /// `pane_control` capability.
    pub const SESSION_FOCUS: &str = "session/focus";
    /// Release (close) a finished session's pane without cancelling (O→P,
    /// #210: worktree cleanup closes the pane before removal). Additive since
    /// protocol 0.2.1; gated on the same `pane_control` capability as
    /// `session/focus` — both are "control this pane" operations.
    pub const SESSION_RELEASE: &str = "session/release";
    /// Enumerate the panes this plugin recognizes as its own (O→P, #211:
    /// `doctor`'s orphan-pane detection). Additive since protocol 0.2.2;
    /// gated on the same `pane_control` capability as `session/release`.
    pub const SESSION_LIST: &str = "session/list";

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
    /// The plugin's own settings (the `[<name>]` table of `config.toml`) with
    /// secret references already resolved, passed through uninterpreted
    /// (F-65, #554).
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
    /// The workflows naming this plugin — as `source` or as `agent` — in
    /// `config.toml` `[[workflows]]` definition order.
    ///
    /// Carries two things a plugin needs and cannot derive: a push source's
    /// watch conditions ([`WorkflowInfo::trigger`], 0.1.6) and the workflow's
    /// plugin-owned settings ([`WorkflowInfo::options`], 0.6.0 / #554).
    ///
    /// Renamed from `triggers` in 0.6.0, and it now reaches agent plugins too:
    /// an option written on a workflow can belong to either of the plugins the
    /// workflow names, so both have to see it. `trigger` is still a task
    /// source's alone and arrives empty for an agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<WorkflowInfo>,
    /// The projects this **task_source** plugin owns (`[[projects]]` entries
    /// whose `source` is this plugin), 0.6.0 / #554. Empty for other kinds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectInfo>,
}

/// One workflow, as supplied to the plugins it names in
/// [`InitializeParams::workflows`] (0.1.6 as `TriggerInfo`, renamed and
/// extended in 0.6.0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowInfo {
    /// The workflow's `name` (`[[workflows]].name`).
    pub workflow: String,
    /// Trigger condition; plugin-defined shape, sent **verbatim** from
    /// `[[workflows]].trigger`. An empty object for a plugin named as the
    /// workflow's `agent` — triggers select tasks, which is the source's
    /// business.
    ///
    /// Until 0.6.0 the Orchestrator also injected two profile-derived keys
    /// into this table (#398); those now travel as
    /// [`instructions_kind`](Self::instructions_kind) and
    /// [`task_id_prefix`](Self::task_id_prefix), so the table is the
    /// operator's spelling and nothing else.
    pub trigger: serde_json::Value,
    /// Which instruction set the source plugin should write for this
    /// workflow's tasks (`"triage"` / `"design"` / `"implement"`), derived by
    /// the Orchestrator from the workflow's `profile` (#398). `None` means
    /// "keep your existing behaviour" — the source's own publish path already
    /// knows what to say. Always `None` for agent plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions_kind: Option<String>,
    /// The prefix a task id raised for this workflow must carry (e.g.
    /// `impl`, `books`), derived from `profile` (#397). `None` means the
    /// plain conversation id. Always `None` for agent plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id_prefix: Option<String>,
    /// The keys written on `[[workflows]]` that are **not** the Orchestrator's
    /// own (#554), verbatim.
    ///
    /// The Orchestrator cannot tell whose they are: a workflow names a source
    /// *and* an agent, and either may define a key. So it hands the whole set
    /// to both and asks each which ones it recognises — see
    /// [`InitializeResult::claimed_options`]. A key nobody claims is a typo
    /// and fails startup; one that two plugins claim is ambiguous and fails
    /// too.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub options: serde_json::Map<String, serde_json::Value>,
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
    /// The project this repository files into (`[[repositories]].project`,
    /// 0.6.0 / #554) — the `name` of one of the [`ProjectInfo`] entries.
    ///
    /// `None` means no project is configured for it, which is the normal
    /// state for a repository nobody files issues about. A source plugin uses
    /// this as the *ingest filter* too: an item from a repository not bound to
    /// one of its projects is none of its business.
    ///
    /// This replaces the reverse list each plugin used to keep
    /// (`[[projects]].repos`, #542 / ADR-0056). One repository files into one
    /// project, so binding it here makes "two plugins claim this repository"
    /// unrepresentable rather than reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// One project this plugin owns, from `config.toml`'s `[[projects]]`
/// (0.6.0, #554).
///
/// Only the entries whose `source` is this plugin are sent, so a plugin never
/// has to filter — and the Orchestrator never has to understand what is
/// inside [`options`](Self::options).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// The entry's `name`, which is what `[[repositories]].project` points at.
    pub name: String,
    /// Everything else written on the entry, verbatim.
    ///
    /// **No claim handshake here, unlike `[[workflows]]`.** A project entry
    /// names exactly one plugin (`source`), so there is no ambiguity to
    /// resolve: the whole table is that plugin's, and its own
    /// `deny_unknown_fields` is what rejects a typo.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub options: serde_json::Map<String, serde_json::Value>,
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

/// One repository this task_source files project items for, and where a new
/// item for it goes (#542).
///
/// The *forward* mapping repository → project, which nothing carried before:
/// [`RepoInfo`] and the Orchestrator's `[[repositories]]` describe a
/// repository, and a task's `repo_hint` points backwards from an item to a
/// repository. Neither answers "a new request about `totsuka` — which board
/// does it belong on".
///
/// The plugin derives this from its own config, which stays the single source
/// of truth: the Orchestrator never learns what a `project_number` or a
/// `database_id` is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimedRepo {
    /// The repository this claim is for (`[[repositories]].name`, the same id
    /// `repo_hint` uses).
    pub repo: String,
    /// Where an item for `repo` goes, **as prose addressed to an agent** —
    /// the board/database and how to file into it.
    ///
    /// Prose rather than a structured `{project_number, owner}` on purpose:
    /// the consumer is an agent's prompt, not code. A struct would force the
    /// Orchestrator to know each project's shape and to render it back into a
    /// sentence, which is the coupling this field exists to avoid — and it
    /// would need a new variant, i.e. a protocol change, for every future
    /// task_source. Nothing machine-checks the text; the triage rubric
    /// checking the agent's report is the only guard
    /// (the same class of guarantee as ADR-0045's read-only).
    pub destination: String,
}

/// `initialize` result (P→O): the plugin's version and declared capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// The plugin's own version.
    pub plugin_version: Version,
    /// Capabilities the plugin actually supports (F-33).
    pub capabilities: Capabilities,
    /// Repositories this task_source files project items for (#542, 0.5.1).
    ///
    /// Empty (or absent) means "I claim nothing", which is what every plugin
    /// predating this version says by omission — so an empty list must never
    /// be read as "this repository has no project anywhere", only as "not
    /// this plugin's". Non-task_source plugins leave it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claimed_repos: Vec<ClaimedRepo>,
    /// Which of the workflow options it was handed this plugin recognises as
    /// its own (0.6.0, #554).
    ///
    /// The Orchestrator uses the union across a workflow's `source` and
    /// `agent` to decide whether every key written on that workflow has
    /// exactly one owner. **A plugin must claim only keys it actually
    /// consumes**: claiming a key it ignores turns a typo into silence, which
    /// is the failure this handshake exists to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claimed_options: Vec<WorkflowOption>,
}

/// One `(workflow, key)` pair a plugin claims as its own (0.6.0, #554).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowOption {
    /// The workflow the key was written on (`[[workflows]].name`).
    pub workflow: String,
    /// The key, as spelled in `config.toml`.
    pub key: String,
}

/// `config/validate` params (O→P): the plugin config to validate (F-59).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidateParams {
    /// The plugin-specific config to check.
    pub config: serde_json::Value,
    /// The workflows naming this plugin, with the options written on each
    /// (0.6.0, #554) — the same list `initialize` supplied.
    ///
    /// Repeated here rather than remembered from `initialize` so the two calls
    /// stay independent: `config/validate` is the offline-ish gate an operator
    /// runs, and a plugin that answered it from state left over from a
    /// different call would be validating something other than what it was
    /// asked about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflows: Vec<WorkflowInfo>,
    /// The projects this plugin owns and the repositories bound to them
    /// (0.6.0, #554) — the same lists `initialize` supplied, for the same
    /// reason as `workflows`.
    ///
    /// A source needs both to say anything useful about its config: the
    /// boards are in `[[projects]]` and their repositories in
    /// `[[repositories]].project`, so validating the plugin's own table alone
    /// would report "no boards configured" for every correct setup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectInfo>,
    /// The Orchestrator's repositories, with their `project` bindings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<RepoInfo>,
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

/// `task/submit` params (P→O request, 0.1.6): push one task into the
/// Orchestrator in the common schema (F-01).
///
/// `task.source` carries the plugin's own source name; the Orchestrator
/// overwrites it with the plugin instance name, exactly as it does for
/// fetched tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSubmitParams {
    /// The task to ingest.
    pub task: Task,
    /// The workflow this task belongs to (`[[workflows]].name`), 0.6.0 / #554.
    ///
    /// **The plugin decides.** It already ran first-match over the workflows
    /// it was given at `initialize`, so it knows; before 0.6.0 the
    /// Orchestrator re-derived the same answer from `Task.status`/`labels`,
    /// which was the *plugin's own* report of the task — the check and the
    /// thing checked came from one place, so it protected against nothing
    /// while forcing every trigger key into the Orchestrator's vocabulary.
    ///
    /// The Orchestrator verifies the two things it can: that a workflow of
    /// this name exists, and that its `source` is the submitting plugin. A
    /// mismatch is [`TaskSubmitStatus::Rejected`].
    pub workflow: String,
}

/// The final disposition of a `task/submit` (0.1.6). Every variant is
/// **final** — the plugin must not re-submit on any of them. Retryable
/// conditions are JSON-RPC *errors* instead:
/// [`NOT_ACCEPTING`](crate::error_code::NOT_ACCEPTING) (Orchestrator
/// draining), [`SUBMIT_OVERLOADED`](crate::error_code::SUBMIT_OVERLOADED)
/// (backpressure) and
/// [`INTERNAL_ERROR`](crate::error_code::INTERNAL_ERROR) (persistence
/// failure) all mean "retry with backoff".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSubmitStatus {
    /// Persisted and queued. Sent only after the durable write committed, so
    /// a plugin that received it may forget the task entirely.
    Accepted,
    /// The task was already ingested (same `source` + task id) — an
    /// idempotent re-submit, e.g. a retry after a lost ack. Drop it.
    Duplicate,
    /// Permanently unprocessable (e.g. the named workflow does not exist,
    /// or belongs to a different source). `reason` says why; drop and log.
    Rejected,
}

/// `task/submit` result (O→P, 0.1.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSubmitResult {
    /// Final disposition (see [`TaskSubmitStatus`]).
    pub status: TaskSubmitStatus,
    /// Cause + next action, present for [`TaskSubmitStatus::Rejected`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `task/lookup` params (P→O request, 0.2.4, #242).
///
/// `task_id` is the **conversation** identity — the value the plugin would put
/// in [`Task::id`](crate::Task) — not an Orchestrator row id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLookupParams {
    /// Source plugin instance name, as in [`Task::source`](crate::Task).
    ///
    /// **Informational.** The Orchestrator answers about the *connection's*
    /// source, ignoring this field, so a plugin cannot read another source's
    /// conversations by naming it. It is not rejected on mismatch either:
    /// `task/submit` already establishes that a plugin's own notion of its
    /// source name may differ from its instance name, and overwrites rather
    /// than refuses — the same convention applies here.
    pub source: String,
    /// The conversation identity to look up.
    pub task_id: String,
}

/// `task/lookup` result (O→P, 0.2.4).
///
/// Deliberately narrow: it answers "have you seen this conversation, and which
/// repository did it settle on" and nothing else. Task state, worktree paths
/// and session ids are the Orchestrator's business — a plugin that branched on
/// them would be duplicating orchestration logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLookupResult {
    /// Whether a task with this `source` + `task_id` exists.
    pub known: bool,
    /// The repository the conversation is bound to, when one was chosen.
    ///
    /// `None` alongside `known: true` is a real state, not an oversight: the
    /// task exists but repository selection has not settled (pending human
    /// input, or an inconclusive classification). A plugin must treat it as
    /// "no hint available", not as "no repository".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `task/update_status` params (O→P, F-84).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskUpdateStatusParams {
    /// Source task id.
    pub task_id: String,
    /// Target status value (source-defined).
    pub status: String,
}

/// `task/claim` params (O→P, 0.6.1, #556): claim `task_id` for exclusive
/// execution, immediately before dispatching it.
///
/// The Orchestrator decides **when** to claim (a free execution slot exists
/// and the task is about to run); the plugin decides **how** — writing a
/// marker on the source that other members' instances will see, and
/// adjudicating races by an ordered, server-side record (F-08 keeps intake
/// control the source plugin's job). The call must be idempotent: claiming a
/// task this identity already holds answers [`TaskClaimOutcome::Won`] without
/// re-writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClaimParams {
    /// Source task id.
    pub task_id: String,
}

/// `task/claim` outcome (0.6.1, #556).
///
/// A transient failure (network, rate limit, an adjudication that cannot be
/// decided yet) is a JSON-RPC **error**, not a variant: the Orchestrator
/// leaves the task queued and retries on a later cycle, so the enum only
/// carries answers that are settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClaimOutcome {
    /// This identity holds the task; dispatch may proceed.
    Won,
    /// Another member holds it; the task must not be dispatched here.
    Lost,
    /// The claim write was silently discarded and will keep being discarded
    /// until a human fixes the configuration (e.g. GitHub ignores an
    /// assignment when the assignee lacks push access — the API answers
    /// 200 either way, so only a read-back detects it). Permanent until
    /// acted on: the Orchestrator must not retry on its own.
    Forbidden,
}

/// `task/claim` result (O→P, 0.6.1, #556).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClaimResult {
    /// How the claim settled.
    pub outcome: TaskClaimOutcome,
    /// Who holds the task, in the source's own vocabulary (a GitHub login,
    /// …). Meaningful with [`TaskClaimOutcome::Lost`]; omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
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
    /// Implementation mode: the agent branches, commits, and publishes its own
    /// work (F-86).
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
/// `worktree_path` is on a **detached `HEAD`** — the agent creates the branch
/// itself, from the repository's own naming convention (F-86, ADR-0026).
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
    /// 0.1.3: Orchestrator-issued correlation key. The plugin injects it into
    /// the launched process's environment as `TOTSUKA_JOB_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// 0.1.3: the agent-native session id to resume a past session with
    /// (`claude --resume <id>`). Used for Slack thread conversation
    /// continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_session_id: Option<String>,
    /// 0.2.3 (#196): the fully-resolved tool launch spec — plan flags, hook
    /// settings (`--settings <path>`) and resume syntax are already baked in
    /// by the Orchestrator's tool registry, and the plugin launches exactly
    /// this argv/env in the pane without interpreting it.
    ///
    /// Optional only for the historical reason that it arrived additively in
    /// 0.2.3 alongside the `hook` spec it replaced, which 0.4.0 removed.
    ///
    /// A plugin that *depends* on this spec should declare `>=0.2.3` in its
    /// manifest and **fail the dispatch** when it arrives `None`, rather than
    /// assembling an argv of its own: there is no second channel left to fall
    /// back to, and an improvised argv would omit `--settings`. That is not a
    /// blanket rule for `agent_ide` — a plugin that never reads `tool_launch`
    /// (orca drives the `orca` CLI itself) keeps a wide lower bound, because
    /// raising it would only refuse orchestrators it works with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_launch: Option<ToolLaunchSpec>,
    /// 0.4.1 (#417): the repository this task was routed to, named as the
    /// operator named it — `[[repositories]].name` from `config.toml`, the
    /// same string that appears in the worktree path, the logs and
    /// `totsuka status`. **Not** in the branch name: since ADR-0026 the agent
    /// picks that from the repository's own conventions.
    ///
    /// For **display**: an IDE plugin has no other way to say which repository
    /// an agent is working in. The worktree path cannot be reduced to it
    /// (`worktree_location` is per-repository overridable, so
    /// `{state_dir}/worktrees/{repo}/{name}` is a coincidence rather than a
    /// contract), shelling out to git would put git knowledge into an adapter
    /// that starts no processes, and `Task::repo_hint` is the source's guess —
    /// overridden by repository selection, and often absent.
    ///
    /// `None` when the Orchestrator predates 0.4.1. A plugin must degrade
    /// (omit the name) rather than refuse the dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
}

/// A fully-resolved agent-CLI launch command (additive since protocol 0.2.3,
/// #196), carried in [`TaskDispatchParams::tool_launch`]. Opaque to the
/// plugin — it starts `program` with `args` and `env` in the pane, exactly as
/// given: tool knowledge stays on the Orchestrator side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolLaunchSpec {
    /// The program to launch (e.g. `claude`).
    pub program: String,
    /// Arguments, fully resolved: mode flags, hook settings, resume id.
    pub args: Vec<String>,
    /// Environment variables to inject into the launched process
    /// (`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN`, …).
    pub env: std::collections::BTreeMap<String, String>,
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

/// `diagnostics/snapshot` params (O→P, R-10): capture the pane screen for a
/// session, used for timeout/escalation diagnostics. Additive since protocol
/// 0.1.3 (`diagnostics_snapshot` capability).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshotParams {
    /// Session id returned by `task/dispatch`.
    pub session_id: String,
}

/// `diagnostics/snapshot` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshotResult {
    /// The captured screen text. `None` when unavailable (pane gone, …) —
    /// capture failure is not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `session/focus` params (O→P, F-94): focus the pane of an existing session
/// so a notification click lands the human on the right pane. Additive since
/// protocol 0.1.4 (`pane_control` capability). The `session_id` stays opaque
/// to the Orchestrator (F-37): decoding it into a pane handle is the plugin's
/// job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFocusParams {
    /// Session id returned by `task/dispatch`.
    pub session_id: String,
}

/// `session/focus` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFocusResult {
    /// Whether the pane was focused. `false` when the pane no longer exists
    /// (task finished, pane closed, …) — a vanished pane is not an error.
    pub focused: bool,
}

/// `session/release` params (O→P, #210): close the pane of a **finished**
/// session, without the interrupt `task/cancel` sends first. Called when the
/// worktree cleanup decided to remove the task's worktree, so the pane's
/// lifetime tracks the worktree's. Additive since protocol 0.2.1
/// (`pane_control` capability).
///
/// The `expect_*` fields guard against pane-id reuse: a retention policy can
/// release days after the dispatch, by which time the (position-based) pane id
/// may name a different pane. The plugin compares every present pair against
/// the live pane and refuses to close on any mismatch; if **no** pair is
/// comparable it closes anyway (degrade-open — a certain leak on every task
/// is worse than a rare reused id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReleaseParams {
    /// Session id returned by `task/dispatch`.
    pub session_id: String,
    /// Expected pane working directory (= the task's worktree path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_cwd: Option<String>,
    /// Expected pane label. Reserved extension point — the orchestrator does
    /// not currently send it (the label format is plugin-internal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_label: Option<String>,
}

/// `session/release` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReleaseResult {
    /// Whether the pane was closed. `false` when it no longer exists or an
    /// identity check refused the close — both are normal, not errors.
    pub released: bool,
    /// Why nothing was closed, when `released` is `false` (0.4.2, #485).
    ///
    /// Absent from a plugin that predates 0.4.2, and meaningless when
    /// `released` is `true`. A caller that needs the distinction must treat
    /// absence as "cannot tell" and fall back to whatever it did before —
    /// the two cases are opposites and guessing picks the wrong one half the
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_released: Option<NotReleased>,
}

/// Why a `session/release` closed nothing (0.4.2, #485).
///
/// A bare `released: false` conflates two situations that are opposites for
/// the caller: the pane is *gone* (nothing to do) or the pane is *alive* and
/// the plugin declined to touch it. The Orchestrator releases a task's
/// previous pane before dispatching a new one, and only the second case means
/// that dispatch is about to collide with a live pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotReleased {
    /// The task holds no pane the plugin can find: it had already been
    /// closed, by this plugin's own earlier release, by the human, or by the
    /// agent exiting. Also the honest answer when the plugin cannot tell
    /// (nothing to match on, an enumeration that failed) — it is what a
    /// pre-0.4.2 plugin effectively said, and callers carry on.
    Gone,
    /// The task still holds a live pane, and it was left running: the
    /// recorded pane id did not resolve to it (the guard refused, or that id
    /// resolves to nothing), so closing anything would have risked a pane the
    /// id no longer names.
    ///
    /// This is an answer about **the task's pane**, not about whatever sits at
    /// the recorded id — that is the question a caller about to open a new
    /// pane for the task actually has. What counts as evidence is the
    /// plugin's business (herdr: a live pane on the same agent conversation,
    /// or one of its own panes on the task's worktree); a plugin that cannot
    /// look beyond the id must answer [`Gone`](Self::Gone) rather than guess.
    Refused,
    /// A reason this build does not know. Keeps a response from a newer
    /// plugin deserializable instead of failing the whole call over a field
    /// that is advisory by construction.
    #[serde(other)]
    Unknown,
}

/// `session/list` params (O→P, #211): enumerate the live panes the plugin
/// recognizes as **its own** (for herdr: panes whose label carries the
/// plugin's task marker). The plugin must never list panes a human opened for
/// unrelated work — the ownership filter lives plugin-side, where the label
/// convention is known. Additive since protocol 0.2.2 (`pane_control`
/// capability, same gate as `session/release`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionListParams {}

/// One live pane in a `session/list` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id in the same opaque format `task/dispatch` returns, so it
    /// can be passed straight back to `session/release`.
    pub session_id: String,
    /// The pane's label, when the backend reports one (for herdr:
    /// `totsuka {task_id}` — the orchestrator correlates panes to tasks
    /// through this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The pane's working directory, when the backend reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `session/list` result (P→O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionListResult {
    /// The plugin-owned live panes. Empty when none exist (not an error).
    pub sessions: Vec<SessionInfo>,
}

// ---------------------------------------------------------------------------
// notifier
// ---------------------------------------------------------------------------

/// The kind of event delivered to a notifier (F-90).
///
/// The `Escalated` and `VerificationPending` variants are additive since
/// protocol 0.1.3. A notifier built against an older protocol fails to
/// deserialize a [`NotifyParams`] carrying them, but `notify` is
/// fire-and-forget (F-93): the result is a dropped notification plus an error
/// log, never an effect on task execution.
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
    /// A task escalated to a human: block-threshold exceeded, timeout, or a
    /// correlation anomaly (0.1.3, D-02/D-03).
    Escalated,
    /// A completed task is waiting for human verification (0.1.3, D-01).
    VerificationPending,
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
            message_key: None,
            instructions: None,
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
                project: Some("tomo-prj".into()),
            }],
            projects: vec![ProjectInfo {
                name: "tomo-prj".into(),
                options: serde_json::json!({ "project_number": 7 })
                    .as_object()
                    .unwrap()
                    .clone(),
            }],
            llm: Some(LlmInfo {
                base_url: "https://openrouter.ai/api/v1".into(),
                model: "anthropic/claude-haiku-4.5".into(),
                api_key: Some("sk-or-resolved".into()),
            }),
            workflows: vec![WorkflowInfo {
                workflow: "design".into(),
                trigger: serde_json::json!({"status": "設計待ち"}),
                instructions_kind: Some("design".into()),
                task_id_prefix: None,
                options: serde_json::json!({"publish": "direct"})
                    .as_object()
                    .unwrap()
                    .clone(),
            }],
        });
        round_trip(&InitializeResult {
            plugin_version: Version::new(1, 0, 0),
            claimed_repos: Vec::new(),
            claimed_options: vec![WorkflowOption {
                workflow: "design".into(),
                key: "publish".into(),
            }],
            capabilities: Capabilities {
                hook_completion: true,
                ..Default::default()
            },
        });
        round_trip(&ConfigValidateParams {
            config: serde_json::json!({}),
            workflows: vec![],
            projects: vec![],
            repositories: vec![],
        });
        // The absent-when-unset contract for the optional fields
        // (`repositories`, `llm`, `workflows`): absent in minimal params
        // (default), omitted when unset, and ignored when a plugin is sent a
        // field it does not know.
        let old: InitializeParams =
            serde_json::from_str(r#"{"protocol_version":"0.1.0","config":{}}"#).unwrap();
        assert!(old.repositories.is_empty());
        assert!(old.projects.is_empty());
        assert!(old.llm.is_none());
        assert!(old.workflows.is_empty());
        let empty = InitializeParams {
            protocol_version: Version::new(0, 1, 2),
            config: serde_json::json!({}),
            repositories: vec![],
            projects: vec![],
            llm: None,
            workflows: vec![],
        };
        let wire = serde_json::to_string(&empty).unwrap();
        assert!(!wire.contains("repositories"));
        assert!(!wire.contains("projects"));
        assert!(!wire.contains("llm"));
        assert!(!wire.contains("workflows"));
        let ignored: ConfigValidateParams =
            serde_json::from_str(r#"{"config":{},"repositories":[{"name":"x"}]}"#).unwrap();
        assert_eq!(ignored.config, serde_json::json!({}));
        assert!(ignored.workflows.is_empty());
        // `claimed_repos` (0.5.1, #542) under the same contract, this time on
        // the *result*: a plugin predating it sends no key and must read back
        // as "claims nothing" rather than failing to deserialize.
        let old_result: InitializeResult =
            serde_json::from_str(r#"{"plugin_version":"1.0.0","capabilities":{"outputs":[]}}"#)
                .unwrap();
        assert!(old_result.claimed_repos.is_empty());
        // `claimed_options` (0.6.0, #554) omits the same way. Note what an
        // empty list means here: "I claim none of the keys I was handed", and
        // the Orchestrator turns that into an error if nobody else claims
        // them — silence is not consent.
        assert!(old_result.claimed_options.is_empty());
        assert!(
            !serde_json::to_string(&old_result)
                .unwrap()
                .contains("claimed_repos"),
            "an empty claim list must not appear on the wire, so an older \
             Orchestrator never sees an unknown key"
        );
        round_trip(&InitializeResult {
            plugin_version: Version::new(1, 0, 0),
            capabilities: Capabilities::default(),
            claimed_options: Vec::new(),
            claimed_repos: vec![ClaimedRepo {
                repo: "totsuka".into(),
                destination: "GitHub Project tomoya-k31/#7 (user)".into(),
            }],
        });
        round_trip(&ConfigValidateResult {
            valid: false,
            errors: vec!["missing socket_path → set it".into()],
        });
    }

    #[test]
    fn task_source_methods_round_trip() {
        round_trip(&TaskUpdateStatusParams {
            task_id: "42".into(),
            status: "レビュー待ち".into(),
        });
        round_trip(&TaskClaimParams {
            task_id: "I_node".into(),
        });
        round_trip(&TaskClaimResult {
            outcome: TaskClaimOutcome::Won,
            holder: None,
        });
        round_trip(&TaskClaimResult {
            outcome: TaskClaimOutcome::Lost,
            holder: Some("member-b".into()),
        });
        // The wire spelling is stable snake_case, and `Forbidden` must never
        // be confused with an error object.
        assert_eq!(
            serde_json::to_value(TaskClaimOutcome::Forbidden).unwrap(),
            serde_json::json!("forbidden"),
        );
        round_trip(&ResultPublishParams {
            task_id: "42".into(),
            content: "# Design".into(),
            format: Some("markdown".into()),
        });
        round_trip(&TaskSubmitParams {
            task: sample_task(),
            workflow: "gh-implement".into(),
        });
        round_trip(&TaskSubmitResult {
            status: TaskSubmitStatus::Accepted,
            reason: None,
        });
        round_trip(&TaskSubmitResult {
            status: TaskSubmitStatus::Rejected,
            reason: Some("workflow `gh-implement` is not defined → add it".into()),
        });
    }

    // The literal wire shapes (ack format, enum snake_case values) are pinned
    // by the golden fixtures in `tests/wire_contract.rs`.

    #[test]
    fn agent_ide_methods_round_trip() {
        round_trip(&TaskDispatchParams {
            task: sample_task(),
            worktree_path: "/wt/agent-github-42".into(),
            mode: ExecutionMode::Implement,
            extra_context: Some(serde_json::json!({"base": "main"})),
            job_id: Some("job-7".into()),
            resume_session_id: Some("claude-sess-abc".into()),
            repo_name: Some("totsuka".into()),
            tool_launch: Some(ToolLaunchSpec {
                program: "claude".into(),
                args: vec![
                    "--settings".into(),
                    "/data/totsuka/hooks/orchestrator-implement.json".into(),
                    "--resume".into(),
                    "claude-sess-abc".into(),
                ],
                env: std::collections::BTreeMap::from([
                    ("TOTSUKA_JOB_ID".to_string(), "job-7".to_string()),
                    (
                        "TOTSUKA_HOOK_ENDPOINT".to_string(),
                        "/run/totsuka/hook.sock".to_string(),
                    ),
                ]),
            }),
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
        round_trip(&DiagnosticsSnapshotParams {
            session_id: "sess-1".into(),
        });
        round_trip(&DiagnosticsSnapshotResult {
            text: Some("╭─ claude ─╮\n…".into()),
        });
        round_trip(&DiagnosticsSnapshotResult { text: None });
        round_trip(&SessionFocusParams {
            session_id: "sess-1".into(),
        });
        round_trip(&SessionFocusResult { focused: true });
        round_trip(&SessionFocusResult { focused: false });
        round_trip(&SessionReleaseParams {
            session_id: "w1:p1|sess".into(),
            expect_cwd: Some("/state/worktrees/agent-slack-1".into()),
            expect_label: Some("totsuka C1:1.0".into()),
        });
        round_trip(&SessionReleaseResult {
            released: true,
            not_released: None,
        });
        // Every reason round-trips, including the catch-all: a build that does
        // not know a reason must still deserialize the response (#485).
        for reason in [
            None,
            Some(NotReleased::Gone),
            Some(NotReleased::Refused),
            Some(NotReleased::Unknown),
        ] {
            round_trip(&SessionReleaseResult {
                released: false,
                not_released: reason,
            });
        }
        // A reason this build does not know arrives as `Unknown` rather than
        // failing the whole response — the forward-compatibility contract.
        let newer: SessionReleaseResult =
            serde_json::from_str(r#"{"released":false,"not_released":"evaporated"}"#).unwrap();
        assert_eq!(newer.not_released, Some(NotReleased::Unknown));
        // A plugin older than 0.4.2 sends no reason at all.
        let older: SessionReleaseResult = serde_json::from_str(r#"{"released":false}"#).unwrap();
        assert_eq!(older.not_released, None);
        round_trip(&SessionListParams {});
        round_trip(&SessionListResult {
            sessions: vec![
                SessionInfo {
                    session_id: "w1:p1|sess".into(),
                    label: Some("totsuka 7".into()),
                    cwd: Some("/state/worktrees/agent-slack-7".into()),
                },
                SessionInfo {
                    session_id: "w2:p1|".into(),
                    label: None,
                    cwd: None,
                },
            ],
        });
        round_trip(&SessionListResult { sessions: vec![] });
    }

    /// `session/list` (0.2.2): `label`/`cwd` follow the additive-field
    /// contract — absent in old wire (default), omitted when unset.
    #[test]
    fn session_list_optional_fields_are_optional_on_the_wire() {
        let bare: SessionInfo = serde_json::from_str(r#"{"session_id":"w1:p1|s"}"#).unwrap();
        assert!(bare.label.is_none());
        assert!(bare.cwd.is_none());
        let wire = serde_json::to_string(&SessionInfo {
            session_id: "w1:p1|s".into(),
            label: None,
            cwd: None,
        })
        .unwrap();
        assert!(!wire.contains("label"));
        assert!(!wire.contains("cwd"));
    }

    /// `session/release` (0.2.1): the `expect_*` identity fields follow the
    /// additive-field contract — absent in old wire (default), omitted when
    /// unset.
    #[test]
    fn session_release_expect_fields_are_optional_on_the_wire() {
        let old: SessionReleaseParams =
            serde_json::from_str(r#"{"session_id":"w1:p1|sess"}"#).unwrap();
        assert!(old.expect_cwd.is_none());
        assert!(old.expect_label.is_none());
        let wire = serde_json::to_string(&SessionReleaseParams {
            session_id: "w1:p1|sess".into(),
            expect_cwd: None,
            expect_label: None,
        })
        .unwrap();
        assert!(!wire.contains("expect_cwd"));
        assert!(!wire.contains("expect_label"));
    }

    /// The 0.1.3 additive fields on `task/dispatch` follow the same contract
    /// as `InitializeParams.repositories`/`llm`: absent in old wire (default),
    /// omitted when unset, ignored by an older plugin when present.
    #[test]
    fn task_dispatch_additive_fields_are_backward_compatible() {
        let old: TaskDispatchParams = serde_json::from_str(
            r#"{"task":{"id":"42","source":"github","title":"t"},
                "worktree_path":"/wt","mode":"implement"}"#,
        )
        .unwrap();
        assert!(old.job_id.is_none());
        assert!(old.resume_session_id.is_none());
        assert!(old.tool_launch.is_none());
        assert!(old.task.instructions.is_none());
        // 0.4.1 (#417): same contract. A plugin reading this must show no
        // repository name, not refuse the dispatch.
        assert!(old.repo_name.is_none());
        let unset = TaskDispatchParams {
            task: sample_task(),
            worktree_path: "/wt".into(),
            mode: ExecutionMode::Plan,
            extra_context: None,
            job_id: None,
            resume_session_id: None,
            tool_launch: None,
            repo_name: None,
        };
        let wire = serde_json::to_string(&unset).unwrap();
        assert!(!wire.contains("job_id"));
        assert!(!wire.contains("resume_session_id"));
        assert!(!wire.contains("tool_launch"));
        assert!(!wire.contains("repo_name"));
        assert!(!wire.contains("instructions"));
        // A `diagnostics/snapshot` result may omit `text` (capture failure is
        // not an error): absent deserializes to None, None stays off the wire.
        let missing: DiagnosticsSnapshotResult = serde_json::from_str("{}").unwrap();
        assert!(missing.text.is_none());
        let wire = serde_json::to_string(&DiagnosticsSnapshotResult { text: None }).unwrap();
        assert_eq!(wire, "{}");
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
        // The 0.1.3 variants round-trip like the original four.
        for event in [NotifierEvent::Escalated, NotifierEvent::VerificationPending] {
            round_trip(&NotifyParams {
                event,
                task_id: Some("42".into()),
                workflow: None,
                title: "t".into(),
                body: None,
            });
        }
    }
}
