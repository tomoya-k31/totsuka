//! Static configuration validation (F-63 `--offline`, F-58).
//!
//! These checks need no plugin process: schema version, repository path
//! existence, unique names, and workflow/repository references to plugins
//! (including the rule that a **disabled** plugin must not be referenced,
//! F-58). Checks that require launching plugins (F-59) are wired in #51.

use std::collections::HashSet;

use super::resolve::expand_path;
use super::schema::{CURRENT_SCHEMA_VERSION, PluginKind, RootConfig};

/// A single static-validation failure. `Display` gives "cause + next action".
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    /// Config schema version is newer/older than supported.
    #[error(
        "unsupported config schema version {found} (supported: {expected}) → upgrade totsuka or run `totsuka config migrate`"
    )]
    UnsupportedVersion { found: u32, expected: u32 },

    /// A repository path does not exist on disk.
    #[error(
        "repository `{name}` path does not exist: {path} → fix `path` in config.toml or clone the repo"
    )]
    RepoPathMissing { name: String, path: String },

    /// A repository path could not be expanded (e.g. missing env var).
    #[error("repository `{name}` path is unresolvable: {reason}")]
    RepoPathUnresolvable { name: String, reason: String },

    /// Two repositories share a name.
    #[error("duplicate repository name `{0}` → repository names must be unique")]
    DuplicateRepo(String),

    /// Two workflows share a name.
    #[error("duplicate workflow name `{0}` → workflow names must be unique")]
    DuplicateWorkflow(String),

    /// A workflow/repo references a plugin that is not defined.
    #[error(
        "{referrer} references unknown plugin `{plugin}` → add `[plugins.{plugin}]` or fix the name"
    )]
    UnknownPluginRef { referrer: String, plugin: String },

    /// A workflow/repo references a disabled plugin (F-58).
    #[error(
        "{referrer} references disabled plugin `{plugin}` → set `[plugins.{plugin}] enabled = true` or point it elsewhere"
    )]
    DisabledPluginRef { referrer: String, plugin: String },

    /// A referenced plugin has the wrong kind for the role.
    #[error(
        "{referrer} plugin `{plugin}` has the wrong kind (expected {expected:?}) → use a {expected:?} plugin"
    )]
    WrongPluginKind {
        referrer: String,
        plugin: String,
        expected: PluginKind,
    },
}

/// Run all static checks, returning every problem found (empty = valid).
pub fn validate_static<E>(cfg: &RootConfig, env: &E) -> Vec<ValidationError>
where
    E: Fn(&str) -> Option<String>,
{
    let mut errors = Vec::new();

    if cfg.version != CURRENT_SCHEMA_VERSION {
        errors.push(ValidationError::UnsupportedVersion {
            found: cfg.version,
            expected: CURRENT_SCHEMA_VERSION,
        });
    }

    // Repositories: unique names + path existence.
    let mut seen_repos = HashSet::new();
    for repo in &cfg.repositories {
        if !seen_repos.insert(repo.name.as_str()) {
            errors.push(ValidationError::DuplicateRepo(repo.name.clone()));
        }
        match expand_path(&repo.path.to_string_lossy(), env) {
            Ok(path) if path.exists() => {}
            Ok(path) => errors.push(ValidationError::RepoPathMissing {
                name: repo.name.clone(),
                path: path.display().to_string(),
            }),
            Err(e) => errors.push(ValidationError::RepoPathUnresolvable {
                name: repo.name.clone(),
                reason: e.to_string(),
            }),
        }
        if let Some(agent) = &repo.default_agent {
            check_plugin_ref(
                cfg,
                &format!("repository `{}` default_agent", repo.name),
                agent,
                PluginKind::AgentIde,
                &mut errors,
            );
        }
    }

    // Workflows: unique names + source/agent references.
    let mut seen_workflows = HashSet::new();
    for wf in &cfg.workflows {
        if !seen_workflows.insert(wf.name.as_str()) {
            errors.push(ValidationError::DuplicateWorkflow(wf.name.clone()));
        }
        check_plugin_ref(
            cfg,
            &format!("workflow `{}` source", wf.name),
            &wf.source,
            PluginKind::TaskSource,
            &mut errors,
        );
        check_plugin_ref(
            cfg,
            &format!("workflow `{}` agent", wf.name),
            &wf.agent,
            PluginKind::AgentIde,
            &mut errors,
        );
    }

    errors
}

/// Validate one plugin reference: it must exist, be enabled (F-58), and be of
/// the expected kind.
fn check_plugin_ref(
    cfg: &RootConfig,
    referrer: &str,
    plugin: &str,
    expected: PluginKind,
    errors: &mut Vec<ValidationError>,
) {
    match cfg.plugin(plugin) {
        None => errors.push(ValidationError::UnknownPluginRef {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
        }),
        Some(p) if !p.enabled => errors.push(ValidationError::DisabledPluginRef {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
        }),
        Some(p) if p.kind != expected => errors.push(ValidationError::WrongPluginKind {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
            expected,
        }),
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn valid_config_has_no_errors() {
        // Use the repo root itself as an existing path.
        let dir = env!("CARGO_MANIFEST_DIR");
        let toml = format!(
            r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[repositories]]
name = "totsuka"
path = "{dir}"
default_agent = "herdr"

[[workflows]]
name = "impl"
source = "github"
trigger = {{ project_status = "todo" }}
mode = "implement"
agent = "herdr"
output = "pull_request"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn disabled_plugin_reference_is_rejected() {
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = false
kind = "agent_ide"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::DisabledPluginRef { plugin, .. } if plugin == "herdr"
        )));
    }

    #[test]
    fn unknown_and_wrong_kind_and_missing_path_are_reported() {
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[[repositories]]
name = "missing"
path = "/nonexistent/totsuka/repo"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "github"
output = "none"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        // agent points at a task_source plugin -> wrong kind.
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::WrongPluginKind { plugin, .. } if plugin == "github"
        )));
        // repo path does not exist.
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::RepoPathMissing { name, .. } if name == "missing"
        )));
    }

    #[test]
    fn unsupported_version_is_reported() {
        let cfg = RootConfig::from_toml_str("version = 999").unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::UnsupportedVersion { found: 999, .. }))
        );
    }
}
