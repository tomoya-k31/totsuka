//! `config.toml` schema (F-60, F-61, F-64) and parsing.
//!
//! The two-layer configuration model (§4.6): common, Orchestrator-interpreted
//! fields live in `config.toml`; plugin-specific settings live in
//! `plugins/{name}.toml` and are held uninterpreted (see
//! [`PluginRawConfig`](crate::config::PluginRawConfig)).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// The current supported config schema version (§10.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Default global concurrent-task limit when `max_concurrency` is omitted
/// (F-40).
pub const DEFAULT_GLOBAL_CONCURRENCY: u32 = 4;

/// Default task-source polling interval in seconds when `poll_interval_secs`
/// is omitted (F-06).
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Root of `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    /// Schema version, for startup migration (§10.2).
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
    /// Default agent plugin for this repo (must be an enabled `agent_ide`).
    #[serde(default)]
    pub default_agent: Option<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicyName {
    /// Remove as soon as the task finishes.
    Immediate,
    /// Never auto-remove; a human cleans up.
    Manual,
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
worktree_location = "{repo}/../.worktrees/{branch}"

[worktree]
location = "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{branch}"

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
}
