//! totsuka CLI entrypoint (§5.1 command tree).
//!
//! `run` (#63) plus the full command surface (#64): init / status / task /
//! plugin / config / logs / doctor / completion, with the shared flags
//! `--config` (highest config layer, F-66) and `--debug`.

mod bundled;
mod common;
mod config_cmd;
mod doctor_cmd;
mod focus_cmd;
mod from_source;
mod init_cmd;
mod logs_cmd;
mod plugin_cmd;
mod run_cmd;
mod setup;
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
    /// Verbose diagnostics: debug-level logging on stderr for every command
    /// (`run` additionally raises its file log level).
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
    /// Fill the config in interactively: pick a starting recipe, answer a few
    /// questions, review the plan. Never handles secret values.
    Setup {
        /// Read answers from a file instead of asking (testing affordance).
        #[arg(long, hide = true)]
        answers: Option<PathBuf>,
        /// Write the collected answers to a file.
        #[arg(long)]
        save_answers: Option<PathBuf>,
        /// Show the plan and stop without writing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Override where bundled plugins are looked up, and never fall back
        /// to building from a checkout. Testing affordance, mirroring
        /// `plugin install --bundled-dir`.
        #[arg(long, hide = true)]
        bundled_dir: Option<PathBuf>,
    },
    /// Run the main loop: fetch → dispatch → monitor (§5.1).
    Run {
        /// Keep polling task sources at their configured intervals (F-06)
        /// instead of exiting after one cycle.
        #[arg(long)]
        watch: bool,
        /// Report what would happen without executing anything.
        ///
        /// **A no-op since protocol 0.2.0**: every task source pushes
        /// (`task/submit`) rather than being fetched on demand, so there is
        /// nothing to preview ahead of time and the only output is the
        /// sentence saying so. Kept because the flag is a documented part of
        /// the command surface and its zero-side-effect guarantee still holds.
        ///
        /// Refused together with `--json` for the same reason: a JSON envelope
        /// around an empty preview would promise machine-readable output that
        /// does not exist.
        #[arg(long, conflicts_with = "json")]
        dry_run: bool,
        /// One-shot's quiet-period floor in milliseconds (default 2000) before
        /// an empty run is allowed to exit.
        ///
        /// Hidden: it exists so the E2E suite does not pay 2s of pure waiting
        /// per `totsuka run` (#281). Lowering it in anger only makes a run give
        /// up on a source that is still mid-handshake.
        #[arg(long, hide = true, value_name = "MS")]
        one_shot_grace_ms: Option<u64>,
        #[command(flatten)]
        json: common::JsonFlag,
    },
    /// Show tasks, worktrees, and whether the orchestrator is running.
    Status {
        #[command(flatten)]
        json: common::JsonFlag,
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
        /// Additionally verify the configured `[llm]` gateway accepts the
        /// API key, with one minimal live request. Opt-in: it is the only
        /// check that goes to the network, and it resolves the key
        /// reference, so an `op://` reference may raise a biometric prompt.
        #[arg(long)]
        online: bool,
        /// Inspect only: skip the writes `doctor` normally performs (hook
        /// assets, $CODEX_HOME/hooks.json, opencode assets, the spool
        /// directory) and do not offer to clean up orphans. For read-only
        /// audits and CI.
        #[arg(long)]
        no_repair: bool,
        #[command(flatten)]
        json: common::JsonFlag,
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
        return std::process::ExitCode::from(common::EXIT_USAGE);
    };
    let json = wants_json(&command);
    match execute(command, cli.config.as_deref(), cli.debug) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            emit_error(&err, json);
            let code = err
                .downcast_ref::<common::ExitWith>()
                .map(|e| e.code)
                .unwrap_or(common::EXIT_ERROR);
            std::process::ExitCode::from(code)
        }
    }
}

/// Whether this invocation asked for machine-readable output: its error (if
/// any) is then emitted as a JSON envelope on stderr instead of plain text.
fn wants_json(command: &Command) -> bool {
    match command {
        Command::Status { json } | Command::Doctor { json, .. } | Command::Run { json, .. } => {
            json.json
        }
        Command::Task { cmd } => cmd.wants_json(),
        Command::Plugin { cmd } => cmd.wants_json(),
        _ => false,
    }
}

/// Print one error to stderr. Plain text keeps the human-facing
/// `error: <cause> → <next action>` line (§7); `--json` invocations instead
/// get a one-line JSON envelope `{"error":{"message":...,"action":...}}` so
/// callers can parse failures the same way they parse stdout (#177). The
/// first ` → ` splits cause from action; without one, `action` is null.
fn emit_error(err: &common::CliError, json: bool) {
    if json {
        let text = err.to_string();
        let (message, action) = match text.split_once(" → ") {
            Some((cause, action)) => (cause.to_string(), Some(action.to_string())),
            None => (text, None),
        };
        eprintln!(
            "{}",
            serde_json::json!({"error": {"message": message, "action": action}})
        );
    } else {
        eprintln!("error: {err}");
    }
}

fn execute(
    command: Command,
    config: Option<&std::path::Path>,
    debug: bool,
) -> Result<(), common::CliError> {
    // `--debug` on any command but `run`: a stderr-only debug subscriber, no
    // log files (#176). Installed before the completion early-return so the
    // flag has an effect on *every* command, as its help promises. `run` owns
    // the file logging and applies `--debug` on top of its config-driven level
    // itself — installing a subscriber here would make its own `logging::init`
    // fail (the global subscriber can only be set once). Prompt/payload
    // fields stay off: `[logging].log_prompts` governs `run`'s file logs and
    // is not loaded yet here, so the ad-hoc terminal stream defaults closed
    // rather than overriding a `log_prompts = false` config.
    if debug && !matches!(command, Command::Run { .. }) {
        orchestrator_core::logging::init_stderr(tracing::Level::DEBUG, false)?;
        tracing::debug!("--debug: verbose diagnostics enabled");
    }

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
        Command::Setup {
            answers,
            save_answers,
            dry_run,
            yes,
            bundled_dir,
        } => setup::run(
            &cx,
            &setup::SetupArgs {
                answers,
                save_answers,
                dry_run,
                yes,
                bundled_dir,
            },
        ),
        Command::Run {
            watch,
            dry_run,
            one_shot_grace_ms,
            json,
        } => run_cmd::run(
            &cx,
            run_cmd::RunArgs {
                watch,
                dry_run,
                debug,
                one_shot_grace_ms,
                json: json.json,
            },
        ),
        Command::Status { json } => status_cmd::run(&cx, json.json),
        Command::Task { cmd } => task_cmd::run(&cx, cmd),
        Command::Focus { id } => focus_cmd::run(&cx, id),
        Command::Plugin { cmd } => plugin_cmd::run(&cx, cmd),
        Command::Config { cmd } => config_cmd::run(&cx, cmd),
        Command::Logs { follow, task } => logs_cmd::run(&cx, follow, task),
        Command::Doctor {
            json,
            online,
            no_repair,
        } => doctor_cmd::run(
            &cx,
            doctor_cmd::DoctorArgs {
                json: json.json,
                online,
                no_repair,
            },
        ),
    }
}
