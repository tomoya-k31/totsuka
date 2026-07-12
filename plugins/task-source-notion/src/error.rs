//! Errors from the Notion task source.

/// An error talking to the Notion API or interpreting its response.
#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    /// The token was rejected (HTTP 401): bad or expired integration secret.
    #[error(
        "Notion rejected the token (HTTP 401) → check `token` in plugins/notion.toml (or the referenced env/Keychain secret) and that the integration is shared with the database"
    )]
    Unauthorized,
    /// The API returned a non-success HTTP status.
    #[error("Notion API returned HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },
    /// A network/transport failure (retryable).
    #[error("Notion API transport error: {0}")]
    Transport(String),
    /// The request timed out (retryable).
    #[error("Notion API request timed out after {0}s")]
    Timeout(u64),
    /// The response was not the JSON shape we expected.
    #[error("Notion returned an unexpected response: {0}")]
    InvalidResponse(String),
    /// A referenced entity (property, status option) was not found.
    #[error("{0}")]
    NotFound(String),
}

impl NotionError {
    /// Whether retrying with backoff is worthwhile (§5.3): transient network,
    /// timeouts, rate limiting (429) and 5xx server errors.
    pub fn is_retryable(&self) -> bool {
        match self {
            NotionError::Transport(_) | NotionError::Timeout(_) => true,
            NotionError::Http { status, .. } => *status == 429 || (500..=599).contains(status),
            _ => false,
        }
    }
}
