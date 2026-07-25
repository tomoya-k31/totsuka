//! [`LookupClient`]: ask the Orchestrator whether a conversation is already
//! known, before submitting to it (`task/lookup`, protocol 0.2.4, #242).
//!
//! A task source resolves a repository only so a *new* conversation has a
//! `repo_hint`; for a reply inside an existing one the Orchestrator already
//! knows the answer. Resolving anyway is not merely wasted work — it can mean
//! an LLM call, or putting a repository picker in front of a human who
//! already chose.
//!
//! **Failure is not an error condition here.** Unlike
//! [`submit`](crate::submit), which must eventually get its task in, a lookup
//! that times out or errors just means "answer unknown", and the caller does
//! what it did before the RPC existed. That degradation is the contract, so
//! the client neither retries nor backs off: one attempt, a timeout, and an
//! answer of [`Lookup::Unknown`]. Retrying would only make the caller wait
//! longer for the same fallback — and the wait is real, because the
//! Orchestrator answers from its event loop, which can be busy creating a
//! worktree.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_protocol::jsonrpc::{Request, to_line};
use plugin_protocol::methods::{TaskLookupParams, TaskLookupResult};
use plugin_protocol::{RequestId, method};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::runtime::Writer;

/// How long to wait for an answer before falling back. Generous because the
/// engine loop may be busy; a late answer for a timed-out attempt is dropped
/// harmlessly.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the Orchestrator knows about a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The conversation exists. `repo` is the repository it settled on, or
    /// `None` when selection has not settled yet (a human is being asked, or
    /// classification was inconclusive) — treat that as "no hint available",
    /// never as "no repository".
    Known {
        /// The repository this conversation is bound to, if one is chosen.
        repo: Option<String>,
    },
    /// The conversation is new — resolve it as a new task.
    New,
    /// The Orchestrator could not be asked (timeout, transport, error
    /// answer). **Indistinguishable from [`New`](Self::New) in effect**: the
    /// caller must resolve as it would without this RPC. Kept separate so the
    /// caller can log the difference.
    Unknown {
        /// Why the answer is missing.
        reason: String,
    },
}

impl Lookup {
    /// Whether the caller may skip resolving a repository for this delivery.
    ///
    /// True only for a conversation the Orchestrator confirmed it knows: the
    /// repository is already recorded against it, so the plugin has nothing
    /// to contribute. Both [`New`](Self::New) and [`Unknown`](Self::Unknown)
    /// are false — an unanswered lookup must never be read as "known".
    pub fn skips_resolution(&self) -> bool {
        matches!(self, Self::Known { .. })
    }
}

/// One pending answer slot, resolved by [`serve`](crate::runtime::serve) from
/// a response line.
type PendingLookup = oneshot::Sender<Result<TaskLookupResult, String>>;

/// The `task/lookup` client bound to the shared [`Writer`].
///
/// Clonable; all clones share one pending map.
#[derive(Debug, Clone)]
pub struct LookupClient {
    writer: Writer,
    pending: Arc<Mutex<HashMap<String, PendingLookup>>>,
    next_id: Arc<AtomicU64>,
    timeout: Duration,
}

impl LookupClient {
    /// Build a client over `writer` with the production timeout.
    pub fn new(writer: Writer) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(0)),
            timeout: LOOKUP_TIMEOUT,
        }
    }

    /// Override the timeout (tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Ask about one conversation. Never fails: an unanswerable lookup comes
    /// back as [`Lookup::Unknown`] and the caller resolves as it always did.
    pub async fn lookup(&self, source: &str, task_id: &str) -> Lookup {
        match self.lookup_once(source, task_id).await {
            Ok(result) if result.known => Lookup::Known { repo: result.repo },
            Ok(_) => Lookup::New,
            Err(reason) => {
                tracing::debug!(
                    task_id,
                    "task/lookup unanswered ({reason}); resolving as a new conversation"
                );
                Lookup::Unknown { reason }
            }
        }
    }

    /// One attempt: send the request, await its answer (or time out).
    async fn lookup_once(&self, source: &str, task_id: &str) -> Result<TaskLookupResult, String> {
        let id = format!("lookup-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let params = TaskLookupParams {
            source: source.to_string(),
            task_id: task_id.to_string(),
        };
        let request = Request::new(
            RequestId::Str(id.clone()),
            method::TASK_LOOKUP,
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
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err("answer channel dropped".to_string()),
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(format!("no answer within {:?}", self.timeout))
            }
        }
    }

    /// Resolve one response line against the pending map (called by
    /// [`serve`](crate::runtime::serve)). Ids this client did not issue — a
    /// `task/submit` ack, or a late answer for a timed-out attempt — are
    /// ignored.
    pub fn resolve(&self, response: &Value) {
        let Some(id) = response.get("id") else { return };
        let key = match id.as_str() {
            Some(s) => s.to_string(),
            None => id.to_string(),
        };
        let Some(tx) = self.pending.lock().expect("pending lock").remove(&key) else {
            return;
        };
        let answer = if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            Err(format!("orchestrator answered {code}: {message}"))
        } else {
            serde_json::from_value::<TaskLookupResult>(
                response.get("result").cloned().unwrap_or(Value::Null),
            )
            .map_err(|e| format!("malformed task/lookup answer: {e}"))
        };
        let _ = tx.send(answer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_confirmed_conversation_skips_resolution() {
        assert!(Lookup::Known { repo: None }.skips_resolution());
        assert!(
            Lookup::Known {
                repo: Some("totsuka".into())
            }
            .skips_resolution()
        );
        assert!(!Lookup::New.skips_resolution());
        // The important one: an unanswered lookup is never read as "known",
        // or a thread the Orchestrator has never seen would be dispatched
        // with no repository at all.
        assert!(
            !Lookup::Unknown {
                reason: "timeout".into()
            }
            .skips_resolution()
        );
    }
}
