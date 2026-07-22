//! Shared CLI plumbing: path/config resolution and the "cause + next action"
//! error convention (§7). Plugin-spec assembly and secret resolution live in
//! core (`orchestrator_core::plugins::spec` / `config::resolve`, #217).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::StateDb;
use orchestrator_core::config::{self, RootConfig};
use orchestrator_core::paths::Paths;
use orchestrator_core::plugins::PluginStore;

/// A boxed error for CLI operations.
pub type CliError = Box<dyn std::error::Error>;

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

    /// The `plugins/` directory itself (next to config.toml), holding one
    /// `{name}.toml` per plugin.
    pub fn plugin_config_dir(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins")
    }
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

/// Print a value as pretty JSON (the `--json` contract: parseable output on
/// stdout, nothing else).
pub fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
