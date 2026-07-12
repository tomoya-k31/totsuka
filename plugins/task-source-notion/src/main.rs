//! Binary entrypoint: a thin NDJSON stdio loop over [`Server`].
//!
//! Protocol traffic is NDJSON on stdout; all diagnostics go to stderr (the host
//! forwards stderr to its log). The plugin never touches the Keychain — its
//! token arrives already resolved in `initialize` config (F-65).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use task_source_notion::server::{Server, TransportFactory};
use task_source_notion::transport::{ReqwestTransport, TransportSettings};

/// Production factory: builds real reqwest-backed transports.
struct ReqwestFactory;

impl TransportFactory for ReqwestFactory {
    type Transport = ReqwestTransport;
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport {
        ReqwestTransport::new(settings)
    }
}

#[tokio::main]
async fn main() {
    // Logs go to stderr so they never corrupt the stdout JSON-RPC channel.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let mut server = Server::new(ReqwestFactory);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin closed (EOF): the host is gone
            // A malformed (e.g. non-UTF-8) line must not kill the session; skip
            // it and keep serving.
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable stdin line");
                continue;
            }
        };
        let reply = server.handle_line(&line).await;
        if let Some(out) = reply.line
            && (stdout.write_all(out.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err())
        {
            break; // stdout closed: the host is gone
        }
        if reply.shutdown {
            break;
        }
    }
}
