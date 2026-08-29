//! Errors from the GitHub task source.

/// An error talking to the GitHub API or interpreting its response.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// The token was rejected (HTTP 401): bad or expired credentials.
    #[error(
        "GitHub rejected the token (HTTP 401) → check `token` in `[github]` of config.toml (or the secret it references) and its scopes"
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
    /// The API throttled us, with the wait it asked for.
    ///
    /// **GitHub returns 403 *or* 429 for both its primary and secondary rate
    /// limits**, so the status alone cannot separate a throttle from a
    /// permission error — the rate-limit headers decide, and `transport` does
    /// that classification. Carrying the wait here is what lets the retry
    /// honour it instead of guessing with backoff: retrying earlier than asked
    /// is guaranteed to be throttled again, and GitHub penalises clients that
    /// ignore `retry-after`.
    #[error("GitHub API rate limited → retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait, from `retry-after` or `x-ratelimit-reset`.
        retry_after_secs: u64,
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
    /// Whether retrying is worthwhile (§5.3): transient network, timeouts,
    /// throttles and 5xx server errors.
    ///
    /// "Throttle" means [`GithubError::RateLimited`], which `transport` derives
    /// from the rate-limit **headers** — not from a status code. GitHub returns
    /// 403 or 429 for both its rate-limit kinds, so a 403 may or may not be one
    /// and a 429 always is. Note also that a throttle is not retried with
    /// backoff: the wait it carries is honoured exactly.
    pub fn is_retryable(&self) -> bool {
        match self {
            GithubError::Transport(_)
            | GithubError::Timeout(_)
            | GithubError::RateLimited { .. } => true,
            // 429 is absent on purpose: `transport` turns every throttle into
            // `RateLimited` above, so a 429 never reaches this arm. Leaving it
            // here would suggest a second, header-less throttle path exists.
            GithubError::Http { status, .. } => (500..=599).contains(status),
            _ => false,
        }
    }

    /// Whether the request is known to have been **rejected** rather than
    /// possibly applied.
    ///
    /// A throttled call never ran, so replaying it is safe even for a
    /// non-idempotent mutation — unlike a lost 5xx or timeout, where the write
    /// may well have landed.
    pub fn is_rejected(&self) -> bool {
        matches!(self, GithubError::RateLimited { .. })
    }
}
