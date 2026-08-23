//! Configuration loading, secret resolution, precedence, and static validation
//! (§4.7, F-60–F-66).
//!
//! - [`schema`]: `config.toml` types and parsing.
//! - [`raw`]: uninterpreted `plugins/{name}.toml` (F-64).
//! - [`resolve`]: `${ENV}` / `keychain:` secret resolution and path expansion.
//! - [`env_overrides`]: `TOTSUKA_*` overrides, layer 2 of the CLI > env >
//!   plugin-file > config-default precedence (F-66).
//! - [`mod@validate`]: static (offline) validation (F-63, F-58).

pub mod edit;
pub mod env_overrides;
pub mod raw;
pub mod resolve;
pub mod schema;
pub mod validate;

pub use edit::{
    EditError, RepositoryDraft, WorkflowDraft, set_default_tool, set_hooks_auth_token_ref, set_llm,
    set_plugin_enabled, set_tool, upsert_repository, upsert_workflow,
};

pub use env_overrides::{ENV_PREFIX, apply_env_overrides, override_keys};
pub use raw::PluginRawConfig;
pub use resolve::{
    ResolveError, SecretResolver, expand_env, expand_path, resolve_strings, secret_resolver,
};
pub use schema::{
    CURRENT_SCHEMA_VERSION, CleanupPolicyConfig, CleanupPolicyName, ConfigError,
    DEFAULT_BLOCK_RETRY_LIMIT, DEFAULT_GLOBAL_CONCURRENCY, DEFAULT_POLL_INTERVAL_SECS,
    DEFAULT_WORKFLOW_TIMEOUT_SECS, HooksConfig, LlmConfig, LogSettings, OutputPolicy, PluginConfig,
    PluginKind, Profile, PublishConfig, RepositoryConfig, RootConfig, ToolConfig, VerificationMode,
    WorkflowConfig, WorkflowMode, WorktreeConfig,
};
pub use validate::{
    Finding, FindingSeverity, ValidationError, has_errors, validate, validate_static,
};
