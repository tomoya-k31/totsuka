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
use tokio::sync::{mpsc, oneshot};

use crate::dispatch::Reply;
use crate::lookup::LookupClient;
use crate::submit::SubmitClient;

/// Install the plugin's tracing subscriber on **stderr** (stdout is the
/// JSON-RPC channel).
///
/// Two shapes, chosen by who is reading (ADR-0096):
///
/// - **Under the host (stderr is a pipe): JSON Lines at every level.** The
///   host parses each line back into a record and re-emits it at the
///   plugin's own level and target, with its fields intact, then filters it
///   against `[log] level`. The host is the **only** filter: a threshold
///   here would be a second, independently configured one, and that is how
///   a plugin's `WARN` used to vanish under a host at `warn` — it arrived
///   relabelled `INFO`. So `RUST_LOG` is deliberately not read here.
/// - **By hand (stderr is a terminal): human-readable, `RUST_LOG` honoured**
///   (default `info`), coloured.
///
/// Call this once, first thing in `main`. A second call is a no-op rather
/// than a panic: `init()` aborts the process when a global subscriber already
/// exists, and now that this lives in a library — reachable from tests and
/// from anything embedding a plugin — that failure would be a crash far from
/// its cause. Nothing can be logged about it either, since the only way to
/// reach this arm is that a subscriber is already installed and doing the job.
pub fn init_tracing() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::LevelFilter;

    if std::io::stderr().is_terminal() {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            // The builder installs no `EnvFilter` of its own — without this
            // `RUST_LOG` is silently ignored and every `debug!` unreachable.
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .json()
            // Fields at the top level next to `message`, which is the shape
            // the host reads; no span keys, since no plugin opens spans.
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_max_level(LevelFilter::TRACE)
            .try_init();
    }
}

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
    /// Asks the writer task to confirm everything queued so far is on stdout.
    flush: mpsc::UnboundedSender<oneshot::Sender<()>>,
}

impl Stdio {
    /// Wait until every line queued before this call has been written.
    ///
    /// [`serve`] calls this before returning: `main` ends right after it, and
    /// the runtime drops the writer task with whatever it had not written yet
    /// — the `shutdown` reply included, which the conformance kit found lost
    /// in most runs (#767).
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        if self.flush.send(ack).is_ok() {
            let _ = done.await;
        }
    }
}

/// Spawn the stdout writer task and build the runtime handles.
pub fn stdio() -> Stdio {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (flush, mut flushes) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        loop {
            // A flush answers once every line queued *before* it is out. Those
            // are exactly the `rx.len()` lines waiting when it is picked —
            // draining only those, and not biasing the select towards lines,
            // keeps a producer that never stops (a state stream) from holding
            // the flush, and with it the process exit, forever.
            let written = tokio::select! {
                Some(line) = rx.recv() => write_line(&mut stdout, &line).await,
                Some(ack) = flushes.recv() => {
                    let mut written = true;
                    for _ in 0..rx.len() {
                        let Ok(line) = rx.try_recv() else { break };
                        written = write_line(&mut stdout, &line).await;
                        if !written {
                            break;
                        }
                    }
                    let _ = ack.send(());
                    written
                }
                else => break,
            };
            if !written {
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
        flush,
    }
}

async fn write_line(stdout: &mut tokio::io::Stdout, line: &str) -> bool {
    stdout.write_all(line.as_bytes()).await.is_ok()
        && stdout.write_all(b"\n").await.is_ok()
        && stdout.flush().await.is_ok()
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
    stdio.flush().await;
}
