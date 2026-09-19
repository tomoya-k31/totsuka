//! [`ClassifyError`]: every way a classification can fail, sorted by what the
//! caller should do about it.

use serde_json::Value;

/// How much of an unrecognised error body is kept.
const ERROR_BODY_HEAD_CHARS: usize = 500;

/// A failed classification.
///
/// The first three variants are the backend failing to answer usefully at the
/// HTTP level; the last two are the backend answering with something we cannot
/// use. Callers treat the two groups differently: a bad answer is worth asking
/// again (F-14), a transport failure has already been retried (§5.3).
#[derive(Debug, Clone, thiserror::Error)]
pub enum ClassifyError {
    /// Network-level failure: DNS, refused connection, broken body read.
    #[error("transport error: {0}")]
    Transport(String),
    /// No answer within the request timeout (seconds).
    #[error("no answer within {0}s")]
    Timeout(u64),
    /// The backend answered with a non-success status.
    #[error("HTTP {status}: {message}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// The provider's own `error.message` when the body is the standard
        /// error envelope, else the truncated body (see [`ClassifyError::status`]).
        message: String,
    },
    /// The backend answered, but not with a verdict we can read.
    #[error("unusable response: {0}")]
    InvalidResponse(String),
    /// The verdict named a repository that is not among the candidates.
    #[error("unknown repository `{0}` (not a candidate)")]
    UnknownRepo(String),
}

impl ClassifyError {
    /// A non-success `status` with its response `body`.
    ///
    /// The body is **narrowed** to the provider's `error.message` when it is the
    /// standard OpenAI-compatible envelope. These errors are logged, printed
    /// by `doctor`, and logged by plugins that have no redacting layer; a
    /// gateway echoing the offending credential back in a 401 body would put
    /// it straight into all three. An unrecognised shape still falls back to
    /// the truncated raw body — that is precisely when the operator needs it.
    pub fn status(status: u16, body: &str) -> Self {
        Self::Status {
            status,
            message: provider_message(body)
                .unwrap_or_else(|| body.chars().take(ERROR_BODY_HEAD_CHARS).collect()),
        }
    }

    /// Whether the backend rejected our credentials (401/403).
    ///
    /// Distinct from every other failure: a bad key does not get better on its
    /// own, so it is reported as a broken configuration rather than an
    /// inconclusive answer (#267).
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: 401 | 403,
                ..
            }
        )
    }

    /// Whether the backend failed to serve at all: no connection, no answer in
    /// time, or a 5xx from the far side (F-111).
    ///
    /// Not [`is_retryable`](Self::is_retryable), which also counts 429 — a
    /// throttled gateway is very much alive — and not any other 4xx: a 400 for
    /// a rejected request or a 404 for a wrong model is the gateway
    /// *answering*, just not the way we hoped.
    pub fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::Timeout(_)
                | Self::Status {
                    status: 500..=599,
                    ..
                }
        )
    }

    /// Whether the failure is worth retrying with backoff (§5.3).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::Timeout(_)
                | Self::Status {
                    status: 429 | 500..=599,
                    ..
                }
        )
    }

    /// Whether the backend answered with something unusable — a verdict that
    /// asking again may fix (F-14), as opposed to a transport failure.
    pub fn is_bad_answer(&self) -> bool {
        matches!(self, Self::InvalidResponse(_) | Self::UnknownRepo(_))
    }
}

/// `error.message` out of an OpenAI-compatible error envelope, truncated.
/// `None` when the body is not that shape (HTML error page, bare text, a
/// proxy's own format).
fn provider_message(body: &str) -> Option<String> {
    let envelope: Value = serde_json::from_str(body).ok()?;
    Some(
        envelope
            .get("error")?
            .get("message")?
            .as_str()?
            .chars()
            .take(ERROR_BODY_HEAD_CHARS)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recognised_error_envelope_keeps_only_the_provider_message() {
        let err = ClassifyError::status(
            401,
            r#"{"error":{"message":"User not found.","code":401},"key":"sk-live-nope"}"#,
        );
        assert_eq!(err.to_string(), "HTTP 401: User not found.");
        assert!(!err.to_string().contains("sk-live"), "{err}");
    }

    #[test]
    fn an_unrecognised_body_is_kept_but_truncated() {
        for body in [
            "<html>502</html>",
            r#"{"detail":"nope"}"#,
            r#"{"error":"bare string"}"#,
        ] {
            assert_eq!(
                ClassifyError::status(502, body).to_string(),
                format!("HTTP 502: {body}")
            );
        }
        let long = "x".repeat(2 * ERROR_BODY_HEAD_CHARS);
        let ClassifyError::Status { message, .. } = ClassifyError::status(500, &long) else {
            unreachable!()
        };
        assert_eq!(message.chars().count(), ERROR_BODY_HEAD_CHARS);
    }

    #[test]
    fn only_401_and_403_are_auth_failures() {
        let status = |s| ClassifyError::status(s, "");
        assert!(status(401).is_auth_failure());
        assert!(status(403).is_auth_failure());
        // Busy, broken, or malformed — none of them say the key is bad.
        for other in [400, 404, 429, 500, 503] {
            assert!(!status(other).is_auth_failure(), "{other}");
        }
        assert!(!ClassifyError::Timeout(30).is_auth_failure());
        assert!(!ClassifyError::Transport("refused".into()).is_auth_failure());
        // An auth failure is terminal, never retried.
        assert!(!status(401).is_retryable());
    }

    #[test]
    fn throttling_is_retryable_but_not_unreachable() {
        let throttled = ClassifyError::status(429, "");
        assert!(throttled.is_retryable());
        assert!(!throttled.is_unreachable());
        let down = ClassifyError::status(503, "");
        assert!(down.is_retryable() && down.is_unreachable());
    }

    #[test]
    fn only_unusable_verdicts_are_bad_answers() {
        assert!(ClassifyError::InvalidResponse("x".into()).is_bad_answer());
        assert!(ClassifyError::UnknownRepo("x".into()).is_bad_answer());
        assert!(!ClassifyError::Timeout(1).is_bad_answer());
        assert!(!ClassifyError::status(400, "").is_bad_answer());
    }
}
