//! totsuka CLI entrypoint.
//!
//! Minimal skeleton for #45: a `clap`-parsed root command that supports
//! `--version` and `--help`. Subcommands are added by later tasks (§5.1 of the
//! spec).

use clap::Parser;

/// totsuka — local AI-agent orchestrator.
#[derive(Debug, Parser)]
#[command(name = "totsuka", version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
