//! `config.toml` schema (F-60, F-61, F-64) and parsing.
//!
//! The two-layer configuration model (§4.6): common, Orchestrator-interpreted
//! fields live in `config.toml`; plugin-specific settings live in
//! `plugins/{name}.toml` and are held uninterpreted (see
//! [`PluginRawConfig`](crate::config::PluginRawConfig)).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::tool::ToolKind;

/// The current supported config schema version (§10.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Default global concurrent-task limit when `max_concurrency` is omitted
/// (F-40).
pub const DEFAULT_GLOBAL_CONCURRENCY: u32 = 4;

/// Default task-source polling interval in seconds when `poll_interval_secs`
/// is omitted (F-06).
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Default number of Stop-hook block re-asks before a task escalates, when
/// `[hooks].block_retry_limit` is omitted (D-02).
pub const DEFAULT_BLOCK_RETRY_LIMIT: u32 = 3;

/// Default per-workflow silence limit in seconds (since the last hook signal)
/// before escalation, when `timeout_secs` is omitted (D-03: 30 minutes).
pub const DEFAULT_WORKFLOW_TIMEOUT_SECS: u64 = 1800;

/// Root of `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    /// Schema version (§10.2). Startup validation rejects a mismatch; the
    /// config is never migrated automatically (#276).
    ///
    /// Note the default: omitting `version` yields whatever
    /// [`CURRENT_SCHEMA_VERSION`] happens to be, so a `version`-less
    /// config.toml written for v1 would be read as v2 the moment this binary
    /// bumps — silently, since the guard above never fires. Deciding that
    /// default is a prerequisite for cutting v2; see the versioning policy in
    /// `docs/development/config-reference.md`.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Global maximum concurrent tasks (F-40). Defaults to
    /// [`DEFAULT_GLOBAL_CONCURRENCY`] when omitted.
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// Registered local repositories (F-61).
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,
    /// Plugin roster + common fields, keyed by plugin instance name (F-56).
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Global default AI tool name when neither the workflow nor the selected
    /// repository picks one (#196). `None` means the built-in `"claude"`.
    #[serde(default)]
    pub default_tool: Option<String>,
    /// AI-tool registry, keyed by tool name (#196). Built-in defaults exist
    /// for `claude`; an entry overrides/extends them (e.g. a
    /// `[tools.claude-fast]` profile with a different model flag).
    #[serde(default)]
    pub tools: BTreeMap<String, ToolConfig>,
    /// Named workflows (parsed structurally here; semantics validated in #54).
    #[serde(default)]
    pub workflows: Vec<WorkflowConfig>,
    /// AI Gateway settings (F-12, F-13).
    pub llm: Option<LlmConfig>,
    /// worktree placement defaults (consumed by #53).
    #[serde(default)]
    pub worktree: WorktreeConfig,
    /// Logging settings (§5.2).
    #[serde(default)]
    pub log: LogSettings,
    /// Output policy settings (PR templates, #65).
    #[serde(default)]
    pub output: OutputSettings,
    /// Claude Code hook-event ingestion settings (#131: E-03, D-02, E-07).
    #[serde(default)]
    pub hooks: HooksConfig,
    /// Prompt-text overrides (#314). Every key is optional and falls back to
    /// the built-in default embedded in
    /// [`prompts`](crate::prompts); a workflow can narrow them further via
    /// [`WorkflowConfig::prompts`].
    #[serde(default)]
    pub prompts: PromptsConfig,
}

/// `[prompts]` — global overrides for the text injected into the AI tool
/// (#314, epic #311).
///
/// Interpreted by [`prompts`](crate::prompts), not here: this struct only
/// records what the operator wrote. Every field is `None` by default, meaning
/// "use the built-in".
///
/// The markers themselves (`<<STATUS:COMPLETED>>` and friends) are **not**
/// configurable — see the module docs of [`prompts`](crate::prompts).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptsConfig {
    /// Completion self-report instruction injected into every hook-capable
    /// dispatch. Placeholders: `{marker_completed}` `{marker_needs_input}`
    /// `{marker_failed}`.
    #[serde(default)]
    pub marker_self_report: Option<String>,
    /// Instruction to create the task's branch, injected when the worktree is
    /// handed over detached (implement mode, not resuming onto an existing
    /// branch). No placeholders.
    ///
    /// Not emitted in plan mode at all — a plan-mode pane cannot run git, so
    /// the text would be unfollowable there regardless of what it says.
    #[serde(default)]
    pub branch_convention: Option<String>,
    /// Judging criteria of the llm-verification prompt hook. The per-workflow
    /// [`rubric`](WorkflowConfig::rubric) key predates this and still wins for
    /// the workflow that sets it.
    #[serde(default)]
    pub verification_rubric: Option<String>,
    /// Intermediate-Stop exemption appended to the rubric.
    #[serde(default)]
    pub verification_background_exemption: Option<String>,
    /// Marker convention appended to the rubric. Placeholders:
    /// `{marker_completed}` `{marker_needs_input}` `{marker_failed}`.
    #[serde(default)]
    pub verification_marker_convention: Option<String>,
    /// How the three keys above are assembled. Placeholders: `{rubric}`
    /// `{background_exemption}` `{marker_convention}`.
    #[serde(default)]
    pub verification_prompt: Option<String>,
    /// Prose body of the opencode plan-mode agent file (#316). No
    /// placeholders.
    ///
    /// **Global only** — one `agents/totsuka-plan.md` on disk backs every
    /// opencode session, and `--agent totsuka-plan` has no per-workflow
    /// dimension, which is why [`WorkflowPromptsConfig`] omits it.
    ///
    /// Body only: the YAML frontmatter carrying `permission: {edit: deny,
    /// bash: deny, task: deny}` is fixed in Rust. Validation rejects a value
    /// starting with `---`.
    #[serde(default)]
    pub opencode_plan_agent: Option<String>,
}

impl PromptsConfig {
    /// The keys the operator actually set, as `(key name, value)` pairs.
    /// Validation walks this instead of hard-coding the field list twice.
    pub fn entries(&self) -> Vec<(&'static str, &str)> {
        [
            ("marker_self_report", &self.marker_self_report),
            ("branch_convention", &self.branch_convention),
            ("verification_rubric", &self.verification_rubric),
            (
                "verification_background_exemption",
                &self.verification_background_exemption,
            ),
            (
                "verification_marker_convention",
                &self.verification_marker_convention,
            ),
            ("verification_prompt", &self.verification_prompt),
            ("opencode_plan_agent", &self.opencode_plan_agent),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_deref().map(|v| (k, v)))
        .collect()
    }
}

/// `[[workflows]].prompts` — the workflow-scoped subset of
/// [`PromptsConfig`] (#314).
///
/// A separate type rather than a reuse of [`PromptsConfig`] because the two
/// diverge: prompts that describe a shared on-disk asset (the opencode plan
/// agent, #316) are global-only, since one file backs every session. Keeping
/// them distinct means `deny_unknown_fields` rejects a global-only key written
/// under a workflow, naming it — no extra validation needed. `serde(flatten)`
/// would have deduplicated the fields but silently disables
/// `deny_unknown_fields`, turning a typo into a silent no-op.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPromptsConfig {
    /// See [`PromptsConfig::marker_self_report`].
    #[serde(default)]
    pub marker_self_report: Option<String>,
    /// See [`PromptsConfig::branch_convention`].
    #[serde(default)]
    pub branch_convention: Option<String>,
    /// See [`PromptsConfig::verification_rubric`].
    #[serde(default)]
    pub verification_rubric: Option<String>,
    /// See [`PromptsConfig::verification_background_exemption`].
    #[serde(default)]
    pub verification_background_exemption: Option<String>,
    /// See [`PromptsConfig::verification_marker_convention`].
    #[serde(default)]
    pub verification_marker_convention: Option<String>,
    /// See [`PromptsConfig::verification_prompt`].
    #[serde(default)]
    pub verification_prompt: Option<String>,
}

impl WorkflowPromptsConfig {
    /// The keys this workflow actually set — see [`PromptsConfig::entries`].
    pub fn entries(&self) -> Vec<(&'static str, &str)> {
        [
            ("marker_self_report", &self.marker_self_report),
            ("branch_convention", &self.branch_convention),
            ("verification_rubric", &self.verification_rubric),
            (
                "verification_background_exemption",
                &self.verification_background_exemption,
            ),
            (
                "verification_marker_convention",
                &self.verification_marker_convention,
            ),
            ("verification_prompt", &self.verification_prompt),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_deref().map(|v| (k, v)))
        .collect()
    }
}

/// Hook-event ingestion settings from `[hooks]` (#131).
///
/// All fields are optional: the consuming components (UDS server, hook
/// rendering — later issues) apply the documented defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// Secret reference (`${ENV}` or `keychain:`) to the Bearer token that
    /// authenticates hook POSTs (E-03). Operationally required whenever a
    /// hook-capable agent is used (validation warns, `doctor` fails).
    #[serde(default)]
    pub auth_token_ref: Option<String>,
    /// Unix domain socket path the hook receiver listens on. `None` uses the
    /// built-in default path.
    #[serde(default)]
    pub socket_path: Option<String>,
    /// Directory where hook events are spooled when the POST fails (E-07).
    /// `None` uses the built-in default path.
    #[serde(default)]
    pub spool_dir: Option<String>,
    /// Max consecutive Stop-hook block re-asks before escalation (D-02).
    /// Defaults to [`DEFAULT_BLOCK_RETRY_LIMIT`].
    #[serde(default)]
    pub block_retry_limit: Option<u32>,
}

/// Output policy settings (F-86 PR templating).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSettings {
    /// Pull-request title template; `None` uses the built-in default.
    /// Placeholders: `{title}` `{task_id}` `{source}`.
    #[serde(default)]
    pub pr_title_template: Option<String>,
    /// Pull-request body template; `None` uses the built-in default.
    /// Placeholders: `{title}` `{url}` `{source}` `{task_id}` `{summary}`.
    #[serde(default)]
    pub pr_body_template: Option<String>,
}

fn default_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// A registered repository (F-61).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    /// Stable identifier used in branch names and logs.
    pub name: String,
    /// Local clone path (may use `~` and `${ENV}`; expanded on validation).
    pub path: PathBuf,
    /// Free-text summary used for LLM repo selection (F-11).
    #[serde(default)]
    pub summary: Option<String>,
    /// Default AI tool for tasks dispatched into this repo (#196). Overrides
    /// `default_tool`; overridden by an explicit `[[workflows]].tool` pin.
    #[serde(default)]
    pub tool: Option<String>,
    /// Per-repository concurrency cap (F-41).
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// Overrides the global `[worktree].location` for this repo (F-22).
    #[serde(default)]
    pub worktree_location: Option<String>,
}

/// Plugin kind (F-50).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    /// Whether the plugin is active (F-56). Declarative roster flag.
    #[serde(default)]
    pub enabled: bool,
    /// Plugin kind.
    pub kind: PluginKind,
    /// Per-plugin concurrency cap (F-42).
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// RPC timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Plugin log level.
    #[serde(default)]
    pub log_level: Option<String>,
    /// Polling interval in seconds for `run --watch` (task sources only,
    /// F-06). Defaults to [`DEFAULT_POLL_INTERVAL_SECS`].
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
}

/// Execution mode of a workflow (F-80, F-82).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMode {
    /// Detailed design: worktree created, but no push/PR.
    Plan,
    /// Implementation.
    Implement,
}

/// Output policy of a workflow (F-83).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputPolicy {
    /// Push + open a pull request.
    PullRequest,
    /// Write back to the task source (`result/publish`).
    Source,
    /// No output.
    None,
}

/// How a workflow's completion self-report is verified (D-01).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    /// In-session LLM verification via a prompt-type Stop hook (default).
    #[default]
    Llm,
    /// A human verifies via `totsuka task verify`; the task waits in
    /// `Verifying` until then.
    Human,
    /// No verification; a completion self-report is accepted as-is.
    None,
}

impl VerificationMode {
    /// The stable snake_case config string for this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationMode::Llm => "llm",
            VerificationMode::Human => "human",
            VerificationMode::None => "none",
        }
    }
}

/// A named workflow (F-80). Parsed structurally; trigger/handoff semantics are
/// validated and matched in #54.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowConfig {
    /// Workflow name.
    pub name: String,
    /// Task source instance name (must be an enabled `task_source`).
    pub source: String,
    /// Trigger condition; kept raw (interpreted in #54).
    #[serde(default)]
    pub trigger: toml::Table,
    /// Execution mode.
    pub mode: WorkflowMode,
    /// Agent plugin instance name (must be an enabled `agent_ide`).
    pub agent: String,
    /// Output policy.
    pub output: OutputPolicy,
    /// Source status transition on success; kept raw (interpreted in #54).
    #[serde(default)]
    pub on_success: Option<toml::Table>,
    /// Source status transition on failure; kept raw (interpreted in #54).
    #[serde(default)]
    pub on_failure: Option<toml::Table>,
    /// How completion self-reports are verified (D-01). Defaults to `llm`.
    #[serde(default)]
    pub verification: VerificationMode,
    /// Silence limit in seconds since the last hook signal before the task
    /// escalates (D-03). Defaults to [`DEFAULT_WORKFLOW_TIMEOUT_SECS`].
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Criteria text embedded into the llm-verification prompt hook. Only
    /// meaningful with `verification = "llm"` (validation warns otherwise).
    ///
    /// Predates `[prompts]` (#314) and is kept working: it outranks
    /// [`PromptsConfig::verification_rubric`] because both are about this
    /// workflow, and letting a newly-added global key silently override every
    /// existing per-workflow `rubric` would be a real regression. It loses only
    /// to [`prompts.verification_rubric`](WorkflowPromptsConfig::verification_rubric)
    /// on the same workflow.
    #[serde(default)]
    pub rubric: Option<String>,
    /// Prompt overrides scoped to this workflow (#314) — the strongest layer,
    /// above `[prompts]` and above [`rubric`](Self::rubric).
    #[serde(default)]
    pub prompts: WorkflowPromptsConfig,
    /// Explicit AI-tool pin for this workflow (#196) — the strongest level of
    /// the tool precedence (workflow > repo > `default_tool`). Use it when the
    /// flow's shape demands a specific tool (e.g. `verification = "llm"`
    /// needs Claude's prompt-type Stop hook). `None` falls through to the
    /// repository/global defaults.
    #[serde(default)]
    pub tool: Option<String>,
}

/// A `[tools.<name>]` registry entry (#196): how to launch one AI tool CLI.
/// Interpreted into a [`ToolProfile`](crate::tool::ToolProfile).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    /// Adapter family (`claude` | `codex` | `opencode`) — determines argv
    /// assembly and completion detection.
    pub kind: ToolKind,
    /// Whitespace-split command line (first token = program, rest = base
    /// args). `None` uses the kind's name as the program.
    #[serde(default)]
    pub command: Option<String>,
    /// Extra args appended in implement mode (overrides the kind default).
    #[serde(default)]
    pub mode_args: Option<Vec<String>>,
    /// Extra args appended in plan mode (overrides the kind default).
    #[serde(default)]
    pub plan_args: Option<Vec<String>>,
}

/// AI Gateway (OpenAI-compatible) settings (F-12, F-13).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// OpenAI-compatible base URL (`/chat/completions`).
    pub base_url: String,
    /// Model name (cheap model assumed).
    pub model: String,
    /// Max tokens for the classification call.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Request timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Secret reference to the API key (`${ENV}` or `keychain:`).
    #[serde(default)]
    pub api_key_ref: Option<String>,
}

/// worktree placement defaults (F-22) and cleanup policies (F-23, F-85).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeConfig {
    /// Global placement template; overridable per repo.
    #[serde(default)]
    pub location: Option<String>,
    /// Cleanup policy for implement-mode worktrees (F-23). Defaults to
    /// `manual` (never lose committed-but-unpushed work).
    #[serde(default)]
    pub cleanup: Option<CleanupPolicyConfig>,
    /// Cleanup policy for plan-mode worktrees (F-85). Defaults to `immediate`
    /// (design-only worktrees carry no unique work).
    #[serde(default)]
    pub plan_cleanup: Option<CleanupPolicyConfig>,
}

/// A worktree cleanup policy as written in config (F-23):
/// `"immediate"`, `"manual"`, or `{ retention_days = 5 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum CleanupPolicyConfig {
    /// A named policy (`immediate` / `manual`).
    Named(CleanupPolicyName),
    /// Keep for N days after the task finished, then remove.
    Retention {
        /// Days to keep a finished task's worktree.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSettings {
    /// Minimum level name (`error`/`warn`/`info`/`debug`/`trace`).
    #[serde(default)]
    pub level: Option<String>,
    /// Whether prompt/RPC-payload fields are logged (debug+ only regardless).
    #[serde(default = "default_log_prompts")]
    pub log_prompts: bool,
    /// Number of daily log files to keep.
    #[serde(default)]
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
    /// Converting a raw plugin config to JSON failed (F-64).
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

impl RootConfig {
    /// Parse a `config.toml` document.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(s)?)
    }

    /// Look up a plugin's common config by instance name.
    pub fn plugin(&self, name: &str) -> Option<&PluginConfig> {
        self.plugins.get(name)
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

[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
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
        assert_eq!(design.mode, WorkflowMode::Plan);
        assert_eq!(design.output, OutputPolicy::Source);
        assert_eq!(
            design.trigger.get("project_status").unwrap().as_str(),
            Some("設計待ち")
        );
        assert_eq!(cfg.workflows[1].output, OutputPolicy::PullRequest);

        let llm = cfg.llm.as_ref().unwrap();
        assert_eq!(
            llm.api_key_ref.as_deref(),
            Some("keychain:totsuka/openrouter")
        );
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = RootConfig::from_toml_str("bogus_key = 1").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn hooks_and_verification_fields_parse() {
        let cfg = RootConfig::from_toml_str(
            r#"
[hooks]
auth_token_ref = "keychain:totsuka/hook-token"
socket_path = "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock"
spool_dir = "${XDG_STATE_HOME}/totsuka/hooks/spool"
block_retry_limit = 3

[[workflows]]
name = "slack-reply"
source = "slack"
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
            cfg.hooks.auth_token_ref.as_deref(),
            Some("keychain:totsuka/hook-token")
        );
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
        assert_eq!(wf.verification, VerificationMode::Human);
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
        assert!(cfg.hooks.auth_token_ref.is_none());
        assert!(cfg.hooks.socket_path.is_none());
        assert!(cfg.hooks.spool_dir.is_none());
        assert!(cfg.hooks.block_retry_limit.is_none());
        for wf in &cfg.workflows {
            assert_eq!(wf.verification, VerificationMode::Llm);
            assert!(wf.timeout_secs.is_none());
            assert!(wf.rubric.is_none());
        }
    }

    #[test]
    fn unknown_hooks_key_is_rejected() {
        // Typo inside [hooks] (auth_token vs auth_token_ref) must not be
        // silently ignored.
        let err = RootConfig::from_toml_str("[hooks]\nauth_token = \"x\"").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
        // An unknown verification mode is rejected, not defaulted.
        assert!(
            RootConfig::from_toml_str(
                r#"
[[workflows]]
name = "w"
source = "s"
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
poll_interval_secs = 30

[worktree]
cleanup = { retention_days = 5 }
plan_cleanup = "immediate"
"#,
        )
        .unwrap();
        assert_eq!(cfg.max_concurrency, Some(8));
        assert_eq!(cfg.plugin("github").unwrap().poll_interval_secs, Some(30));
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

[[workflows]]
name = "reply"
source = "slack"
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
