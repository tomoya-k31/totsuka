//! Shared CLI plumbing: path/config resolution, plugin-spec assembly, and the
//! "cause + next action" error convention (§7).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orchestrator_core::adapters::StateDb;
use orchestrator_core::adapters::plugin_host::PluginSpec;
use orchestrator_core::config::{PluginRawConfig, RootConfig, SecretResolver};
use orchestrator_core::paths::Paths;
use orchestrator_core::platform::PlatformSecretStore;
use orchestrator_core::plugins::PluginStore;
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

    /// Load and parse `config.toml`, with an actionable error when absent.
    pub fn load_config(&self) -> Result<RootConfig, CliError> {
        match std::fs::read_to_string(&self.config_path) {
            Ok(s) => Ok(RootConfig::from_toml_str(&s)?),
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
    Ok(PluginSpec {
        name: name.to_string(),
        program: store.plugin_dir(name).join(&manifest.name),
        args: vec![],
        manifest,
        init_config,
        timeout,
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

/// The platform secret resolver over a snapshot of the environment.
pub fn secret_resolver(
    env: &HashMap<String, String>,
) -> SecretResolver<PlatformSecretStore, impl Fn(&str) -> Option<String> + '_> {
    SecretResolver::new(PlatformSecretStore::default(), |k: &str| {
        env.get(k).cloned()
    })
}

/// Recursively resolve `${ENV}` / `keychain:` references in every string leaf.
pub fn resolve_strings<E>(
    value: &mut Value,
    resolver: &SecretResolver<PlatformSecretStore, E>,
) -> Result<(), orchestrator_core::config::ResolveError>
where
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
