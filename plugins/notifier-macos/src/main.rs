//! Binary entrypoint: a thin NDJSON stdio loop over [`Server`].
//!
//! Protocol traffic is NDJSON on stdout; diagnostics go to stderr. `notify` is
//! fire-and-forget (F-93): its delivery is spawned by the server, so the loop
//! keeps reading regardless of send latency or failure.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use notifier_macos::config::NotifierConfig;
use notifier_macos::sender::BackendSender;
use notifier_macos::server::{SenderFactory, Server};

/// Production factory: builds the backend `[notifier].backend` selects —
/// `osascript` (default) or `terminal-notifier` (click-to-focus, F-94).
struct BackendFactory;

impl SenderFactory for BackendFactory {
    type Sender = BackendSender;
    fn build(&self, config: &NotifierConfig) -> BackendSender {
        BackendSender::from_config(config)
    }
}

#[tokio::main]
async fn main() {
    // Logs go to stderr so they never corrupt the stdout NDJSON channel.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let mut server = Server::new(BackendFactory);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin closed (EOF): the host is gone
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
