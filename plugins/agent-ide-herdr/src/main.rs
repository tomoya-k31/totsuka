//! Binary entrypoint: an NDJSON stdio loop over [`Server`], with a dedicated
//! writer task so streamed `state/notification`s and request responses share
//! stdout safely (F-38/F-51).
//!
//! Protocol traffic is NDJSON on stdout; diagnostics go to stderr. The plugin
//! connects to herdr's Unix socket lazily at `initialize` (F-30).

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use agent_ide_herdr::error::HerdrError;
use agent_ide_herdr::server::{Server, TransportFactory};
use agent_ide_herdr::transport::SocketTransport;

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
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    // Single writer task owns stdout; the server and its stream tasks enqueue
    // lines here so responses and notifications never interleave mid-line.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break; // stdout closed: the host is gone
            }
        }
    });

    let mut server = Server::new(SocketFactory, out_tx.clone());
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin closed (EOF): the host is gone
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable stdin line");
                continue;
            }
        };
        if !server.handle_line(&line).await {
            break; // shutdown
        }
    }

    // Drop the server (and its out_tx clone) so the writer drains and exits.
    drop(server);
    drop(out_tx);
    let _ = writer.await;
}
