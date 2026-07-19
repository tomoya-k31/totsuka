//! Binary entrypoint: the SDK stdio runtime over [`Server`].
//!
//! Protocol traffic is NDJSON on stdout through the SDK's **single writer
//! task**, so host replies and the pipeline's background `task/submit`
//! requests (0.1.6) never interleave partial lines. All diagnostics go to
//! stderr (the host forwards stderr to its log). The plugin never touches
//! the Keychain — its tokens arrive already resolved in `initialize` config
//! (F-65).

use task_source_slack::llm::ReqwestChat;
use task_source_slack::server::{Server, TransportFactory};
use task_source_slack::transport::{ReqwestTransport, TransportSettings};

/// Production factory: builds real reqwest-backed transports.
struct ReqwestFactory;

impl TransportFactory for ReqwestFactory {
    type Transport = ReqwestTransport;
    type Chat = ReqwestChat;
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport {
        ReqwestTransport::new(settings)
    }
    fn build_chat(&self) -> Self::Chat {
        ReqwestChat::new()
    }
}

#[tokio::main]
async fn main() {
    // Logs go to stderr so they never corrupt the stdout JSON-RPC channel.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let stdio = plugin_sdk::runtime::stdio();
    let server = Server::new(ReqwestFactory, stdio.submit.clone());
    plugin_sdk::runtime::serve(server, &stdio).await;
}
