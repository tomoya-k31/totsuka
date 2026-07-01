use crate::error::TotsukactlError;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "totsukactl",
    version,
    about = "Supervisor + CLI for the totsuka stack"
)]
pub struct Cli {
    /// Path to totsuka.toml (defaults to $TOTSUKA_CONFIG or ~/.config/totsuka/config.toml)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Start the stack (postgres → preflight → adapter → orchestrator → watcher∥qa)
    Up {
        #[arg(long)]
        recreate: bool,
        #[arg(long)]
        bootstrap: bool,
    },
    /// Graceful shutdown (reverse dep order, 15s grace)
    Down {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        postgres: bool,
    },
    /// Print process registry as a formatted table
    Status,
    /// Apply sqlx migrations
    Migrate,
    /// First-run bootstrap (write config.toml + secrets.toml, compose up, migrate)
    Init,
    /// Restart a single bin (respects dependency order)
    Restart { bin: String },
    /// Send SIGHUP to a single bin (only agent-adapter is meaningful)
    Reload { bin: String },
    /// Tail a child's log file
    Logs {
        bin: String,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: u32,
    },
}

pub fn parse() -> Cli {
    Cli::parse()
}

pub async fn dispatch(cli: Cli) -> Result<(), TotsukactlError> {
    // Init bootstraps the config from scratch, so it runs before config loading.
    if let Cmd::Init = &cli.command {
        let state = crate::paths::resolve_tilde("~/.local/state/totsuka");
        let data = crate::paths::resolve_tilde("~/.local/share/totsuka");
        let paths = crate::paths::Paths {
            log_dir: state.join("logs"),
            pid_dir: state.join("pids"),
            sock_dir: state.join("sock"),
            state_dir: state,
            data_dir: data,
        };
        return crate::commands::init::run(&paths).await;
    }

    let config_path = cli
        .config
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| std::env::var("TOTSUKA_CONFIG").ok())
        .unwrap_or_else(|| "~/.config/totsuka/config.toml".into());
    let cfg = totsuka_config::Config::load(crate::paths::resolve_tilde(&config_path))
        .map_err(|e| TotsukactlError::Config(format!("{e:?}")))?;
    let paths = crate::paths::Paths::from_config(&cfg);
    let clock: std::sync::Arc<dyn totsuka_core::Clock> =
        std::sync::Arc::new(totsuka_core::SystemClock);

    match cli.command {
        Cmd::Down { force, postgres } => {
            let wait_budget = crate::commands::down::shutdown_wait_budget(
                cfg.supervisor.shutdown_grace_secs,
                cfg.supervisor.shutdown_kill_secs,
            );
            crate::commands::down::run(&paths, force, postgres, wait_budget).await
        }
        Cmd::Restart { bin } => crate::commands::restart::run(&paths, &bin).await,
        Cmd::Reload { bin } => crate::commands::reload::run(&paths, &bin).await,
        Cmd::Status => crate::commands::status::run(&paths, clock.as_ref()).await,
        Cmd::Up {
            recreate,
            bootstrap,
        } => crate::commands::up::run(cfg, paths, recreate, bootstrap).await,
        Cmd::Logs { bin, follow, lines } => {
            crate::commands::logs::run(&paths, &bin, lines, follow).await
        }
        Cmd::Migrate => crate::commands::migrate::run(&cfg).await,
        Cmd::Init => unreachable!("Init is handled before config loading"),
    }
}
