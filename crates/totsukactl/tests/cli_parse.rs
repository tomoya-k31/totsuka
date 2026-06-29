use clap::Parser;
use totsukactl::cli::{Cli, Cmd};

#[test]
fn up_with_flags() {
    let c = Cli::parse_from(["totsukactl", "up", "--recreate", "--bootstrap"]);
    assert_eq!(c.command, Cmd::Up { recreate: true, bootstrap: true });
}

#[test]
fn down_force_and_postgres() {
    let c = Cli::parse_from(["totsukactl", "down", "--force", "--postgres"]);
    assert_eq!(c.command, Cmd::Down { force: true, postgres: true });
}

#[test]
fn logs_default_lines_100() {
    let c = Cli::parse_from(["totsukactl", "logs", "orchestrator"]);
    assert_eq!(c.command, Cmd::Logs { bin: "orchestrator".into(), follow: false, lines: 100 });
}

#[test]
fn restart_and_reload_take_bin_name() {
    let c = Cli::parse_from(["totsukactl", "restart", "agent-adapter"]);
    assert_eq!(c.command, Cmd::Restart { bin: "agent-adapter".into() });
    let c = Cli::parse_from(["totsukactl", "reload", "agent-adapter"]);
    assert_eq!(c.command, Cmd::Reload { bin: "agent-adapter".into() });
}
