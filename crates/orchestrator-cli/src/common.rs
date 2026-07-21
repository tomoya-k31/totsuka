//! Shared CLI plumbing: path/config resolution, plugin-spec assembly, and the
//! "cause + next action" error convention (§7).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orchestrator_core::adapters::StateDb;
use orchestrator_core::adapters::plugin_host::PluginSpec;
use orchestrator_core::config::{self, PluginRawConfig, RootConfig, SecretResolver};
use orchestrator_core::paths::Paths;
use orchestrator_core::platform::PlatformSecretStore;
use orchestrator_core::plugins::PluginStore;
use plugin_protocol::manifest::PluginKind;
use plugin_protocol::methods::{LlmInfo, RepoInfo, TriggerInfo};
use serde_json::Value;

/// A boxed error for CLI operations.
pub type CliError = Box<dyn std::error::Error>;

/// Default per-call plugin RPC timeout when `timeout_secs` is omitted.
pub const DEFAULT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolved locations every command operates on. `--config` overrides the
/// config file path (highest layer of F-66); everything else stays XDG.
pub struct Cx {
    /// XDG-resolved application directories.
    pub paths: Paths,
    /// Path of `config.toml` (possibly overridden by `--config`).
    pub config_path: PathBuf,
}

impl Cx {
    /// Resolve paths, honoring a `--config <path>` override.
    pub fn resolve(config_override: Option<&Path>) -> Result<Self, CliError> {
        let paths = Paths::from_system()?;
        let config_path = match config_override {
            Some(path) => path.to_path_buf(),
            None => paths.config_dir().join("config.toml"),
        };
        Ok(Self { paths, config_path })
    }

    /// Load and parse `config.toml` and apply the `TOTSUKA_*` overrides
    /// (F-66 layer 2), with an actionable error when the file is absent.
    ///
    /// Every command that reads config goes through here so the layers stay
    /// consistent: `totsuka focus` / `doctor` resolve `[hooks].socket_path` to
    /// find the socket `totsuka run` bound, so applying the env layer in `run`
    /// alone would leave them looking at a different socket. CLI flags are
    /// applied by the individual commands *after* this call, which is what
    /// makes "CLI > env" hold.
    ///
    /// Warnings (unknown `TOTSUKA_*` names, empty values) go to stderr so the
    /// `--json` commands' stdout contract stays parseable.
    pub fn load_config(&self, env: &HashMap<String, String>) -> Result<RootConfig, CliError> {
        match std::fs::read_to_string(&self.config_path) {
            Ok(s) => {
                let mut cfg = RootConfig::from_toml_str(&s)?;
                let warnings = config::apply_env_overrides(
                    &mut cfg,
                    env.iter().map(|(k, v)| (k.clone(), v.clone())),
                )?;
                for warning in warnings {
                    eprintln!("config warning: {warning}");
                }
                Ok(cfg)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!(
                "config not found at {} → run `totsuka init` to create it",
                self.config_path.display()
            )
            .into()),
            Err(e) => Err(e.into()),
        }
    }

    /// Path of the state database.
    pub fn state_db_path(&self) -> PathBuf {
        self.paths.state_dir().join("state.db")
    }

    /// Open the state DB for reading, with an actionable error when it does
    /// not exist yet (opening would silently create an empty one).
    pub fn open_state_db(&self) -> Result<StateDb, CliError> {
        let path = self.state_db_path();
        if !path.exists() {
            return Err(format!(
                "state database not found at {} → run `totsuka run` at least once",
                path.display()
            )
            .into());
        }
        Ok(StateDb::open(&path)?)
    }

    /// The plugin store under the data directory.
    pub fn store(&self) -> PluginStore {
        PluginStore::new(self.paths.data_dir().join("plugins"))
    }

    /// Directory holding `plugins/{name}.toml` files (next to config.toml).
    pub fn plugin_config_dir(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins")
    }
}

/// Build the [`PluginSpec`] for one enabled plugin from the store and its
/// secret-resolved `plugins/{name}.toml` (F-58/64/65).
pub fn plugin_spec(
    cx: &Cx,
    cfg: &RootConfig,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<PluginSpec, CliError> {
    let store = cx.store();
    let manifest = store.manifest_of(name)?.ok_or_else(|| {
        format!("plugin `{name}` is enabled but not installed → `totsuka plugin install <dir>`")
    })?;
    let init_config = plugin_init_config(cx, name, env)?;
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
    cx: &Cx,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = cx.plugin_config_dir().join(format!("{name}.toml"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => PluginRawConfig::from_toml_str(&s)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => PluginRawConfig::from_toml_str("")?,
        Err(e) => return Err(e.into()),
    };
    let mut value = raw.to_json()?;
    let resolver = secret_resolver(env);
    resolve_strings(&mut value, &resolver).map_err(|e| format!("in {}: {e}", path.display()))?;
    Ok(value)
}

/// The hook/control UDS socket path: `[hooks].socket_path` (with `~`/`${ENV}`
/// expanded) or the XDG runtime default. Shared by `doctor`'s probe and
/// `totsuka focus` so both always target the socket `totsuka run` binds.
pub fn hook_socket_path(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
) -> Result<PathBuf, CliError> {
    let env_fn = |k: &str| env.get(k).cloned();
    match &cfg.hooks.socket_path {
        Some(raw) => config::expand_path(raw, &env_fn)
            .map_err(|e| format!("[hooks].socket_path does not expand: {e}").into()),
        None => Ok(cx.paths.runtime_dir().join("claude-events.sock")),
    }
}

/// The platform secret resolver over a snapshot of the environment.
pub fn secret_resolver(
    env: &HashMap<String, String>,
) -> SecretResolver<PlatformSecretStore, impl Fn(&str) -> Option<String> + '_> {
    SecretResolver::new(PlatformSecretStore::default(), |k: &str| {
        env.get(k).cloned()
    })
}

/// Recursively resolve `${ENV}` / `keychain:` / `op://` references in every
/// string leaf. Generic over the store so tests can inject a fake.
pub fn resolve_strings<S, E>(
    value: &mut Value,
    resolver: &SecretResolver<S, E>,
) -> Result<(), orchestrator_core::config::ResolveError>
where
    S: orchestrator_core::ports::SecretStore,
    E: Fn(&str) -> Option<String>,
{
    match value {
        Value::String(s) => {
            *s = resolver.resolve(s)?.expose().to_string();
        }
        Value::Array(items) => {
            for item in items {
                resolve_strings(item, resolver)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                resolve_strings(item, resolver)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Print a value as pretty JSON (the `--json` contract: parseable output on
/// stdout, nothing else).
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
