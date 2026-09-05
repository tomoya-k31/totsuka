//! [`SubmitClient`]: push one task to the Orchestrator via `task/submit`
//! (protocol 0.1.6) and await the persist-before-ack outcome.
//!
//! Retry contract (ADR-0008): the three result statuses
//! (`accepted`/`duplicate`/`rejected`) are **final** — never re-submitted.
//! The [`RETRYABLE_CODES`] JSON-RPC errors and an ack timeout are retried
//! with exponential backoff (a re-submit after a lost ack is answered
//! `duplicate` by the Orchestrator, so retrying is always safe). Any other
//! error code is a protocol violation, and a closed writer means the host is
//! gone — both fail the submission immediately, no pointless backoff.

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
    /// Submit one task, naming the workflow it belongs to, and await its
    /// final outcome (0.6.0, #554).
    fn submit(&self, task: Task, workflow: &str) -> impl Future<Output = SubmitOutcome> + Send;
}

/// Submit a batch under one workflow, logging each outcome the standard way:
/// `duplicate` is the silent steady state, everything else says what happened.
/// Shared by [`poll_loop`](crate::poll_loop) and
/// [`backfill_pass`](crate::watch::backfill_pass) so the two paths cannot
/// drift in how an outcome reads in the logs.
pub async fn submit_all<S: Submitter>(submitter: &S, tasks: Vec<Task>, workflow: &str) {
    for task in tasks {
        let task_id = task.id.clone();
        match submitter.submit(task, workflow).await {
            SubmitOutcome::Accepted => {
                tracing::info!(task = %task_id, workflow, "task submitted");
            }
            // The normal steady state: the task was already ingested earlier.
            SubmitOutcome::Duplicate => {}
            SubmitOutcome::Rejected { reason } => {
                tracing::warn!(
                    task = %task_id,
                    workflow,
                    "task rejected: {}",
                    reason.as_deref().unwrap_or("no reason given")
                );
            }
            SubmitOutcome::GaveUp { error } => {
                tracing::error!(
                    task = %task_id,
                    workflow,
                    "task submission gave up: {error} → a later fetch pass can retry it (the \
                     source remains the durable origin)"
                );
            }
        }
    }
}

/// A failed attempt: whether backing off and re-submitting can help, plus a
/// human-readable description.
#[derive(Debug)]
struct AttemptError {
    retryable: bool,
    message: String,
}

impl AttemptError {
    fn retryable(message: impl std::fmt::Display) -> Self {
        Self {
            retryable: true,
            message: message.to_string(),
        }
    }

    fn permanent(message: impl std::fmt::Display) -> Self {
        Self {
            retryable: false,
            message: message.to_string(),
        }
    }
}

/// One pending ack slot: resolved with the typed result, or a classified
/// attempt error.
type PendingAck = oneshot::Sender<Result<TaskSubmitResult, AttemptError>>;

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

    /// Submit `task` as belonging to `workflow`, retrying retryable
    /// failures, until a final outcome.
    pub async fn submit_task(&self, task: Task, workflow: &str) -> SubmitOutcome {
        let mut backoff = self.first_backoff;
        let mut last_error = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            match self.submit_once(&task, workflow).await {
                Ok(result) => {
                    return match result.status {
                        TaskSubmitStatus::Accepted => SubmitOutcome::Accepted,
                        TaskSubmitStatus::Duplicate => SubmitOutcome::Duplicate,
                        TaskSubmitStatus::Rejected => SubmitOutcome::Rejected {
                            reason: result.reason,
                        },
                    };
                }
                // A permanent failure (protocol violation, host gone):
                // backing off cannot help, give up now.
                Err(error) if !error.retryable => {
                    tracing::error!(
                        task = %task.id,
                        "task/submit failed permanently: {} → \
                         the source system remains the durable origin",
                        error.message
                    );
                    return SubmitOutcome::GaveUp {
                        error: error.message,
                    };
                }
                Err(error) => {
                    tracing::warn!(
                        task = %task.id,
                        attempt,
                        "task/submit attempt failed (will retry): {}",
                        error.message
                    );
                    last_error = error.message;
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

    /// One attempt: send the request, await its ack (or time out). Final
    /// statuses come back as `Ok`; an `Err` carries whether backing off and
    /// re-submitting can help.
    async fn submit_once(
        &self,
        task: &Task,
        workflow: &str,
    ) -> Result<TaskSubmitResult, AttemptError> {
        let id = format!("submit-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let params = TaskSubmitParams {
            task: task.clone(),
            workflow: workflow.to_string(),
        };
        let request = Request::new(
            RequestId::Str(id.clone()),
            method::TASK_SUBMIT,
            Some(serde_json::to_value(&params).map_err(AttemptError::permanent)?),
        );
        let line = to_line(&request).map_err(AttemptError::permanent)?;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), tx);
        if !self.writer.send_line(line) {
            self.pending.lock().expect("pending lock").remove(&id);
            // The writer channel only closes when the writer task ended —
            // the host is gone for good, not busy.
            return Err(AttemptError::permanent("writer closed (host gone)"));
        }
        match tokio::time::timeout(self.ack_timeout, rx).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(AttemptError::retryable("ack channel dropped")),
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(AttemptError::retryable(format!(
                    "no ack within {:?}",
                    self.ack_timeout
                )))
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
            let rendered = format!("orchestrator answered {code}: {message}");
            // Only the ADR-0008 contract codes are worth a backoff; anything
            // else (INVALID_PARAMS, METHOD_NOT_FOUND, …) is a protocol
            // violation a retry cannot fix.
            if RETRYABLE_CODES.contains(&code) {
                Err(AttemptError::retryable(rendered))
            } else {
                Err(AttemptError::permanent(rendered))
            }
        } else {
            match serde_json::from_value::<TaskSubmitResult>(
                response.get("result").cloned().unwrap_or(Value::Null),
            ) {
                Ok(result) => Ok(result),
                Err(e) => Err(AttemptError::retryable(format!(
                    "malformed task/submit ack: {e}"
                ))),
            }
        };
        let _ = tx.send(outcome);
    }
}

impl Submitter for SubmitClient {
    async fn submit(&self, task: Task, workflow: &str) -> SubmitOutcome {
        self.submit_task(task, workflow).await
    }
}

/// The JSON-RPC error codes worth a backoff (ADR-0008):
/// [`NOT_ACCEPTING`](error_code::NOT_ACCEPTING),
/// [`SUBMIT_OVERLOADED`](error_code::SUBMIT_OVERLOADED),
/// [`INTERNAL_ERROR`](error_code::INTERNAL_ERROR). Any other error code is a
/// protocol violation and fails the submission immediately; final
/// dispositions ride the result instead ([`TaskSubmitStatus`]).
pub const RETRYABLE_CODES: [i64; 3] = [
    error_code::NOT_ACCEPTING,
    error_code::SUBMIT_OVERLOADED,
    error_code::INTERNAL_ERROR,
];
