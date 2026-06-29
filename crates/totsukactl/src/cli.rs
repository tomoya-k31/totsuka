use crate::error::TotsukactlError;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "totsukactl", version, about = "Supervisor + CLI for the totsuka stack")]
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
    let _ = cli;
    Err(TotsukactlError::Internal(
        "cli dispatch wiring lands in Tasks 19-25".into(),
    ))
}
