//! `config.toml` schema (F-60, F-61) and parsing.
//!
//! **One file.** Both layers live in `config.toml` (#554): the
//! Orchestrator-interpreted keys are the named fields of [`RootConfig`], and
//! every other top-level table is one plugin's own settings, held
//! uninterpreted in [`RootConfig::plugin_settings`]. The `plugins/{name}.toml`
//! split that F-64 used to mandate is gone — it expressed ownership through
//! file location, which is exactly the thing that had no way to reach inside a
//! core structure like `[[workflows]]`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::domain::{OutputPolicy, Profile, VerificationMode, WorkflowMode};
use crate::tool::ToolKind;

/// The current supported config schema version (§10.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Default global concurrent-task limit when `max_concurrency` is omitted
/// (F-40).
pub const DEFAULT_GLOBAL_CONCURRENCY: u32 = 4;

/// Default number of Stop-hook block re-asks before a task escalates, when
/// `[hooks].block_retry_limit` is omitted (D-02).
pub const DEFAULT_BLOCK_RETRY_LIMIT: u32 = 3;

/// Default per-workflow silence limit in seconds (since the last hook signal)
/// before escalation, when `timeout_secs` is omitted (D-03). `0`: the sweep is
/// off unless a workflow sets a limit — silence alone cannot tell a hung agent
/// from one waiting on a human.
pub const DEFAULT_WORKFLOW_TIMEOUT_SECS: u64 = 0;

/// Root of `config.toml`.
///
/// # Why this is not `deny_unknown_fields`
///
/// Every top-level table that is *not* a named field here is a plugin's own
/// settings, captured verbatim by [`plugin_settings`](Self::plugin_settings)
/// (#554). serde therefore cannot be the one to reject an unknown key.
///
/// The check moves to validation and gets **stronger** rather than weaker:
/// a leftover table is legitimate only when a plugin of that name is in the
/// `[plugins.*]` roster, so `[worktre]` (a core-key typo) and `[slak]` (a
/// plugin-name typo) both fail, where before only the first did.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct RootConfig {
    /// Schema version (§10.2). Startup validation rejects a mismatch; the
    /// config is never migrated automatically (#276).
    ///
    /// Note the default: omitting `version` yields whatever
    /// [`CURRENT_SCHEMA_VERSION`] happens to be, so a `version`-less
    /// config.toml written for v1 would be read as v2 the moment this binary
    /// bumps — silently, since the guard above never fires. Deciding that
    /// default is a prerequisite for cutting v2; see the versioning policy in
    /// `ai-docs/development/config-reference.md`.
    #[serde(default = "default_version")]
    #[schemars(extend(
        "x-category" = "General",
        "x-title" = "Config format version",
        "x-help" = "The version of this file's format. Leave it at 1."
    ))]
    pub version: u32,
    /// Global maximum concurrent tasks (F-40). Defaults to
    /// [`DEFAULT_GLOBAL_CONCURRENCY`] when omitted.
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "General",
        "x-title" = "Concurrent tasks",
        "x-placeholder" = "4",
        "x-help" = "How many tasks run at the same time, across all repositories."
    ))]
    pub max_concurrency: Option<u32>,
    /// Registered local repositories (F-61).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Repositories",
        "x-title" = "Repositories",
        "x-help" = "The local clones that tasks are worked on."
    ))]
    pub repositories: Vec<RepositoryConfig>,
    /// Projects a repository can file into (#554): a GitHub Project, a Notion
    /// database, a Jira project.
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Projects",
        "x-title" = "Projects",
        "x-help" = "Named parts of a task source: a GitHub Project board, a Notion database, a Slack workspace. Workflows take tasks from them; repositories file new issues into them."
    ))]
    pub projects: Vec<ProjectConfig>,
    /// Plugin roster + common fields, keyed by plugin instance name (F-56).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Plugins",
        "x-title" = "Plugins",
        "x-help" = "Which installed plugins are used. Each plugin's own settings are in its own section."
    ))]
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Global default AI tool name when neither the workflow nor the selected
    /// repository picks one (#196). `None` means the built-in `"claude"`.
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "AI tools",
        "x-title" = "Default AI tool",
        "x-placeholder" = "claude",
        "x-help" = "The AI tool used when neither the workflow nor the repository names one."
    ))]
    pub default_tool: Option<String>,
    /// AI-tool registry, keyed by tool name (#196). Built-in defaults exist
    /// for `claude`; an entry overrides/extends them (e.g. a
    /// `[tools.claude-fast]` profile with a different model flag).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "AI tools",
        "x-title" = "AI tools",
        "x-help" = "The AI tool commands started in the agent's pane. claude, codex and opencode are built in; an entry with the same name overrides one."
    ))]
    pub tools: BTreeMap<String, ToolConfig>,
    /// Named workflows (parsed structurally here; semantics validated in #54).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Workflows",
        "x-title" = "Workflows",
        "x-help" = "Which tasks to pick up, and which agent works on them, how. The first workflow that matches a task wins."
    ))]
    pub workflows: Vec<WorkflowConfig>,
    /// AI Gateway settings (F-12, F-13).
    #[schemars(extend(
        "x-category" = "Repository classification",
        "x-title" = "Repository classification",
        "x-help" = "An LLM that decides which repository a task belongs to when the task does not say. Without it, such tasks wait for a person to choose."
    ))]
    #[schemars(with = "Option<RawLlmConfig>")]
    pub llm: Option<LlmConfig>,
    /// worktree placement defaults (consumed by #53).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Worktrees",
        "x-title" = "Worktrees",
        "x-help" = "Where each task's git worktree is created, and when it is removed."
    ))]
    pub worktree: WorktreeConfig,
    /// Logging settings (§5.2).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Logging",
        "x-title" = "Logging",
        "x-help" = "Log level and how many daily log files to keep."
    ))]
    pub log: LogSettings,
    /// Claude Code hook-event ingestion settings (#131: E-03, D-02, E-07).
    #[serde(default)]
    #[schemars(extend(
        "x-category" = "Agent hooks",
        "x-title" = "Agent hooks",
        "x-help" = "Where completion signals from the AI tool are received. The defaults rarely need changing."
    ))]
    pub hooks: HooksConfig,
    /// `[prompts]` — **removed in #465** (an amend of ADR-0023). Prompt text is
    /// built-in only; the one surviving knob is [`WorkflowConfig::rubric`].
    ///
    /// Parsed as an opaque table rather than dropped from the struct so a
    /// config that still carries it fails with an error naming each key and
    /// what became of it. Dropping the field would leave serde saying only
    /// that `prompts` is an unknown field — true, but silent about the fact
    /// that it used to be a supported table, which is the worst outcome for an
    /// operator who wrote it on purpose (#465).
    #[serde(default)]
    #[schemars(skip)]
    pub prompts: toml::Table,
    /// Every top-level table that is not one of the fields above: one
    /// plugin's own settings, held **uninterpreted** (#554).
    ///
    /// Keyed by plugin instance name, so `[slack]` here is the config the
    /// `slack` plugin receives at `initialize` — the same bytes that used to
    /// live in `[slack]` in config.toml. The Orchestrator never reads inside:
    /// secret references are resolved (F-65) and the rest is handed over as
    /// JSON, and `config/validate` (F-59) is what checks the contents.
    ///
    /// A name in here that the `[plugins.*]` roster does not know is a
    /// validation error, which is what keeps a typo from being read as
    /// "settings for a plugin nobody enabled".
    #[schemars(skip)]
    #[serde(flatten)]
    pub plugin_settings: BTreeMap<String, toml::Value>,
}

/// Hook-event ingestion settings from `[hooks]` (#131).
///
/// All fields are optional: the consuming components (UDS server, hook
/// rendering — later issues) apply the documented defaults.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// Removed (#785): `totsuka run` generates the Bearer token itself
    /// ([`hooks::token`](crate::hooks::token)). Kept only so a leftover line
    /// fails with [`REMOVED_AUTH_TOKEN_REF`] instead of serde's generic
    /// "unknown field".
    #[serde(default, deserialize_with = "removed_auth_token_ref")]
    #[schemars(skip)]
    pub auth_token_ref: (),
    /// Unix domain socket path the hook receiver listens on. `None` uses the
    /// built-in default path.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Socket path",
        "x-help" = "The Unix socket that receives hook signals. Leave empty for the default."
    ))]
    pub socket_path: Option<String>,
    /// Directory where hook events are spooled when the POST fails (E-07).
    /// `None` uses the built-in default path.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Spool directory",
        "x-help" = "Where hook signals are kept when they cannot be delivered. Leave empty for the default."
    ))]
    pub spool_dir: Option<String>,
    /// Max consecutive Stop-hook block re-asks before escalation (D-02).
    /// Defaults to [`DEFAULT_BLOCK_RETRY_LIMIT`].
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Re-ask limit",
        "x-placeholder" = "3",
        "x-help" = "How many times in a row an agent may be asked to finish its work before the task is escalated to you."
    ))]
    pub block_retry_limit: Option<u32>,
}

/// Why `[hooks].auth_token_ref` is now an error, and the one-line fix.
pub const REMOVED_AUTH_TOKEN_REF: &str = "[hooks].auth_token_ref was removed → delete this line; \
     `totsuka run` generates the hook token itself ($XDG_STATE_HOME/totsuka/hook-token)";

fn removed_auth_token_ref<'de, D: serde::Deserializer<'de>>(_: D) -> Result<(), D::Error> {
    Err(serde::de::Error::custom(REMOVED_AUTH_TOKEN_REF))
}

fn default_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// A registered repository (F-61).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    /// Stable identifier used in the worktree path, the logs and `totsuka
    /// status`. Not in branch names — the agent picks those (ADR-0026).
    #[schemars(extend(
        "x-title" = "Name",
        "x-help" = "A short, stable ID for the repository, used in logs and paths."
    ))]
    pub name: String,
    /// Local clone path (may use `~` and `${ENV}`; expanded on validation).
    #[schemars(extend(
        "x-title" = "Path",
        "x-help" = "The path to the local clone. ~ and ${VAR} are expanded."
    ))]
    pub path: PathBuf,
    /// Free-text summary used for LLM repo selection (F-11).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Summary",
        "x-help" = "What the repository is about. Repository classification reads it to choose a repository."
    ))]
    pub summary: Option<String>,
    /// Default AI tool for tasks dispatched into this repo (#196). Overrides
    /// `default_tool`; overridden by an explicit `[[workflows]].tool` pin.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "AI tool",
        "x-help" = "The AI tool for tasks in this repository, unless the workflow names one."
    ))]
    pub tool: Option<String>,
    /// Per-repository concurrency cap (F-41).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Concurrent tasks",
        "x-help" = "How many tasks run in this repository at the same time. Empty means no limit of its own."
    ))]
    pub max_concurrency: Option<u32>,
    /// Overrides the global `[worktree].location` for this repo (F-22).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Worktree location",
        "x-help" = "Overrides the worktree location template for this repository."
    ))]
    pub worktree_location: Option<String>,
    /// Which project this repository files into: the `name` of a
    /// `[[projects]]` entry (#554).
    ///
    /// Optional — a repository with no project is the normal state for anyone
    /// who has not set one up, and the source plugins must treat it as "say
    /// nothing extra", never as an error.
    ///
    /// **One scalar, so a repository has at most one project by
    /// construction.** Until #554 the mapping lived the other way round, as a
    /// `repos = [...]` list inside each source plugin's config
    /// ([ADR-0056](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md)),
    /// where two plugins could name the same repository and the Orchestrator
    /// had machinery to detect and report that. Here it cannot be written.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Project",
        "x-help" = "The project new issues for this repository are filed into (a project name)."
    ))]
    pub project: Option<String>,
}

/// A project a repository can file into (#554): one `[[projects]]` entry.
///
/// `name` and `source` are the Orchestrator's — the reference target and the
/// owning plugin. Everything else belongs to that plugin and is held
/// uninterpreted, exactly like a `[<name>]` table.
///
/// # Why `source` is written out rather than inferred
///
/// It could be guessed from the keys (only github understands
/// `project_number`), but naming it makes the reference chain
/// `[[repositories]].project` → `[[projects]].name` → `[plugins.<source>]`
/// walkable **without launching a plugin**, so a broken reference is caught by
/// `config validate --offline` and by anyone reading the file.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ProjectConfig {
    /// Stable identifier `[[repositories]].project` points at.
    #[schemars(extend(
        "x-title" = "Name",
        "x-help" = "A stable ID that workflows and repositories refer to."
    ))]
    pub name: String,
    /// The task_source plugin that owns this project.
    #[schemars(extend(
        "x-title" = "Task source",
        "x-help" = "The task source plugin this project belongs to (github, notion, slack, …). Its other keys are that plugin's."
    ))]
    pub source: String,
    /// Everything else on the entry, uninterpreted (#554).
    ///
    /// Unlike a workflow's options these need no claim handshake: an entry
    /// names exactly one plugin, so ownership is not in question and the
    /// plugin's own `deny_unknown_fields` is what rejects a typo.
    #[schemars(skip)]
    #[serde(flatten)]
    pub options: toml::Table,
}

/// Plugin kind (F-50).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// Task source (GitHub, Notion, ...).
    TaskSource,
    /// Agent IDE (herdr, orca, ...).
    AgentIde,
    /// Notifier (macOS notification center, ...).
    Notifier,
}

impl PluginKind {
    /// The stable snake_case wire/config string for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::TaskSource => "task_source",
            PluginKind::AgentIde => "agent_ide",
            PluginKind::Notifier => "notifier",
        }
    }
}

/// Common, Orchestrator-interpreted plugin fields from `[plugins.{name}]`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    /// Whether the plugin is active (F-56). Declarative roster flag.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Enabled",
        "x-help" = "Whether the plugin is started."
    ))]
    pub enabled: bool,
    /// Plugin kind.
    #[schemars(extend(
        "x-title" = "Kind",
        "x-help" = "task_source, agent_ide or notifier. Must match the plugin."
    ))]
    pub kind: PluginKind,
    /// Per-plugin concurrency cap (F-42).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Concurrent tasks",
        "x-help" = "For an agent plugin: how many tasks it runs at the same time. Empty means no limit of its own."
    ))]
    pub max_concurrency: Option<u32>,
    /// RPC timeout in seconds.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Call timeout (seconds)",
        "x-placeholder" = "120",
        "x-help" = "How long to wait for each answer from the plugin."
    ))]
    pub timeout_secs: Option<u64>,
    /// Whether a crash of this plugin is followed by a relaunch (#495).
    /// Defaults to `true`.
    ///
    /// Turning it off keeps the **detection**: the death is logged, counted in
    /// [`RunStats::plugin_crashes`](crate::run::RunStats), fails an agent's
    /// in-flight tasks, and sends an `escalated` notification. Only the
    /// relaunch is suppressed, which is what someone debugging a plugin by
    /// hand wants — a process that stays dead so they can see why.
    #[serde(default = "default_true")]
    #[schemars(extend(
        "x-title" = "Restart on crash",
        "x-help" = "Restart the plugin when it crashes (up to 5 times in 5 minutes)."
    ))]
    pub restart: bool,
}

/// serde default for [`PluginConfig::restart`].
fn default_true() -> bool {
    true
}

/// The settings window's schema of an `on_start` / `on_success` /
/// `on_failure` table (ADR-0109). Schema only: serde reads the table raw, and
/// `validate` holds its keys to
/// [`OUTCOME_ACTION_KEYS`](super::interpret::OUTCOME_ACTION_KEYS) — which a
/// test in `json_schema` ties these fields to, so the two cannot drift.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) struct OutcomeActionSchema {
    #[schemars(extend(
        "x-title" = "Status",
        "x-help" = "The status (column) to move the task to."
    ))]
    status: Option<String>,
    #[schemars(extend(
        "x-title" = "Labels",
        "x-help" = "Labels to add (+name) or remove (-name)."
    ))]
    labels: Option<Vec<String>>,
}

/// A named workflow (F-80). Parsed structurally; trigger/handoff semantics are
/// validated and matched in #54.
///
/// `mode` / `output` / `verification` are `Option` because
/// [`profile`](Self::profile) supplies them as a bundle. Read them through
/// [`resolved_mode`](Self::resolved_mode) /
/// [`resolved_output`](Self::resolved_output) /
/// [`resolved_verification`](Self::resolved_verification) — the raw fields are
/// for validation only, which is the one place that has to tell "omitted" from
/// "written out".
///
/// # Why this is not `deny_unknown_fields`
///
/// A plugin may define keys of its own on a workflow, written **flat**,
/// alongside the Orchestrator's (#554) — see
/// [`options`](Self::options). serde therefore cannot decide what is unknown,
/// because the Orchestrator does not know either: a workflow reaches a task
/// source (through its [`projects`](Self::projects)) *and* an `agent`, and the
/// key could be either one's.
///
/// The check moves to the plugins, which is the only place the answer exists.
/// It is not weaker: a key **no** plugin claims is an error, so `profil` still
/// fails — just at `initialize` rather than at parse.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct WorkflowConfig {
    /// Workflow name.
    #[schemars(extend(
        "x-title" = "Name",
        "x-help" = "The workflow's name."
    ))]
    pub name: String,
    /// The `[[projects]]` entries this workflow draws tasks from (#626), by
    /// `name`. Always a list, and never empty — validation rejects `[]`.
    ///
    /// The task source is **derived** from these rather than written out:
    /// every entry names its owning plugin, so a second spelling of the same
    /// fact could only ever contradict it. Listing more than one entry is a
    /// statement that those domains share a lane vocabulary, and
    /// `config validate` holds it to that: the columns a workflow names must
    /// exist on every domain it lists (#626).
    ///
    /// All named entries must resolve to the same `source` — a workflow
    /// straddling two plugins has no single claimant for its unclaimed keys
    /// (see [`options`](Self::options)), so validation refuses it.
    #[schemars(extend(
        "x-title" = "Projects",
        "x-help" = "The projects this workflow takes tasks from (project names)."
    ))]
    pub projects: Vec<String>,
    /// Trigger condition; kept raw (interpreted in #54).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Trigger",
        "x-help" = "Which tasks this workflow picks up, for example { status = \"Todo\" }. The keys are the task source's."
    ))]
    #[schemars(with = "serde_json::Map<String, serde_json::Value>")]
    pub trigger: toml::Table,
    /// One of the four archetypes (#393 D5). Supplies all three of `mode`,
    /// `output` and `verification`. `mode` and `verification` must then not
    /// also be written out; `output` may, and an explicit one wins.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Profile",
        "x-help" = "answer, triage, design or implement. Sets the mode, output and verification together."
    ))]
    pub profile: Option<Profile>,
    /// Execution mode. Required unless [`profile`](Self::profile) supplies it.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Mode",
        "x-help" = "plan (design only) or implement. Required when there is no profile."
    ))]
    pub mode: Option<WorkflowMode>,
    /// Agent plugin instance name (must be an enabled `agent_ide`).
    #[schemars(extend(
        "x-title" = "Agent",
        "x-help" = "The agent plugin that runs the task (herdr, orca, …)."
    ))]
    pub agent: String,
    /// Output policy. Required unless [`profile`](Self::profile) supplies it,
    /// and the one field a profile may be overridden on.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Output",
        "x-help" = "source (write the result back to the task source) or none. Required when there is no profile."
    ))]
    pub output: Option<OutputPolicy>,
    /// Source status transition when the task starts running (#556): applied
    /// right before dispatch. When the source supports an exclusion claim
    /// (#556's follow-up in this stack), that claim settles first, so the
    /// write only ever happens for a task this instance actually holds.
    /// `None` writes nothing — exactly the pre-#556 behaviour — so existing
    /// configs are untouched.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "On start",
        "x-help" = "What to change on the task when work starts: { status = \"…\", labels = [\"+a\", \"-b\"] }."
    ))]
    #[schemars(with = "Option<OutcomeActionSchema>")]
    pub on_start: Option<toml::Table>,
    /// Source status transition on success; kept raw (interpreted in #54).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "On success",
        "x-help" = "What to change on the task when the work succeeds."
    ))]
    #[schemars(with = "Option<OutcomeActionSchema>")]
    pub on_success: Option<toml::Table>,
    /// Source status transition on failure; kept raw (interpreted in #54).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "On failure",
        "x-help" = "What to change on the task when the work fails."
    ))]
    #[schemars(with = "Option<OutcomeActionSchema>")]
    pub on_failure: Option<toml::Table>,
    /// How completion self-reports are verified (D-01). Omitted means `llm`,
    /// same as before profiles existed — the `Option` distinguishes "omitted"
    /// from "written out" so validation can reject writing it out *alongside* a
    /// profile. Both resolve to the same value.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Verification",
        "x-placeholder" = "llm",
        "x-help" = "How finished work is checked: llm, human or none."
    ))]
    pub verification: Option<VerificationMode>,
    /// Silence limit in seconds since the last hook signal before the task
    /// escalates (D-03). Defaults to [`DEFAULT_WORKFLOW_TIMEOUT_SECS`].
    ///
    /// `0` disables the sweep for this workflow (#439) — for attended panes
    /// where a human watches the agent and silence is not evidence of a stall.
    /// The trade-off is real: a genuinely hung agent is never detected either,
    /// so never set it on an unattended workflow. (Before #439 a written-out
    /// `0` effectively escalated on the first sweep, which no config could
    /// have wanted.)
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Silence limit (seconds)",
        "x-placeholder" = "0",
        "x-help" = "Escalate when the agent has been silent this long. 0 turns it off."
    ))]
    pub timeout_secs: Option<u64>,
    /// Criteria text embedded into the llm-verification prompt hook. Only
    /// meaningful with `verification = "llm"` (validation warns otherwise).
    ///
    /// **The only prompt knob there is** since #465 removed the `[prompts]`
    /// surface. It predates that surface (it is older than #314) and outlived
    /// it: of the fifteen keys #314 added, this is the one an operator ever
    /// set. It beats the profile's rubric, which is what makes it useful —
    /// see [`Prompts::resolve_for`](crate::prompts::Prompts::resolve_for).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Rubric",
        "x-help" = "The criteria the llm verification checks the work against."
    ))]
    pub rubric: Option<String>,
    /// `[[workflows]].prompts` — **removed in #465**, same treatment as
    /// [`RootConfig::prompts`]: parsed opaquely so validation can name each key
    /// instead of leaving a bare unknown-field error.
    #[serde(default)]
    #[schemars(skip)]
    pub prompts: toml::Table,
    /// Explicit AI-tool pin for this workflow (#196) — the strongest level of
    /// the tool precedence (workflow > repo > `default_tool`). Use it when the
    /// flow's shape demands a specific tool (e.g. `verification = "llm"`
    /// needs Claude's prompt-type Stop hook). `None` falls through to the
    /// repository/global defaults.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "AI tool",
        "x-help" = "Pin the AI tool for this workflow. Wins over the repository's and the default."
    ))]
    pub tool: Option<String>,
    /// Extra instructions prepended to the task body the **first** time a
    /// conversation is started (#415).
    ///
    /// A separate layer from [`prompts`](Self::prompts): those seven keys are
    /// wire-convention prose that breaks the completion contract when dropped,
    /// are substituted into a fixed template, and are validated against
    /// `ALLOWED_PLACEHOLDERS`. This is operator-written task shaping — no
    /// substitution happens, `{` is literal, and it goes to a different place
    /// (the visible pane, not the invisible channel or the Stop hook).
    ///
    /// Empty or whitespace-only is treated as unset rather than rejected.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Initial prompt",
        "x-help" = "Extra instructions given to the agent before the task, at the start of a new conversation."
    ))]
    pub initial_prompt: Option<String>,
    /// Worktree cleanup override for this workflow's tasks (#548, ADR-0057).
    ///
    /// Beats the mode-selected `[worktree]` default (`cleanup` /
    /// `plan_cleanup`). Absent means the default, which is what every config
    /// written before this key existed says. If the workflow is later removed
    /// or renamed in config, the sweep can no longer resolve this override
    /// and **falls back to the default** — deliberate (ADR-0057): the
    /// operator changed the config, and persisting overrides elsewhere just
    /// to survive that is not worth a second source of truth.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Worktree cleanup",
        "x-help" = "Overrides the worktree cleanup for this workflow's tasks."
    ))]
    pub cleanup: Option<CleanupPolicyConfig>,
    /// Every key on this workflow that is not one of the fields above: a
    /// plugin's own, held **uninterpreted** (#554).
    ///
    /// Written flat, next to the Orchestrator's keys, because that is what a
    /// workflow option is from the operator's side — `publish = "direct"`
    /// reads the same whether core or the Slack source is the one that
    /// consumes it. The nesting that would make ownership syntactically
    /// obvious would also make the config say something the operator does not
    /// care about.
    ///
    /// Ownership is resolved by asking instead: the whole set goes to the
    /// workflow's task source (the owner of its
    /// [`projects`](WorkflowConfig::projects)) and its `agent` at
    /// `initialize`, and each answers which
    /// keys are its own (`InitializeResult::claimed_options`). **Exactly one**
    /// claimant is required — zero is a typo, two is an ambiguity the
    /// Orchestrator will not settle by picking.
    #[schemars(skip)]
    #[serde(flatten)]
    pub options: toml::Table,
}

impl WorkflowConfig {
    /// The execution mode this workflow actually runs in: the profile's, else
    /// the explicit one.
    ///
    /// The last arm is unreachable through a validated config —
    /// `validate_static` rejects a workflow with neither — and resolves to the
    /// *least* powerful value on purpose. If a path ever reached here
    /// unvalidated, the failure to prefer is a read-only worktree, not an
    /// implement run nobody asked for.
    pub fn resolved_mode(&self) -> WorkflowMode {
        match (self.profile, self.mode) {
            (Some(profile), _) => profile.mode(),
            (None, Some(mode)) => mode,
            (None, None) => WorkflowMode::Plan,
        }
    }

    /// The output policy this workflow actually uses.
    ///
    /// An explicit `output` **wins over the profile's** — the one documented
    /// override (#393/#394). It picks a wiring destination rather than a
    /// permission, so allowing it costs no safety, and a Slack-sourced
    /// `implement` needs `output = "source"` to get its PR URL back into the
    /// thread. The `(None, None)` fallback mirrors
    /// [`resolved_mode`](Self::resolved_mode): publish nothing.
    pub fn resolved_output(&self) -> OutputPolicy {
        match (self.output, self.profile) {
            (Some(output), _) => output,
            (None, Some(profile)) => profile.output(),
            (None, None) => OutputPolicy::None,
        }
    }

    /// How this workflow's completion self-report is verified. Omitting it has
    /// always meant `llm`, and every profile resolves to `llm` too, so the
    /// fallback here is the real default rather than a safety net.
    pub fn resolved_verification(&self) -> VerificationMode {
        match (self.profile, self.verification) {
            (Some(profile), _) => profile.verification(),
            (None, Some(verification)) => verification,
            (None, None) => VerificationMode::default(),
        }
    }
}

/// A `[tools.<name>]` registry entry (#196): how to launch one AI tool CLI.
/// Interpreted into a [`ToolProfile`](crate::tool::ToolProfile).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    /// Adapter family (`claude` | `codex` | `opencode`) — determines argv
    /// assembly and completion detection.
    #[schemars(extend(
        "x-title" = "Kind",
        "x-help" = "claude, codex or opencode."
    ))]
    pub kind: ToolKind,
    /// Whitespace-split command line (first token = program, rest = base
    /// args). `None` uses the kind's name as the program.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Command",
        "x-help" = "The command line to start, for example \"claude --model haiku\". Not run through a shell."
    ))]
    pub command: Option<String>,
    /// Extra args appended in implement mode (overrides the kind default).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Implement arguments",
        "x-help" = "Arguments added in implement mode."
    ))]
    pub mode_args: Option<Vec<String>>,
    /// Extra args appended in plan mode (overrides the kind default).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Plan arguments",
        "x-help" = "Arguments added in plan mode."
    ))]
    pub plan_args: Option<Vec<String>>,
    /// A `KEY=value` file whose values are added to the env of every agent
    /// this tool launches (#744). `~` / `${VAR}` expand; the result must be
    /// absolute. Resolved once when `totsuka run` starts — see
    /// [`env_file`](crate::config::env_file).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Environment file",
        "x-help" = "A file of KEY=value lines, added to the agent's environment."
    ))]
    pub env_file: Option<String>,
}

/// Repository classifier settings — `[llm]` (F-11–F-14).
///
/// Read through `RawLlmConfig` so that a key that does not apply to the
/// chosen [`LlmApi`] (`endpoint` on a chat gateway, `base_url` on a decisions
/// model) is a load error rather than a silently ignored line.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawLlmConfig")]
pub struct LlmConfig {
    /// Which kind of API, with its API-specific settings.
    pub api: LlmApi,
    /// Model name (cheap model assumed for chat; `~typesafe/jev-latest` for
    /// decisions).
    pub model: String,
    /// Request timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Secret reference to the API key (`${ENV}` or `keychain:`).
    pub api_key_ref: Option<String>,
    /// Minimum classifier confidence to select a repository without asking
    /// (F-14), `0.0..=1.0`. Default 0.6.
    pub confidence_threshold: Option<f64>,
}

/// `[llm].api` with the keys only that API takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmApi {
    /// An OpenAI-compatible `/chat/completions` gateway (the default).
    Chat {
        /// Base URL; `/chat/completions` is appended.
        base_url: String,
        /// Max tokens for the classification call (not sent when unset).
        max_tokens: Option<u32>,
    },
    /// A decisions model (TypeSafe Jev) behind a Decisions API.
    Decisions {
        /// The full endpoint URL; `None` is OpenRouter's
        /// ([`LlmApi::DEFAULT_DECISIONS_ENDPOINT`]).
        endpoint: Option<String>,
    },
}

impl LlmApi {
    /// OpenRouter's Decisions API, the endpoint when `[llm].endpoint` is unset.
    pub const DEFAULT_DECISIONS_ENDPOINT: &str =
        repo_classifier::DecisionsSettings::OPENROUTER_ENDPOINT;

    /// The value written as `[llm].api`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Chat { .. } => "chat",
            Self::Decisions { .. } => "decisions",
        }
    }
}

/// `[llm].api` as written.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LlmApiKind {
    #[default]
    Chat,
    Decisions,
}

/// `[llm]` as written, before the per-API keys are checked.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RawLlmConfig {
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "API",
        "x-placeholder" = "chat",
        "x-help" = "chat (an OpenAI-compatible chat API) or decisions (a model that only picks)."
    ))]
    api: LlmApiKind,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Base URL",
        "x-help" = "For chat: the API's base URL, for example https://openrouter.ai/api/v1."
    ))]
    base_url: Option<String>,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Endpoint",
        "x-help" = "For decisions: the full URL of the Decisions API. Empty means OpenRouter's."
    ))]
    endpoint: Option<String>,
    #[schemars(extend(
        "x-title" = "Model",
        "x-help" = "The model name."
    ))]
    model: String,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Max tokens",
        "x-help" = "For chat: the maximum tokens per call. Empty means the provider's default."
    ))]
    max_tokens: Option<u32>,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Timeout (seconds)",
        "x-placeholder" = "30",
        "x-help" = "How long to wait for an answer."
    ))]
    timeout_secs: Option<u64>,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "API key",
        "x-help" = "The API key.",
        "x-secret" = true
    ))]
    api_key_ref: Option<String>,
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Confidence threshold",
        "x-placeholder" = "0.6",
        "x-help" = "Below this confidence (0.0–1.0) the task waits for a person instead."
    ))]
    confidence_threshold: Option<f64>,
}

impl TryFrom<RawLlmConfig> for LlmConfig {
    type Error = String;

    fn try_from(raw: RawLlmConfig) -> Result<Self, String> {
        let misplaced = |key: &str, api: &str| {
            format!(
                "`[llm].{key}` does not apply to api = \"{api}\" → remove it{}",
                if api == "chat" {
                    ", or set api = \"decisions\""
                } else {
                    ", or set api = \"chat\""
                }
            )
        };
        let api = match raw.api {
            LlmApiKind::Chat => {
                if raw.endpoint.is_some() {
                    return Err(misplaced("endpoint", "chat"));
                }
                LlmApi::Chat {
                    base_url: raw.base_url.ok_or(
                        "`[llm].base_url` is required for api = \"chat\" (e.g. \"https://openrouter.ai/api/v1\")",
                    )?,
                    max_tokens: raw.max_tokens,
                }
            }
            LlmApiKind::Decisions => {
                if raw.base_url.is_some() {
                    return Err(misplaced("base_url", "decisions"));
                }
                if raw.max_tokens.is_some() {
                    return Err(misplaced("max_tokens", "decisions"));
                }
                LlmApi::Decisions {
                    endpoint: raw.endpoint,
                }
            }
        };
        Ok(Self {
            api,
            model: raw.model,
            timeout_secs: raw.timeout_secs,
            api_key_ref: raw.api_key_ref,
            confidence_threshold: raw.confidence_threshold,
        })
    }
}

/// worktree placement defaults (F-22) and cleanup policies (F-23, F-85).
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeConfig {
    /// Global placement template; overridable per repo.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Location",
        "x-help" = "Where worktrees are created. A template with {repo_name}, {worktree_name} and others. Empty means the default."
    ))]
    pub location: Option<String>,
    /// Cleanup policy for implement-mode worktrees (F-23). Defaults to
    /// `manual` (never lose committed-but-unpushed work).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Cleanup (implement)",
        "x-placeholder" = "manual",
        "x-help" = "When implement-mode worktrees are removed: manual, immediate, keep_7d, keep_28d, or after a number of days."
    ))]
    pub cleanup: Option<CleanupPolicyConfig>,
    /// Cleanup policy for plan-mode worktrees (F-85). Defaults to `immediate`
    /// (design-only worktrees carry no unique work).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Cleanup (plan)",
        "x-placeholder" = "immediate",
        "x-help" = "When plan-mode worktrees are removed."
    ))]
    pub plan_cleanup: Option<CleanupPolicyConfig>,
    /// Seconds a single git command may run before it is killed (#764).
    /// Omitted: `DEFAULT_GIT_TIMEOUT` (300). `0`: no limit.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "git timeout (seconds)",
        "x-placeholder" = "300",
        "x-help" = "Stop a git command that runs longer than this. 0 means no limit."
    ))]
    pub git_timeout_secs: Option<u64>,
}

/// A worktree cleanup policy as written in config (F-23):
/// `"immediate"`, `"manual"`, or `{ retention_days = 5 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum CleanupPolicyConfig {
    /// A named policy (`immediate` / `manual`).
    Named(CleanupPolicyName),
    /// Keep for N days after the task finished, then remove.
    Retention {
        /// Days to keep a finished task's worktree.
        #[schemars(extend(
        "x-title" = "Days to keep",
        "x-help" = "Days to keep a finished task's worktree."
    ))]
        retention_days: u32,
    },
}

/// The named cleanup policies.
///
/// `keep_7d` / `keep_28d` (#210) are pure sugar for `{ retention_days = 7 }`
/// / `{ retention_days = 28 }` — the mapping happens at config interpretation
/// and nothing downstream knows the presets exist. `keep_` names the behavior;
/// `7d`/`28d` are exact where `week`/`month` would be ambiguous (28 days ≠ one
/// month). NB the explicit `rename`s: `rename_all = "snake_case"` would turn
/// `Keep7d` into `keep7d`, not `keep_7d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicyName {
    /// Remove as soon as the task finishes.
    Immediate,
    /// Never auto-remove; a human cleans up.
    Manual,
    /// Keep for 7 days after the task finished, then remove.
    #[serde(rename = "keep_7d")]
    Keep7d,
    /// Keep for 28 days after the task finished, then remove.
    #[serde(rename = "keep_28d")]
    Keep28d,
}

/// Logging settings from `[log]` (§5.2).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogSettings {
    /// Minimum level name (`error`/`warn`/`info`/`debug`/`trace`).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Level",
        "x-placeholder" = "info",
        "x-help" = "error, warn, info, debug or trace."
    ))]
    pub level: Option<String>,
    /// Whether prompt/RPC-payload fields are logged (debug+ only regardless).
    #[serde(default = "default_log_prompts")]
    #[schemars(extend(
        "x-title" = "Log prompts",
        "x-help" = "Record prompts and payloads (only written at debug level or above)."
    ))]
    pub log_prompts: bool,
    /// Number of daily log files to keep.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = "Files to keep",
        "x-placeholder" = "7",
        "x-help" = "How many daily log files to keep."
    ))]
    pub max_files: Option<usize>,
}

fn default_log_prompts() -> bool {
    true
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: None,
            log_prompts: default_log_prompts(),
            max_files: None,
        }
    }
}

/// Errors from parsing or converting configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A TOML document failed to parse against the schema.
    #[error("failed to parse TOML config: {0}")]
    Parse(#[from] toml::de::Error),
    /// Converting a plugin's settings table to JSON failed (#554).
    #[error("failed to convert plugin config to JSON: {0}")]
    Convert(#[from] serde_json::Error),
    /// A `TOTSUKA_*` override could not be applied (F-66 layer 2; see
    /// [`env_overrides`](crate::config::env_overrides)).
    #[error("invalid environment override {var}: {reason}")]
    EnvOverride {
        /// The environment variable name, e.g. `TOTSUKA_MAX_CONCURRENCY`.
        var: String,
        /// Why it could not be applied (expected type, or missing table).
        reason: String,
    },
}

/// Whether `name` is already a top-level key of [`RootConfig`], and therefore
/// cannot be used as a plugin instance name (#554).
///
/// A plugin called `log` would write its settings to `[log]`, which serde
/// hands to [`RootConfig::log`] instead — the plugin would start with an
/// **empty config and no error anywhere**. Plugin names are binary names and
/// cannot be renamed ([ADR-0027] refused `name != bin name`), so the only
/// honest answer is to reject the roster entry.
///
/// # Why this probes instead of consulting a list
///
/// A hand-written list of reserved names is a second copy of the struct's
/// field set with nothing keeping the two in step: add a top-level key,
/// forget the list, and the silent-empty-config bug is back. Deserializing a
/// one-key probe asks serde the question directly, so the answer is correct
/// by construction. A name that fails to parse as its field's type (`version`
/// as a table, say) is reserved too — it never reaches
/// [`RootConfig::plugin_settings`] either way.
///
/// [ADR-0027]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0027-plugin-artifact-naming.md
pub fn is_reserved_top_level_key(name: &str) -> bool {
    let mut probe = toml::Table::new();
    probe.insert(name.to_string(), toml::Value::Table(toml::Table::new()));
    match toml::Value::Table(probe).try_into::<RootConfig>() {
        Ok(cfg) => !cfg.plugin_settings.contains_key(name),
        Err(_) => true,
    }
}

impl RootConfig {
    /// Parse a `config.toml` document.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(s)?)
    }

    /// Look up a plugin's common config by instance name.
    pub fn plugin(&self, name: &str) -> Option<&PluginConfig> {
        self.plugins.get(name)
    }

    /// Look up a `[[projects]]` entry by `name`.
    pub fn project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }

    /// The task source a workflow draws from, resolved through the first of
    /// its [`projects`](WorkflowConfig::projects) (#626).
    ///
    /// Resolving from the *first* entry is sound because validation refuses a
    /// workflow whose entries disagree on `source`; a config that reaches
    /// runtime has one answer by then.
    ///
    /// `None` means the workflow names no project, or names one that does not
    /// exist — both are config-validation errors, so a caller that meets one
    /// should skip the workflow rather than guess an owner for it.
    pub fn workflow_source(&self, workflow: &WorkflowConfig) -> Option<&str> {
        self.project(workflow.projects.first()?)
            .map(|p| p.source.as_str())
    }

    /// A plugin's own uninterpreted settings (`[<name>]`), if it wrote any.
    ///
    /// Absent is normal — a plugin whose defaults suffice needs no table at
    /// all, which is the same thing an absent `plugins/{name}.toml` used to
    /// mean.
    pub fn plugin_settings(&self, name: &str) -> Option<&toml::Value> {
        self.plugin_settings.get(name)
    }

    /// Look up a `[tools.<name>]` entry by tool name (#196). Built-in
    /// defaults are resolved separately
    /// ([`ToolProfile::builtin`](crate::tool::ToolProfile::builtin)); an
    /// entry here overrides the built-in of the same name.
    pub fn tool(&self, name: &str) -> Option<&ToolConfig> {
        self.tools.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact §4.6 example must parse.
    const SPEC_EXAMPLE: &str = r#"
[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3
timeout_secs = 120

[plugins.github]
enabled = true
kind = "task_source"

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
worktree_location = "{repo}/../.worktrees/{worktree_name}"

[worktree]
location = "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{worktree_name}"

[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-3.5-haiku"
api_key_ref = "keychain:totsuka/openrouter"

[[projects]]
name = "github"
source = "github"

[[workflows]]
name = "design"
projects = ["github"]
trigger = { status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
projects = ["github"]
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "source"
on_success = { status = "レビュー待ち" }
"#;

    #[test]
    fn parses_spec_example() {
        let cfg = RootConfig::from_toml_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(cfg.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.repositories.len(), 1);
        assert_eq!(cfg.repositories[0].name, "totsuka");
        assert_eq!(cfg.plugin("herdr").unwrap().kind, PluginKind::AgentIde);
        assert_eq!(cfg.plugin("herdr").unwrap().max_concurrency, Some(3));
        assert!(cfg.plugin("github").unwrap().enabled);

        assert_eq!(cfg.workflows.len(), 2);
        let design = &cfg.workflows[0];
        assert_eq!(design.mode, Some(WorkflowMode::Plan));
        assert_eq!(design.output, Some(OutputPolicy::Source));
        assert_eq!(
            design.trigger.get("status").unwrap().as_str(),
            Some("設計待ち")
        );
        assert_eq!(cfg.workflows[1].output, Some(OutputPolicy::Source));

        let llm = cfg.llm.as_ref().unwrap();
        assert_eq!(
            llm.api_key_ref.as_deref(),
            Some("keychain:totsuka/openrouter")
        );
    }

    /// `[llm].api` picks which keys are allowed; the others are load errors
    /// naming the key, never silently ignored (#723).
    #[test]
    fn llm_keys_follow_the_api() {
        let llm = |body: &str| RootConfig::from_toml_str(&format!("[llm]\n{body}"));

        // No `api` is chat, as every config before #723 was written.
        let chat = llm("base_url = \"https://gw/v1\"\nmodel = \"m\"\nmax_tokens = 64")
            .unwrap()
            .llm
            .unwrap();
        assert_eq!(
            chat.api,
            LlmApi::Chat {
                base_url: "https://gw/v1".into(),
                max_tokens: Some(64),
            }
        );

        let decisions = llm("api = \"decisions\"\nmodel = \"~typesafe/jev-latest\"")
            .unwrap()
            .llm
            .unwrap();
        assert_eq!(decisions.api, LlmApi::Decisions { endpoint: None });
        assert_eq!(decisions.api.name(), "decisions");

        for (body, needle) in [
            ("model = \"m\"", "`[llm].base_url` is required"),
            (
                "base_url = \"https://gw/v1\"\nendpoint = \"https://gw/d\"\nmodel = \"m\"",
                "`[llm].endpoint` does not apply",
            ),
            (
                "api = \"decisions\"\nbase_url = \"https://gw/v1\"\nmodel = \"m\"",
                "`[llm].base_url` does not apply",
            ),
            (
                "api = \"decisions\"\nmax_tokens = 64\nmodel = \"m\"",
                "`[llm].max_tokens` does not apply",
            ),
            ("api = \"telepathy\"\nmodel = \"m\"", "telepathy"),
            (
                "base_url = \"u\"\nmodel = \"m\"\nbase_uri = \"typo\"",
                "base_uri",
            ),
        ] {
            let message = llm(body).unwrap_err().to_string();
            assert!(message.contains(needle), "{body}: {message}");
        }
    }

    /// Since #554 an unknown top-level key is not a parse error — it is a
    /// plugin's settings table, and rejecting it here would reject every valid
    /// config. Parsing holds it; `validate_static` is what decides whether the
    /// roster knows the name.
    #[test]
    fn unknown_top_level_keys_are_held_for_validation() {
        let cfg = RootConfig::from_toml_str("bogus_key = 1").unwrap();
        assert_eq!(
            cfg.plugin_settings
                .get("bogus_key")
                .and_then(|v| v.as_integer()),
            Some(1)
        );
    }

    /// The Orchestrator's own keys must never be captured as plugin settings,
    /// or a plugin named after one would start with an empty config and no
    /// error (#554). Probed rather than listed so the answer cannot drift from
    /// the struct.
    #[test]
    fn every_root_field_is_a_reserved_plugin_name() {
        // Two shapes on purpose: `worktree` parses fine from an empty table
        // (all-default struct), `version` does not (it is a `u32`). Both are
        // reserved, and the two paths through `is_reserved_top_level_key` are
        // exactly those cases.
        for name in [
            "version",
            "max_concurrency",
            "repositories",
            "projects",
            "plugins",
            "default_tool",
            "tools",
            "workflows",
            "llm",
            "worktree",
            "log",
            "hooks",
            "prompts",
        ] {
            assert!(
                is_reserved_top_level_key(name),
                "`{name}` is a RootConfig field but was not reported as reserved"
            );
        }
        for name in ["slack", "github", "herdr", "macos", "mock_agent"] {
            assert!(
                !is_reserved_top_level_key(name),
                "`{name}` is not a RootConfig field but was reported as reserved"
            );
        }
    }

    /// The flattened catch-all must capture **only** what the Orchestrator does
    /// not name (#554). If a core key leaked into it, every workflow using that
    /// key would demand a plugin claim for it — and, worse, the tests that
    /// exercise the claim rule would be asserting on the wrong keys and pass
    /// while checking nothing.
    #[test]
    fn workflow_options_hold_only_the_keys_core_does_not_name() {
        let cfg = RootConfig::from_toml_str(
            r#"
[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "reply"
projects = ["slack"]
agent = "herdr"
profile = "answer"
publish = "direct"
timeout_secs = 0
trigger = { reaction = "eyes" }
thread_scope = "parent"
"#,
        )
        .unwrap();
        let wf = &cfg.workflows[0];
        assert_eq!(
            wf.options.keys().collect::<Vec<_>>(),
            vec!["publish", "thread_scope"],
            "the plugin-defined keys are leftover — `publish` among them since \
             #554 moved it out of core"
        );
        // …and the named fields still parsed, rather than being shadowed.
        assert_eq!(wf.timeout_secs, Some(0));
        assert_eq!(
            wf.trigger.get("reaction").and_then(|v| v.as_str()),
            Some("eyes")
        );
    }

    /// A plugin's table travels through parsing untouched, nested shapes
    /// included — that is the whole contract `plugins/{name}.toml` used to
    /// carry.
    #[test]
    fn a_plugin_table_is_held_verbatim() {
        let cfg = RootConfig::from_toml_str(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"

[slack]
app_token = "op://Dev/Slack/app_token"

[[slack.channel_groups]]
prefix = "dev-"
repos = ["dotfiles"]
"#,
        )
        .unwrap();
        let slack = cfg.plugin_settings("slack").expect("[slack] is held");
        assert_eq!(
            slack.get("app_token").and_then(|v| v.as_str()),
            Some("op://Dev/Slack/app_token")
        );
        assert_eq!(
            slack["channel_groups"][0]["prefix"].as_str(),
            Some("dev-"),
            "the array-of-tables came through nested under the plugin"
        );
        // The roster table is a named field and never leaks into the catch-all.
        assert_eq!(
            cfg.plugin_settings.keys().collect::<Vec<_>>(),
            vec!["slack"]
        );
    }

    #[test]
    fn hooks_and_verification_fields_parse() {
        let cfg = RootConfig::from_toml_str(
            r#"
[hooks]
socket_path = "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock"
spool_dir = "${XDG_STATE_HOME}/totsuka/hooks/spool"
block_retry_limit = 3

[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "slack-reply"
projects = ["slack"]
mode = "implement"
agent = "herdr"
output = "source"
verification = "human"
timeout_secs = 1800
rubric = "回答は対象リポジトリの実調査に基づくこと"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.hooks.socket_path.as_deref(),
            Some("${XDG_RUNTIME_DIR}/totsuka/agent-events.sock")
        );
        assert_eq!(
            cfg.hooks.spool_dir.as_deref(),
            Some("${XDG_STATE_HOME}/totsuka/hooks/spool")
        );
        assert_eq!(cfg.hooks.block_retry_limit, Some(3));

        let wf = &cfg.workflows[0];
        assert_eq!(wf.verification, Some(VerificationMode::Human));
        assert_eq!(wf.timeout_secs, Some(1800));
        assert_eq!(
            wf.rubric.as_deref(),
            Some("回答は対象リポジトリの実調査に基づくこと")
        );
    }

    #[test]
    fn hooks_and_verification_default_when_omitted() {
        // The pre-#135 spec example omits every new key -> all defaults.
        let cfg = RootConfig::from_toml_str(SPEC_EXAMPLE).unwrap();
        assert!(cfg.hooks.socket_path.is_none());
        assert!(cfg.hooks.spool_dir.is_none());
        assert!(cfg.hooks.block_retry_limit.is_none());
        for wf in &cfg.workflows {
            assert_eq!(wf.verification, None);
            assert_eq!(wf.resolved_verification(), VerificationMode::Llm);
            assert!(wf.timeout_secs.is_none());
            assert!(wf.rubric.is_none());
        }
    }

    /// #785: a leftover `auth_token_ref` names the fix, not "unknown field".
    #[test]
    fn removed_auth_token_ref_says_to_delete_the_line() {
        let err = RootConfig::from_toml_str("[hooks]\nauth_token_ref = \"keychain:x\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains(REMOVED_AUTH_TOKEN_REF), "{err}");
    }

    #[test]
    fn unknown_hooks_key_is_rejected() {
        // A typo inside [hooks] (auth_token) must not be
        // silently ignored.
        let err = RootConfig::from_toml_str("[hooks]\nauth_token = \"x\"").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
        // An unknown verification mode is rejected, not defaulted.
        assert!(
            RootConfig::from_toml_str(
                r#"
[[projects]]
name = "s"
source = "s"

[[workflows]]
name = "w"
projects = ["s"]
mode = "implement"
agent = "a"
output = "none"
verification = "manual"
"#
            )
            .is_err()
        );
    }

    #[test]
    fn empty_config_is_valid_with_defaults() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        assert_eq!(cfg.version, CURRENT_SCHEMA_VERSION);
        assert!(cfg.repositories.is_empty());
        assert!(cfg.plugins.is_empty());
        assert!(cfg.max_concurrency.is_none());
        assert!(cfg.worktree.cleanup.is_none());
    }

    #[test]
    fn run_loop_fields_parse() {
        let cfg = RootConfig::from_toml_str(
            r#"
max_concurrency = 8

[plugins.github]
enabled = true
kind = "task_source"

[worktree]
cleanup = { retention_days = 5 }
plan_cleanup = "immediate"
"#,
        )
        .unwrap();
        assert_eq!(cfg.max_concurrency, Some(8));
        assert_eq!(
            cfg.worktree.cleanup,
            Some(CleanupPolicyConfig::Retention { retention_days: 5 })
        );
        assert_eq!(
            cfg.worktree.plan_cleanup,
            Some(CleanupPolicyConfig::Named(CleanupPolicyName::Immediate))
        );
        // An unknown policy name is rejected, not silently ignored.
        assert!(RootConfig::from_toml_str("[worktree]\ncleanup = \"sometimes\"").is_err());
    }

    #[test]
    fn tool_fields_parse() {
        // #196: the tool registry + the three levels of tool selection.
        let cfg = RootConfig::from_toml_str(
            r#"
default_tool = "claude"

[tools.codex]
kind = "codex"
command = "codex"

[tools.claude-fast]
kind = "claude"
command = "claude --model haiku"
plan_args = ["--permission-mode", "plan"]

[[repositories]]
name = "totsuka"
path = "/tmp"
tool = "codex"

[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "reply"
projects = ["slack"]
mode = "plan"
agent = "herdr"
output = "source"
tool = "claude"
"#,
        )
        .unwrap();
        assert_eq!(cfg.default_tool.as_deref(), Some("claude"));
        assert_eq!(
            cfg.tool("codex").unwrap().kind,
            crate::tool::ToolKind::Codex
        );
        assert_eq!(
            cfg.tool("claude-fast").unwrap().command.as_deref(),
            Some("claude --model haiku")
        );
        assert_eq!(cfg.repositories[0].tool.as_deref(), Some("codex"));
        assert_eq!(cfg.workflows[0].tool.as_deref(), Some("claude"));
        // Omitted everywhere -> None (built-in claude applies downstream).
        let cfg = RootConfig::from_toml_str(SPEC_EXAMPLE).unwrap();
        assert!(cfg.default_tool.is_none());
        assert!(cfg.tools.is_empty());
        assert!(cfg.repositories[0].tool.is_none());
        assert!(cfg.workflows[0].tool.is_none());
    }

    #[test]
    fn unknown_tool_key_and_removed_default_agent_are_rejected() {
        // Typos inside [tools.*] must not be silently ignored.
        assert!(RootConfig::from_toml_str("[tools.x]\nkind = \"claude\"\ncomand = \"c\"").is_err());
        // An unknown kind is rejected, not defaulted.
        assert!(RootConfig::from_toml_str("[tools.x]\nkind = \"cursor\"").is_err());
        // #196: `default_agent` was removed (never consumed at runtime; the
        // `tool` axis replaces it) — deny_unknown_fields turns a leftover
        // entry into a named parse error rather than silent dead config.
        let err = RootConfig::from_toml_str(
            "[[repositories]]\nname = \"r\"\npath = \"/tmp\"\ndefault_agent = \"herdr\"",
        )
        .unwrap_err();
        assert!(err.to_string().contains("default_agent"), "got {err}");
    }

    #[test]
    fn cleanup_presets_parse() {
        // The `keep_*` retention presets (#210) — note the explicit serde
        // renames: plain snake_case would demand `keep7d`.
        let cfg = RootConfig::from_toml_str(
            r#"
[worktree]
cleanup = "keep_7d"
plan_cleanup = "keep_28d"
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.worktree.cleanup,
            Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep7d))
        );
        assert_eq!(
            cfg.worktree.plan_cleanup,
            Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep28d))
        );
        // Only the two presets exist — other durations use the explicit form.
        assert!(RootConfig::from_toml_str("[worktree]\ncleanup = \"keep_14d\"").is_err());
        assert!(RootConfig::from_toml_str("[worktree]\ncleanup = \"keep7d\"").is_err());
    }
}
