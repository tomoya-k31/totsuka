//! Configuration loading, secret resolution, precedence, and static validation
//! (§4.7, F-60–F-66).
//!
//! - [`schema`]: `config.toml` types and parsing.
//! - [`raw`]: uninterpreted `plugins/{name}.toml` (F-64).
//! - [`resolve`]: `${ENV}` / `keychain:` secret resolution and path expansion.
//! - [`layered`]: CLI > env > plugin-file > config-default precedence (F-66).
//! - [`validate`]: static (offline) validation (F-63, F-58).

pub mod edit;
pub mod layered;
pub mod raw;
pub mod resolve;
pub mod schema;
pub mod validate;

pub use edit::{EditError, set_plugin_enabled};

pub use layered::ConfigResolver;
pub use raw::PluginRawConfig;
pub use resolve::{ResolveError, SecretResolver, expand_env, expand_path};
pub use schema::{
    CURRENT_SCHEMA_VERSION, CleanupPolicyConfig, CleanupPolicyName, ConfigError,
    DEFAULT_GLOBAL_CONCURRENCY, DEFAULT_POLL_INTERVAL_SECS, LlmConfig, LogSettings, OutputPolicy,
    PluginConfig, PluginKind, RepositoryConfig, RootConfig, WorkflowConfig, WorkflowMode,
    WorktreeConfig,
};
pub use validate::{
    Finding, FindingSeverity, ValidationError, has_errors, validate, validate_static,
};
