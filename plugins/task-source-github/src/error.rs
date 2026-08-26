//! Errors from the GitHub task source.

/// An error talking to the GitHub API or interpreting its response.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// The token was rejected (HTTP 401): bad or expired credentials.
    #[error(
        "GitHub rejected the token (HTTP 401) → check `token` in `[github]` of config.toml (or the referenced env/Keychain secret) and its scopes"
    )]
    Unauthorized,
    /// The API returned a non-success HTTP status.
    #[error("GitHub API returned HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },
    /// A network/transport failure (retryable).
    #[error("GitHub API transport error: {0}")]
    Transport(String),
    /// The request timed out (retryable).
    #[error("GitHub API request timed out after {0}s")]
    Timeout(u64),
    /// The GraphQL response carried an `errors` array.
    #[error("GitHub GraphQL error: {0}")]
    GraphQl(String),
    /// The response was not the JSON shape we expected.
    #[error("GitHub returned an unexpected response: {0}")]
    InvalidResponse(String),
    /// A referenced entity (project status option, field) was not found.
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

impl GithubError {
    /// Whether retrying with backoff is worthwhile (§5.3): transient network,
    /// timeouts, rate limiting (429) and 5xx server errors.
    pub fn is_retryable(&self) -> bool {
        match self {
            GithubError::Transport(_) | GithubError::Timeout(_) => true,
            GithubError::Http { status, .. } => *status == 429 || (500..=599).contains(status),
            _ => false,
        }
    }
}
