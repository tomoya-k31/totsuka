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
}
