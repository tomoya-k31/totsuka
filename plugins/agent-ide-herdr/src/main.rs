//! Binary entrypoint: the SDK stdio runtime over [`Server`], wrapped in the
//! SDK's `AgentIdeServer` (F-38/F-51, #759).
//!
//! Protocol traffic is NDJSON on stdout; diagnostics go to stderr. The plugin
//! connects to herdr's Unix socket lazily at `initialize` (F-30).

use std::path::Path;
use std::time::Duration;

use agent_ide_herdr::error::HerdrError;
use agent_ide_herdr::server::{Server, TransportFactory};
use agent_ide_herdr::transport::SocketTransport;
use plugin_sdk::AgentIdeServer;

/// Production factory: connects real herdr sockets.
struct SocketFactory;

impl TransportFactory for SocketFactory {
    type Transport = SocketTransport;
    async fn build(&self, path: &Path, timeout: Duration) -> Result<SocketTransport, HerdrError> {
        SocketTransport::connect(path, timeout).await
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
    let server = AgentIdeServer::new(Server::new(SocketFactory), stdio.writer.clone());
    plugin_sdk::runtime::serve(server, &stdio).await;
}
