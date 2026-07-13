//! totsuka CLI entrypoint.
//!
//! Wires the `plugin` subcommand (#52) and the `run` main loop (#63). The
//! remaining command tree (`status`, `task`, `config`, `doctor`, ...) is added
//! in #64.

mod plugin_cmd;
mod run_cmd;

use clap::{Parser, Subcommand};

/// totsuka — local AI-agent orchestrator.
#[derive(Debug, Parser)]
#[command(name = "totsuka", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the main loop: fetch → dispatch → monitor (§5.1).
    Run {
        /// Keep polling task sources at their configured intervals (F-06)
        /// instead of exiting after one cycle.
        #[arg(long)]
        watch: bool,
        /// Show which tasks would match, which repository would be selected,
        /// and which agent would run — without executing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage plugins (install / uninstall / enable / disable / list).
    Plugin {
        #[command(subcommand)]
        cmd: plugin_cmd::PluginCommand,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Run { watch, dry_run }) => run_cmd::run(watch, dry_run),
        Some(Command::Plugin { cmd }) => plugin_cmd::run(cmd),
        None => {
            eprintln!("totsuka: no command given. Try `totsuka --help`.");
            return std::process::ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
