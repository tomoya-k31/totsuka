//! Binary entrypoint: an NDJSON stdio loop over [`Server`], with a dedicated
//! writer task so streamed `state/notification`s and request responses share
//! stdout safely (F-38/F-51).
//!
//! Protocol traffic is NDJSON on stdout; diagnostics go to stderr. Each request
//! shells out to the `orca` CLI (F-30).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use agent_ide_orca::cli::ProcessCli;
use agent_ide_orca::config::OrcaConfig;
use agent_ide_orca::server::{CliFactory, Server};

/// Production factory: builds a CLI driver for the configured `orca` binary.
struct ProcessFactory;

impl CliFactory for ProcessFactory {
    type Cli = ProcessCli;
    fn build(&self, config: &OrcaConfig) -> ProcessCli {
        ProcessCli::new(config.orca_bin.clone())
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

    let mut server = Server::new(ProcessFactory, out_tx.clone());
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
