//! totsuka CLI entrypoint (§5.1 command tree).
//!
//! `run` (#63) plus the full command surface (#64): init / status / task /
//! plugin / config / logs / doctor / completion, with the shared flags
//! `--config` (highest config layer, F-66) and `--debug`.

mod common;
mod config_cmd;
mod doctor_cmd;
mod focus_cmd;
mod hooks;
mod init_cmd;
mod logs_cmd;
mod plugin_cmd;
mod run_cmd;
mod status_cmd;
mod task_cmd;

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use common::Cx;

/// totsuka — local AI-agent orchestrator.
#[derive(Debug, Parser)]
#[command(name = "totsuka", version, about, long_about = None)]
struct Cli {
    /// Path to config.toml (overrides $XDG_CONFIG_HOME/totsuka/config.toml).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Verbose diagnostics (debug-level logging where applicable).
    #[arg(long, global = true)]
    debug: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a config skeleton and check the environment.
    Init,
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
    /// Show tasks, worktrees, and whether the orchestrator is running.
    Status {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Operate on individual tasks (list / show / cancel / retry).
    Task {
        #[command(subcommand)]
        cmd: task_cmd::TaskCommand,
    },
    /// Bring a task's agent pane to the foreground (the notification's
    /// click target, F-94). Quietly no-ops when the orchestrator is stopped.
    Focus {
        /// Task id (see `totsuka task list`).
        id: i64,
    },
    /// Manage plugins (install / uninstall / enable / disable / list).
    Plugin {
        #[command(subcommand)]
        cmd: plugin_cmd::PluginCommand,
    },
    /// Validate or display the configuration.
    Config {
        #[command(subcommand)]
        cmd: config_cmd::ConfigCommand,
    },
    /// View the orchestrator logs.
    Logs {
        /// Keep following appended log lines.
        #[arg(short = 'f', long)]
        follow: bool,
        /// Only show lines for one task id.
        #[arg(long, value_name = "ID")]
        task: Option<i64>,
    },
    /// Diagnose the environment (git, config, plugins, orphan worktrees).
    Doctor {
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completions (zsh, bash, fish, ...).
    Completion {
        /// Target shell.
        shell: clap_complete::Shell,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // No subcommand is a usage error: exit code 2 (clap's convention, which the
    // rest of the CLI shares) so scripts can tell it apart from a runtime
    // failure (exit 1). A single message, no double-print.
    let Some(command) = cli.command else {
        eprintln!("totsuka: no command given. Try `totsuka --help`.");
        return std::process::ExitCode::from(2);
    };
    match execute(command, cli.config.as_deref(), cli.debug) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn execute(
    command: Command,
    config: Option<&std::path::Path>,
    debug: bool,
) -> Result<(), common::CliError> {
    // Completion needs no environment at all.
    if let Command::Completion { shell } = command {
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "totsuka",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    let cx = Cx::resolve(config)?;
    match command {
        Command::Completion { .. } => unreachable!("handled above"),
        Command::Init => init_cmd::run(&cx),
        Command::Run { watch, dry_run } => run_cmd::run(&cx, watch, dry_run, debug),
        Command::Status { json } => status_cmd::run(&cx, json),
        Command::Task { cmd } => task_cmd::run(&cx, cmd),
        Command::Focus { id } => focus_cmd::run(&cx, id),
        Command::Plugin { cmd } => plugin_cmd::run(cmd),
        Command::Config { cmd } => config_cmd::run(&cx, cmd),
        Command::Logs { follow, task } => logs_cmd::run(&cx, follow, task),
        Command::Doctor { json } => doctor_cmd::run(&cx, json),
    }
}
