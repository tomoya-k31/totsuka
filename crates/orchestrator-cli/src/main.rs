//! totsuka CLI entrypoint.
//!
//! For #52 this wires the `plugin` subcommand (install / uninstall / enable /
//! disable / list). The remaining command tree (`run`, `status`, `config`,
//! `doctor`, ...) is added in #63/#64.

mod plugin_cmd;

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
    /// Manage plugins (install / uninstall / enable / disable / list).
    Plugin {
        #[command(subcommand)]
        cmd: plugin_cmd::PluginCommand,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Plugin { cmd }) => plugin_cmd::run(cmd),
        None => {
            // No subcommand: print help hint (the full run loop is #63).
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
