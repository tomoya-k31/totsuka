//! Errors from the Slack task source.

/// An error talking to the Slack Web API or interpreting its response.
#[derive(Debug, thiserror::Error)]
pub enum SlackError {
    /// `auth.test` rejected the user token. The message carries token-state
    /// specific recovery guidance (see [`auth_failure`]).
    #[error("{0}")]
    Auth(String),
    /// The token authenticates as someone other than `target_user_id` —
    /// refused so the plugin can never reply as somebody else.
    #[error(
        "the user token belongs to `{actual}` but `target_user_id` is `{expected}` → replies \
         would be posted as the wrong person; fix `target_user_id` in `[slack]` of config.toml or \
         supply that user's own xoxp- token"
    )]
    IdentityMismatch {
        /// The configured `target_user_id`.
        expected: String,
        /// The `user_id` reported by `auth.test`.
        actual: String,
    },
    /// A Web API call returned `ok: false` with an error code.
    #[error("Slack API `{method}` failed: {error} → check the token's scopes and the request")]
    Api {
        /// The Web API method (e.g. `auth.test`).
        method: String,
        /// Slack's error code (e.g. `missing_scope`).
        error: String,
    },
    /// The API returned a non-success HTTP status.
    #[error("Slack API returned HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },
    /// Slack rate-limited the call (HTTP 429). Retryable after the delay the
    /// `Retry-After` header asked for.
    #[error("Slack API rate limited `{method}` → retry after {retry_after_secs}s")]
    RateLimited {
        /// The Web API method that was throttled.
        method: String,
        /// Seconds to wait, from the `Retry-After` header.
        retry_after_secs: u64,
    },
    /// A network/transport failure (retryable).
    #[error("Slack API transport error: {0}")]
    Transport(String),
    /// The request timed out (retryable).
    #[error("Slack API request timed out after {0}s")]
    Timeout(u64),
    /// The response was not the JSON shape we expected.
    #[error("Slack returned an unexpected response: {0}")]
    InvalidResponse(String),
    /// The plugin built a request Slack could not be asked (programmer error,
    /// e.g. non-object Web API arguments) — distinct from [`InvalidResponse`]
    /// so the log does not blame Slack for a local bug.
    ///
    /// [`InvalidResponse`]: SlackError::InvalidResponse
    #[error("invalid Slack API request (plugin bug): {0}")]
    InvalidRequest(String),
}

impl SlackError {
    /// Whether retrying with backoff is worthwhile: transient network,
    /// timeouts, rate limiting (429) and 5xx server errors.
    pub fn is_retryable(&self) -> bool {
        match self {
            SlackError::Transport(_) | SlackError::Timeout(_) | SlackError::RateLimited { .. } => {
                true
            }
            SlackError::Http { status, .. } => (500..=599).contains(status),
            _ => false,
        }
    }

    /// Whether the request is known to have been *rejected* rather than
    /// possibly applied — a 429 never processed the call, so replaying it is
    /// safe even for a non-idempotent mutation (a lost 5xx/timeout is not).
    pub fn is_rejected(&self) -> bool {
        matches!(self, SlackError::RateLimited { .. })
    }

    /// Whether this is a startup-blocking credential/identity problem (the
    /// TokenGuard class of failures), as opposed to a transient runtime error.
    pub fn is_credential(&self) -> bool {
        matches!(
            self,
            SlackError::Auth(_) | SlackError::IdentityMismatch { .. }
        )
    }
}

/// Map an `auth.test` failure code to a [`SlackError::Auth`] whose message
/// explains the cause and how to recover (token re-issue / app re-install /
/// Keychain update).
pub fn auth_failure(error: &str) -> SlackError {
    let message = match error {
        "invalid_auth" => {
            "Slack rejected the user token (invalid_auth) → the token is wrong or expired; \
             re-issue the User OAuth Token (xoxp-) from the Slack app's OAuth & Permissions \
             page and update the secret referenced by `user_token` in `[slack]` of config.toml \
             (e.g. the Keychain entry)"
        }
        "token_revoked" => {
            "the user token was revoked (token_revoked) → re-install the Slack app to your \
             workspace to issue a fresh User OAuth Token (xoxp-), then update the secret \
             referenced by `user_token` in `[slack]` of config.toml (e.g. the Keychain entry)"
        }
        "account_inactive" => {
            "the token's Slack account is deactivated (account_inactive) → the workspace \
             account tied to this token no longer works; ask a workspace admin, or issue the \
             token from an active account and update `[slack]` in config.toml"
        }
        other => {
            return SlackError::Auth(format!(
                "Slack `auth.test` failed: {other} → check the user token and the Slack app's \
                 configuration, then update `[slack]` in config.toml"
            ));
        }
    };
    SlackError::Auth(message.to_string())
}

/// Map a credential-class failure of an *App-Level Token* call
/// (`apps.connections.open`) to a [`SlackError::Auth`] with xapp-specific
/// recovery guidance — the fix differs from the user token's (regenerate the
/// xapp- token rather than re-issue the xoxp- one).
pub fn app_auth_failure(error: &str) -> SlackError {
    let hint = match error {
        "account_inactive" => {
            "the Slack account behind the app is deactivated (account_inactive); from an \
             active account, "
        }
        _ => "",
    };
    SlackError::Auth(format!(
        "Slack rejected the App-Level Token ({error}) → {hint}regenerate the xapp- token \
         under the Slack app's Basic Information > App-Level Tokens (scope \
         `connections:write`) and update the secret referenced by `app_token` in \
         `[slack]` in config.toml"
    ))
}

/// Map a credential-class failure of a *bot token* call (the notification
/// nudge) to a [`SlackError::Auth`] with xoxb-specific recovery guidance —
/// the fix differs from both other tokens' (copy the Bot User OAuth Token,
/// which a re-install re-issues alongside the xoxp- one).
pub fn bot_auth_failure(error: &str) -> SlackError {
    let hint = match error {
        "token_revoked" => "the bot token was revoked (token_revoked); ",
        "account_inactive" => {
            "the Slack account behind the app is deactivated (account_inactive); from an \
             active account, "
        }
        _ => "",
    };
    SlackError::Auth(format!(
        "Slack rejected the bot token ({error}) → {hint}re-install the Slack app if needed, \
         copy the Bot User OAuth Token (xoxb-) from its OAuth & Permissions page, and update \
         the secret referenced by `bot_token` in `[slack]` of config.toml (a re-install also \
         re-issues the xoxp- user token — update both)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_carries_recovery_guidance() {
        assert!(
            auth_failure("invalid_auth")
                .to_string()
                .contains("re-issue")
        );
        assert!(
            auth_failure("token_revoked")
                .to_string()
                .contains("re-install")
        );
        assert!(
            auth_failure("account_inactive")
                .to_string()
                .contains("deactivated")
        );
        assert!(
            auth_failure("weird_error")
                .to_string()
                .contains("weird_error")
        );
    }

    #[test]
    fn retryable_classification() {
        assert!(SlackError::Transport("x".into()).is_retryable());
        assert!(SlackError::Timeout(30).is_retryable());
        assert!(
            SlackError::RateLimited {
                method: "chat.postMessage".into(),
                retry_after_secs: 5
            }
            .is_retryable()
        );
        assert!(
            SlackError::Http {
                status: 503,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(
            !SlackError::Http {
                status: 404,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(!auth_failure("invalid_auth").is_retryable());
    }

    #[test]
    fn only_rate_limiting_counts_as_rejected() {
        assert!(
            SlackError::RateLimited {
                method: "chat.postMessage".into(),
                retry_after_secs: 5
            }
            .is_rejected()
        );
        // A timeout / 5xx may have applied the write; replaying could duplicate.
        assert!(!SlackError::Timeout(30).is_rejected());
        assert!(
            !SlackError::Http {
                status: 503,
                body: String::new()
            }
            .is_rejected()
        );
    }

    #[test]
    fn app_auth_failure_points_at_the_xapp_token() {
        for code in ["invalid_auth", "token_revoked", "account_inactive"] {
            let err = app_auth_failure(code);
            assert!(err.is_credential(), "{code}");
            let message = err.to_string();
            assert!(message.contains(code), "{message}");
            assert!(message.contains("xapp-"), "{message}");
            assert!(message.contains("connections:write"), "{message}");
        }
        assert!(
            app_auth_failure("account_inactive")
                .to_string()
                .contains("deactivated")
        );
    }

    #[test]
    fn bot_auth_failure_points_at_the_xoxb_token() {
        for code in ["invalid_auth", "token_revoked", "account_inactive"] {
            let err = bot_auth_failure(code);
            assert!(err.is_credential(), "{code}");
            let message = err.to_string();
            assert!(message.contains(code), "{message}");
            assert!(message.contains("xoxb-"), "{message}");
            assert!(message.contains("bot_token"), "{message}");
        }
        // A re-install re-issues the user token too; the guidance must say so.
        assert!(
            bot_auth_failure("token_revoked")
                .to_string()
                .contains("xoxp-")
        );
    }

    #[test]
    fn credential_classification() {
        assert!(auth_failure("invalid_auth").is_credential());
        assert!(
            SlackError::IdentityMismatch {
                expected: "U1".into(),
                actual: "U2".into()
            }
            .is_credential()
        );
        assert!(!SlackError::Transport("x".into()).is_credential());
    }
}
