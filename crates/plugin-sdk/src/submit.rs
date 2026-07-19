//! [`SubmitClient`]: push one task to the Orchestrator via `task/submit`
//! (protocol 0.1.6) and await the persist-before-ack outcome.
//!
//! Retry contract (ADR-0008): the three result statuses
//! (`accepted`/`duplicate`/`rejected`) are **final** — never re-submitted.
//! JSON-RPC errors `NOT_ACCEPTING(-32004)` / `SUBMIT_OVERLOADED(-32005)` /
//! `INTERNAL_ERROR(-32603)`, a lost writer, and an ack timeout are retried
//! with exponential backoff; a re-submit after a lost ack is answered
//! `duplicate` by the Orchestrator, so retrying is always safe.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_protocol::jsonrpc::{Request, error_code, to_line};
use plugin_protocol::methods::{TaskSubmitParams, TaskSubmitResult, TaskSubmitStatus};
use plugin_protocol::{RequestId, Task, method};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::runtime::Writer;

/// Per-attempt ack timeout. Generous because the engine loop may be busy
/// (worktree creation blocks it); a timed-out attempt is retried and a late
/// ack for it is dropped harmlessly.
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// First backoff step; doubles per retry up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);

/// Backoff ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Attempts before giving up ([`SubmitOutcome::GaveUp`]). The source system
/// (Slack/GitHub/Notion) remains the durable origin, so giving up loses
/// nothing permanently — the task is re-derivable.
const MAX_ATTEMPTS: u32 = 5;

/// The final outcome of [`SubmitClient::submit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Persisted and queued; the plugin may forget the task.
    Accepted,
    /// Already ingested (idempotent re-submit); drop it.
    Duplicate,
    /// Permanently unprocessable; drop and log the reason.
    Rejected {
        /// Cause + next action from the Orchestrator.
        reason: Option<String>,
    },
    /// All retries exhausted (transport gone or persistent errors). The
    /// source system remains the durable origin.
    GaveUp {
        /// The last error observed.
        error: String,
    },
}

/// Anything that can submit a task — the seam [`poll_loop`](crate::poll)
/// tests mock.
pub trait Submitter: Send + Sync {
    /// Submit one task and await its final outcome.
    fn submit(&self, task: Task) -> impl Future<Output = SubmitOutcome> + Send;
}

/// One pending ack slot: resolved with the typed result, or a retryable
/// error description.
type PendingAck = oneshot::Sender<Result<TaskSubmitResult, String>>;

/// The `task/submit` client bound to the shared [`Writer`].
///
/// Clonable; all clones share one pending-ack map, which
/// [`serve`](crate::runtime::serve) resolves from response lines.
#[derive(Debug, Clone)]
pub struct SubmitClient {
    writer: Writer,
    pending: Arc<Mutex<HashMap<String, PendingAck>>>,
    next_id: Arc<AtomicU64>,
    ack_timeout: Duration,
    first_backoff: Duration,
}

impl SubmitClient {
    /// Build a client over `writer` with production timeouts.
    pub fn new(writer: Writer) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(0)),
            ack_timeout: ACK_TIMEOUT,
            first_backoff: FIRST_BACKOFF,
        }
    }

    /// Override the timeouts (tests).
    pub fn with_timeouts(mut self, ack_timeout: Duration, first_backoff: Duration) -> Self {
        self.ack_timeout = ack_timeout;
        self.first_backoff = first_backoff;
        self
    }

    /// Submit `task`, retrying retryable failures, until a final outcome.
    pub async fn submit_task(&self, task: Task) -> SubmitOutcome {
        let mut backoff = self.first_backoff;
        let mut last_error = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            match self.submit_once(&task).await {
                Ok(result) => {
                    return match result.status {
                        TaskSubmitStatus::Accepted => SubmitOutcome::Accepted,
                        TaskSubmitStatus::Duplicate => SubmitOutcome::Duplicate,
                        TaskSubmitStatus::Rejected => SubmitOutcome::Rejected {
                            reason: result.reason,
                        },
                    };
                }
                Err(error) => {
                    tracing::warn!(
                        task = %task.id,
                        attempt,
                        "task/submit attempt failed (will retry): {error}"
                    );
                    last_error = error;
                }
            }
            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
        tracing::error!(
            task = %task.id,
            "task/submit gave up after {MAX_ATTEMPTS} attempts: {last_error} → \
             the source system remains the durable origin"
        );
        SubmitOutcome::GaveUp { error: last_error }
    }

    /// One attempt: send the request, await its ack (or time out). `Err` is
    /// always retryable — final statuses come back as `Ok`.
    async fn submit_once(&self, task: &Task) -> Result<TaskSubmitResult, String> {
        let id = format!("submit-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let params = TaskSubmitParams { task: task.clone() };
        let request = Request::new(
            RequestId::Str(id.clone()),
            method::TASK_SUBMIT,
            Some(serde_json::to_value(&params).map_err(|e| e.to_string())?),
        );
        let line = to_line(&request).map_err(|e| e.to_string())?;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), tx);
        if !self.writer.send_line(line) {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err("writer closed (host gone)".to_string());
        }
        match tokio::time::timeout(self.ack_timeout, rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            // Retryable JSON-RPC error (the resolver pre-filters final ones —
            // there are none: every error code is retryable by contract).
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err("ack channel dropped".to_string()),
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(format!("no ack within {:?}", self.ack_timeout))
            }
        }
    }

    /// Resolve one response line against the pending map (called by
    /// [`serve`](crate::runtime::serve)). Unknown ids — e.g. a late ack for
    /// a timed-out attempt — are dropped harmlessly.
    pub fn resolve(&self, response: &Value) {
        let Some(id) = response.get("id") else { return };
        let key = match id.as_str() {
            Some(s) => s.to_string(),
            None => id.to_string(),
        };
        let Some(tx) = self.pending.lock().expect("pending lock").remove(&key) else {
            return;
        };
        let outcome = if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            Err(format!("orchestrator answered {code}: {message}"))
        } else {
            match serde_json::from_value::<TaskSubmitResult>(
                response.get("result").cloned().unwrap_or(Value::Null),
            ) {
                Ok(result) => Ok(result),
                Err(e) => Err(format!("malformed task/submit ack: {e}")),
            }
        };
        let _ = tx.send(outcome);
    }
}

impl Submitter for SubmitClient {
    async fn submit(&self, task: Task) -> SubmitOutcome {
        self.submit_task(task).await
    }
}

/// The retryable error codes, re-exported for reference: every JSON-RPC
/// error on `task/submit` means "retry with backoff" —
/// [`NOT_ACCEPTING`](error_code::NOT_ACCEPTING),
/// [`SUBMIT_OVERLOADED`](error_code::SUBMIT_OVERLOADED),
/// [`INTERNAL_ERROR`](error_code::INTERNAL_ERROR) — while final dispositions
/// ride the result. See [`TaskSubmitStatus`].
pub const RETRYABLE_CODES: [i64; 3] = [
    error_code::NOT_ACCEPTING,
    error_code::SUBMIT_OVERLOADED,
    error_code::INTERNAL_ERROR,
];
