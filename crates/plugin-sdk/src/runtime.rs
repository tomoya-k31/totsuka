//! The stdio NDJSON runtime: one reader loop, one **shared writer task**.
//!
//! Plugins historically wrote replies inline from the read loop, which is
//! line-safe only while nothing else writes. A push source also emits
//! `task/submit` requests from background tasks, so every write must go
//! through one channel — the writer task is the single owner of stdout and
//! each message is exactly one atomic line.
//!
//! The reader additionally routes *responses* (`id` + `result`/`error`, no
//! `method`) to the [`SubmitClient`] so a plugin's own requests get
//! answered; everything else goes to the [`LineHandler`].

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::dispatch::Reply;
use crate::lookup::LookupClient;
use crate::submit::SubmitClient;

/// A clonable handle onto the shared writer task; each `send` is one NDJSON
/// line on stdout. Send failures mean the host is gone — callers treat them
/// as shutdown, not errors.
#[derive(Debug, Clone)]
pub struct Writer {
    tx: mpsc::UnboundedSender<String>,
}

impl Writer {
    /// A writer over an arbitrary channel — for tests and custom transports;
    /// production plugins get theirs from [`stdio`].
    pub fn from_channel(tx: mpsc::UnboundedSender<String>) -> Self {
        Self { tx }
    }

    /// Enqueue one line (without trailing newline).
    pub fn send_line(&self, line: String) -> bool {
        self.tx.send(line).is_ok()
    }
}

/// The assembled stdio runtime: the writer handle plus the submit client
/// wired to it. Build once in `main`, hand [`SubmitClient`] clones to the
/// pipeline/poll tasks, then call [`serve`].
pub struct Stdio {
    /// The shared writer.
    pub writer: Writer,
    /// The `task/submit` client bound to this writer.
    pub submit: SubmitClient,
    /// The `task/lookup` client bound to this writer (0.2.4, #242).
    pub lookup: LookupClient,
}

/// Spawn the stdout writer task and build the runtime handles.
pub fn stdio() -> Stdio {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break; // stdout closed: the host is gone
            }
        }
    });
    let writer = Writer { tx };
    let submit = SubmitClient::new(writer.clone());
    let lookup = LookupClient::new(writer.clone());
    Stdio {
        writer,
        submit,
        lookup,
    }
}

/// One line of the host-driven protocol, answered with a [`Reply`].
pub trait LineHandler: Send {
    /// Handle one NDJSON line (request or notification).
    fn handle_line(&mut self, line: &str) -> impl Future<Output = Reply> + Send;
}

/// Run the read loop until EOF or a `shutdown` reply.
///
/// Responses to this plugin's own requests (`id` present, no `method`) are
/// resolved against the request clients (`stdio.submit` / `stdio.lookup`);
/// every other line goes to `handler` and its reply is written through the
/// shared writer.
pub async fn serve<H: LineHandler>(mut handler: H, stdio: &Stdio) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // stdin closed (EOF): the host is gone
            // A malformed (e.g. non-UTF-8) line must not kill the session.
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable stdin line");
                continue;
            }
        };
        // A response to one of our own requests? (`id` + result/error, no
        // `method`.) Route it to the submit client instead of the handler.
        if let Ok(value) = serde_json::from_str::<Value>(line.trim())
            && value.get("method").is_none()
            && value.get("id").is_some()
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            // Both clients see every response and ignore ids they did not
            // issue; the id prefixes (`submit-` / `lookup-`) keep them apart.
            stdio.submit.resolve(&value);
            stdio.lookup.resolve(&value);
            continue;
        }
        let reply = handler.handle_line(&line).await;
        if let Some(out) = reply.line
            && !stdio.writer.send_line(out)
        {
            break; // writer gone: the host is gone
        }
        if reply.shutdown {
            break;
        }
    }
}
