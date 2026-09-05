//! Binary entrypoint: the SDK stdio runtime over [`Server`].
//!
//! Protocol traffic is NDJSON on stdout through the SDK's **single writer
//! task**, so host replies and the Gateway's background `task/submit`
//! requests never interleave partial lines. All diagnostics go to stderr.
//! The plugin never touches the Keychain — its token arrives already resolved
//! in `initialize` config (F-65).

use task_source_discord::server::{Server, TransportFactory};
use task_source_discord::transport::TransportSettings;

/// Production factory: builds the real reqwest-backed transport.
struct ReqwestFactory;

impl TransportFactory for ReqwestFactory {
    type Transport = task_source_discord::http::ReqwestTransport;
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport {
        task_source_discord::http::ReqwestTransport::new(settings)
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
