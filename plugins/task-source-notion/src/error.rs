//! Errors from the Notion task source.

/// An error talking to the Notion API or interpreting its response.
#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    /// The token was rejected (HTTP 401): a bad or expired integration secret,
    /// or a Notion CLI login that expired or moved to another workspace.
    ///
    /// The next action differs by which kind of token is configured, so the
    /// message names both rather than only the integration case — a CLI token
    /// has no "share the integration with the database" setting to check, and
    /// sending its holder looking for one costs more than the extra clause.
    #[error(
        "Notion rejected the token (HTTP 401) → check `token` in `[notion]` of config.toml (or the secret it references). An integration secret needs the database shared with the integration; a Notion CLI token (`cmd:ntn auth token`) instead expires with the CLI login and follows whichever workspace you logged into"
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
    /// The workflow's trigger could not be read (#572).
    ///
    /// Unreachable in a running plugin: `initialize` parses every trigger and
    /// refuses to start on the same error. It exists so `fetch` has somewhere
    /// to put the failure rather than falling back to a default that differs
    /// from what the operator wrote.
    #[error("{0}")]
    InvalidTrigger(String),
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
