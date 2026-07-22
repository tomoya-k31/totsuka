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

// Process exit codes (#177; the table lives in
// docs/components/orchestrator-cli.md). 0 = success is expressed as
// `ExitCode::SUCCESS` at the call site; 2 also covers clap's own parse
// failures, which exit before `main`'s mapping is reached.

/// A runtime failure (any error without a more specific code).
pub const EXIT_ERROR: u8 = 1;
/// A usage error (no/unknown subcommand, bad flags).
pub const EXIT_USAGE: u8 = 2;
/// Diagnostics ran to completion and found problems (`totsuka doctor`).
/// Distinct from [`EXIT_ERROR`] so scripts can tell "doctor itself failed"
/// from "doctor worked and the environment has issues".
pub const EXIT_PROBLEMS_FOUND: u8 = 3;

/// A failure that maps to a specific process exit code. `main` downcasts the
/// returned [`CliError`] to this; any other error exits [`EXIT_ERROR`]. The
/// message keeps the "cause → next action" convention (§7) like every other
/// CLI error.
#[derive(Debug)]
pub struct ExitWith {
    /// The process exit code to return.
    pub code: u8,
    /// The "cause → next action" message.
    pub message: String,
}

impl ExitWith {
    /// A failure exiting with `code` and the conventional message.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExitWith {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitWith {}

/// The shared `--json` flag (machine-readable contract: parseable output on
/// stdout, nothing else). Flattened into every command that supports JSON
/// output so the flag is declared once.
#[derive(Debug, Default, clap::Args)]
pub struct JsonFlag {
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

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
