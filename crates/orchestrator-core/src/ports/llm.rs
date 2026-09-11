//! LLM router port for OpenAI-compatible AI Gateways (F-12, F-13).
//!
//! The router makes a `/chat/completions` call requesting structured JSON
//! output and returns the parsed object. Concrete transport (reqwest) lives in
//! [`adapters::llm`](crate::adapters::llm) behind this trait so it can be
//! swapped and the [`repo_select`](crate::repo_select) pipeline mocked (§6).

use std::future::Future;

use serde_json::Value;

/// A structured-output chat request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// System prompt (instructions).
    pub system: String,
    /// User prompt (the task + candidates).
    pub user: String,
    /// JSON Schema the model must conform to (`response_format: json_schema`).
    pub json_schema: Value,
    /// Max output tokens.
    pub max_tokens: Option<u32>,
}

/// Errors from an LLM call. `Transport`/`Timeout` are retryable (§5.3);
/// `InvalidResponse` means the model returned unusable output.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Network/HTTP transport failure (retryable).
    #[error("llm transport error: {0}")]
    Transport(String),
    /// The request timed out (retryable).
    #[error("llm request timed out after {0}s")]
    Timeout(u64),
    /// The gateway returned a non-success status.
    #[error("llm gateway returned status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },
    /// The response was not valid JSON / not the expected shape.
    #[error("llm returned an invalid response: {0}")]
    InvalidResponse(String),
}

impl LlmError {
    /// Whether the gateway rejected our credentials (401/403).
    ///
    /// Distinct from every other failure: a bad key does not get better on
    /// its own, so `doctor --online` reports it as a failed check while a
    /// timeout or a 5xx is only advisory (#267).
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            LlmError::Status {
                status: 401 | 403,
                ..
            }
        )
    }

    /// Whether the gateway failed to serve at all: no connection, no answer
    /// in time, or a 5xx from the far side.
    ///
    /// The "is the LLM alive" question (F-111). Distinct from
    /// [`is_retryable`](Self::is_retryable), which also counts 429 — a
    /// throttled gateway is very much alive — and from every 4xx: a 400 for a
    /// rejected schema or a 404 for a wrong model name is the gateway
    /// *answering*, just not the way we hoped.
    pub fn is_unreachable(&self) -> bool {
        matches!(
            self,
            LlmError::Transport(_)
                | LlmError::Timeout(_)
                | LlmError::Status {
                    status: 500..=599,
                    ..
                }
        )
    }

    /// Whether the error is worth retrying with backoff (§5.3).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmError::Transport(_)
                | LlmError::Timeout(_)
                | LlmError::Status {
                    status: 429 | 500..=599,
                    ..
                }
        )
    }
}

/// Calls an OpenAI-compatible chat endpoint and returns parsed JSON.
pub trait LlmRouter: Send + Sync {
    /// Send `request` and return the model's JSON object (schema-constrained).
    fn chat_json(
        &self,
        request: &ChatRequest,
    ) -> impl Future<Output = Result<Value, LlmError>> + Send;

    /// Ask the gateway whether it is there and accepts our credentials, with
    /// the cheapest request it will answer.
    ///
    /// The liveness check the engine runs at startup, after a resume from
    /// sleep, and while no real call has been made for a while. A
    /// **liveness** question, not a correctness one: it never sends a
    /// schema, because a provider rejecting our structured-output shape (400)
    /// would masquerade as an outage. The default answers "alive" — the
    /// right answer for a test double, and the only honest one for a router
    /// that has no gateway to ask.
    fn probe(&self) -> impl Future<Output = Result<(), LlmError>> + Send {
        async { Ok(()) }
    }

    /// Drop every pooled connection, so the next call opens a fresh one.
    ///
    /// Called when the engine detects that the machine slept: keep-alive
    /// connections that survived the suspend are half-open, and the first
    /// request on one either fails at once or hangs until the timeout. The
    /// default is a no-op for routers that hold no connections.
    fn reset_connections(&self) {}
}
