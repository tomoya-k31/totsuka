//! Launch-spec assembly for enabled plugins (F-58/64/65, moved from the CLI
//! in #217).
//!
//! [`plugin_spec`] combines the on-disk store (manifest, binary path), the
//! plugin's secret-resolved `plugins/{name}.toml`, and the `config.toml`
//! material a task_source needs at `initialize` (repositories #109, `[llm]`
//! defaults #119, workflow triggers + poll cadence 0.1.6) into one
//! [`PluginSpec`] for [`plugin_host`](crate::adapters::plugin_host).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use plugin_protocol::manifest::PluginKind;
use plugin_protocol::methods::{LlmInfo, RepoInfo, TriggerInfo};
use serde_json::Value;

use crate::adapters::plugin_host::PluginSpec;
use crate::config::{
    self, ConfigError, PluginRawConfig, ResolveError, RootConfig, resolve_strings, secret_resolver,
};
use crate::plugins::{PluginStore, StoreError};

/// Default per-call plugin RPC timeout when `timeout_secs` is omitted.
pub const DEFAULT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Errors from assembling a plugin's launch spec.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    /// The plugin is declared enabled in config but has no installed binary.
    #[error("plugin `{0}` is enabled but not installed → `totsuka plugin install <dir>`")]
    NotInstalled(String),
    /// The plugin store failed (unreadable manifest, invalid name, ...).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// `plugins/{name}.toml` could not be read.
    #[error("could not read plugin config {path}: {source}")]
    Io {
        /// The unreadable `plugins/{name}.toml`.
        path: PathBuf,
        /// The underlying filesystem error.
        source: io::Error,
    },
    /// `plugins/{name}.toml` could not be parsed or converted.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A secret reference inside `plugins/{name}.toml` did not resolve.
    #[error("in {path}: {source}")]
    Resolve {
        /// The offending `plugins/{name}.toml`.
        path: PathBuf,
        /// What failed to resolve.
        source: ResolveError,
    },
}

/// Build the [`PluginSpec`] for one enabled plugin from the store and its
/// secret-resolved `plugins/{name}.toml` (F-58/64/65). `plugin_config_dir` is
/// the directory holding `plugins/{name}.toml` (next to `config.toml`).
pub fn plugin_spec(
    store: &PluginStore,
    plugin_config_dir: &Path,
    cfg: &RootConfig,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<PluginSpec, SpecError> {
    let manifest = store
        .manifest_of(name)?
        .ok_or_else(|| SpecError::NotInstalled(name.to_string()))?;
    let init_config = plugin_init_config(plugin_config_dir, name, env)?;
    let timeout = cfg
        .plugin(name)
        .and_then(|p| p.timeout_secs)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_PLUGIN_TIMEOUT);
    // task_source plugins get the orchestrator's repository list (#109),
    // `[llm]` settings (#119), and — 0.1.6 — their workflow triggers plus
    // `poll_interval_secs` at `initialize`, so a push source knows its watch
    // conditions and cadence without a `tasks/fetch` call carrying them.
    let (repositories, llm, triggers, poll_interval_secs) =
        if manifest.kind == PluginKind::TaskSource {
            let triggers = cfg
                .workflows
                .iter()
                .filter(|w| w.source == name)
                .map(|w| TriggerInfo {
                    workflow: w.name.clone(),
                    trigger: serde_json::to_value(&w.trigger).unwrap_or(serde_json::Value::Null),
                })
                .collect();
            let poll = cfg.plugin(name).and_then(|p| p.poll_interval_secs);
            (repo_infos(cfg, env), llm_info(cfg, env), triggers, poll)
        } else {
            (vec![], None, vec![], None)
        };
    Ok(PluginSpec {
        name: name.to_string(),
        program: store.plugin_dir(name).join(&manifest.name),
        args: vec![],
        manifest,
        init_config,
        repositories,
        llm,
        triggers,
        poll_interval_secs,
        timeout,
    })
}

/// `config.toml` `[[repositories]]` mapped to the protocol's [`RepoInfo`],
/// with paths `~`/`${ENV}`-expanded (best effort: an unresolvable path is
/// passed through raw — the plugin treats paths as optional material).
fn repo_infos(cfg: &RootConfig, env: &HashMap<String, String>) -> Vec<RepoInfo> {
    let env_fn = |k: &str| env.get(k).cloned();
    cfg.repositories
        .iter()
        .map(|repo| {
            let raw = repo.path.to_string_lossy();
            let path = config::expand_path(&raw, &env_fn)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| raw.into_owned());
            RepoInfo {
                name: repo.name.clone(),
                summary: repo.summary.clone(),
                path: Some(path),
            }
        })
        .collect()
}

/// `config.toml` `[llm]` mapped to the protocol's [`LlmInfo`] with its
/// `api_key_ref` resolved (F-65), supplied to task_source plugins as a
/// source-side classification default (#119). Best effort: an unresolvable
/// key reference yields `None` (nothing supplied) rather than an error —
/// `doctor`'s dedicated `llm` check reports the broken reference, and
/// `totsuka run` fails when building the orchestrator's own router from the
/// same reference, so the problem surfaces where it can be acted on without
/// also failing every plugin launch here.
fn llm_info(cfg: &RootConfig, env: &HashMap<String, String>) -> Option<LlmInfo> {
    let llm = cfg.llm.as_ref()?;
    let api_key = match &llm.api_key_ref {
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(secret) => Some(secret.expose().to_string()),
            Err(_) => return None,
        },
        None => None,
    };
    Some(LlmInfo {
        base_url: llm.base_url.clone(),
        model: llm.model.clone(),
        api_key,
    })
}

/// Load `plugins/{name}.toml` (empty object if absent) and resolve secret
/// references in its string values (F-65).
pub fn plugin_init_config(
    plugin_config_dir: &Path,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<Value, SpecError> {
    let path = plugin_config_dir.join(format!("{name}.toml"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => PluginRawConfig::from_toml_str(&s)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => PluginRawConfig::from_toml_str("")?,
        Err(e) => {
            return Err(SpecError::Io { path, source: e });
        }
    };
    let mut value = raw.to_json()?;
    let resolver = secret_resolver(env);
    resolve_strings(&mut value, &resolver).map_err(|source| SpecError::Resolve { path, source })?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(toml: &str) -> RootConfig {
        RootConfig::from_toml_str(toml).unwrap()
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn llm_info_is_none_without_an_llm_table() {
        assert!(llm_info(&root(""), &env(&[])).is_none());
    }

    #[test]
    fn llm_info_resolves_an_env_key_reference() {
        let cfg = root(
            r#"
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4.5"
api_key_ref = "${OPENROUTER_API_KEY}"
"#,
        );
        let info = llm_info(&cfg, &env(&[("OPENROUTER_API_KEY", "sk-or-test")])).unwrap();
        assert_eq!(info.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(info.model, "anthropic/claude-haiku-4.5");
        assert_eq!(info.api_key.as_deref(), Some("sk-or-test"));
    }

    #[test]
    fn llm_info_without_a_key_reference_has_no_key() {
        let cfg = root(
            r#"
[llm]
base_url = "http://localhost:11434/v1"
model = "local"
"#,
        );
        let info = llm_info(&cfg, &env(&[])).unwrap();
        assert!(info.api_key.is_none());
    }

    #[test]
    fn llm_info_is_best_effort_on_an_unresolvable_reference() {
        // Nothing is supplied rather than failing every plugin launch —
        // doctor's `llm` check and `totsuka run`'s own router construction
        // surface the broken reference.
        let cfg = root(
            r#"
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "m"
api_key_ref = "${UNSET_VAR_FOR_TEST}"
"#,
        );
        assert!(llm_info(&cfg, &env(&[])).is_none());
    }
}
