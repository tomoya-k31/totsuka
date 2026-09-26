//! Binary entrypoint: the SDK stdio runtime over [`Server`], wrapped in the
//! SDK's `AgentIdeServer` (F-38/F-51, #759).
//!
//! Protocol traffic is NDJSON on stdout; diagnostics go to stderr. Each request
//! shells out to the `orca` CLI (F-30).

use agent_ide_orca::cli::ProcessCli;
use agent_ide_orca::config::OrcaConfig;
use agent_ide_orca::server::{CliFactory, Server};
use plugin_sdk::AgentIdeServer;

/// Production factory: builds a CLI driver for the configured `orca` binary.
struct ProcessFactory;

impl CliFactory for ProcessFactory {
    type Cli = ProcessCli;
    fn build(&self, config: &OrcaConfig) -> ProcessCli {
        ProcessCli::new(
            config.orca_bin.clone(),
            std::time::Duration::from_secs(config.request_timeout_secs),
        )
    }
}

#[tokio::main]
async fn main() {
    // Logs go to stderr so they never corrupt the stdout NDJSON channel.
    plugin_sdk::runtime::init_tracing();

    // The SDK's single writer task owns stdout; replies and the streamed
    // `state/notification`s both go through it, so they never interleave
    // mid-line.
    let stdio = plugin_sdk::runtime::stdio();
    let server = AgentIdeServer::new(Server::new(ProcessFactory), stdio.writer.clone());
    plugin_sdk::runtime::serve(server, &stdio).await;
}
