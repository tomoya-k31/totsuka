//! Error type for the Discord plugin, and the recovery guidance that turns a
//! failure into something an operator can act on.

use thiserror::Error;

/// Anything that can go wrong talking to Discord.
#[derive(Debug, Error)]
pub enum DiscordError {
    /// The bot token is wrong, revoked, or lacks what the call needs. Carries
    /// guidance rather than the raw status, because the raw status is the
    /// least useful half.
    #[error("{0}")]
    Auth(String),
    /// The Gateway refused the connection in a way reconnecting cannot fix
    /// (close codes 4004 / 4010–4014). Carries guidance for the same reason.
    #[error("{0}")]
    Gateway(String),
    /// A REST call answered with a non-success status.
    #[error("discord API error on {method}: HTTP {status}{}", detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default())]
    Api {
        /// The route, for the log.
        method: String,
        /// The HTTP status.
        status: u16,
        /// Discord's own `message` field, when the body carried one.
        detail: Option<String>,
    },
    /// The response was not the shape this plugin expects.
    #[error("unexpected discord response: {0}")]
    InvalidResponse(String),
    /// Transport failure (DNS, TLS, timeout, socket).
    #[error("discord transport error: {0}")]
    Transport(String),
}

impl DiscordError {
    /// Whether this is a credential/permission problem the operator must fix.
    ///
    /// The distinction is load-bearing in two places: `initialize` maps it to
    /// `CONFIG_INVALID` rather than `INTERNAL_ERROR`, and the Gateway loop
    /// **stops** instead of reconnecting forever — an unfixable failure that
    /// retries looks exactly like an outage in the logs.
    pub fn is_credential(&self) -> bool {
        match self {
            Self::Auth(_) | Self::Gateway(_) => true,
            Self::Api { status, .. } => matches!(*status, 401 | 403),
            Self::InvalidResponse(_) | Self::Transport(_) => false,
        }
    }
}

/// Guidance for a REST call rejected as unauthorized.
pub fn auth_failure(status: u16) -> DiscordError {
    let hint = match status {
        401 => {
            "the bot token is invalid or was regenerated → copy the current one from the \
             Developer Portal (Bot → Reset Token issues a NEW token) and update the value \
             `bot_token` points at"
        }
        403 => {
            "the bot lacks permission for this channel → check that it is a member of the \
             server, that its role can View Channel / Read Message History / Send Messages \
             there, and that no channel override takes those away"
        }
        _ => "the bot token was rejected → re-check it in the Developer Portal",
    };
    DiscordError::Auth(format!(
        "discord rejected the bot token (HTTP {status}): {hint}"
    ))
}

/// Guidance for a Gateway close code that reconnecting cannot fix.
///
/// Discord answers a disallowed intent by closing the socket, not by failing
/// the handshake, so **without this mapping the plugin would reconnect in a
/// loop against a configuration problem** — the symptom would read as a flaky
/// network rather than as one un-ticked checkbox.
pub fn gateway_close_failure(code: u16) -> DiscordError {
    let hint = match code {
        4004 => {
            "authentication failed → the bot token is wrong; copy the current one from the \
             Developer Portal"
        }
        4010 => "an invalid shard was sent → this plugin never shards, so this is a bug",
        4011 => {
            "this bot is in too many servers to connect unsharded → totsuka's Discord source \
             is meant for a dedicated server; sharding is not implemented"
        }
        4012 => "the gateway version is no longer supported → upgrade totsuka",
        4013 => "an invalid intent was sent → this is a bug in the plugin's intent bitfield",
        4014 => {
            "a privileged intent is not enabled for this app → open the Developer Portal, \
             Bot → Privileged Gateway Intents, and turn on MESSAGE CONTENT INTENT. Below \
             10,000 users this is a toggle and needs no review"
        }
        _ => "the gateway closed the connection permanently",
    };
    DiscordError::Gateway(format!("discord gateway closed with {code}: {hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_class_covers_what_must_not_be_retried() {
        assert!(auth_failure(401).is_credential());
        assert!(gateway_close_failure(4014).is_credential());
        assert!(
            DiscordError::Api {
                method: "GET /channels/1".into(),
                status: 403,
                detail: None
            }
            .is_credential()
        );
        // …and not the ones that do fix themselves.
        assert!(
            !DiscordError::Api {
                method: "GET /channels/1".into(),
                status: 500,
                detail: None
            }
            .is_credential()
        );
        assert!(!DiscordError::Transport("connection reset".into()).is_credential());
    }

    /// 4014 is the one an operator will actually hit, and the fix is a
    /// checkbox — so the message has to name it.
    #[test]
    fn the_disallowed_intent_message_names_the_toggle() {
        let message = gateway_close_failure(4014).to_string();
        assert!(message.contains("MESSAGE CONTENT INTENT"), "{message}");
        assert!(message.contains("Developer Portal"), "{message}");
    }

    /// A regenerated token is the common 401, and "reset token" is the step
    /// people forget — naming it is the difference between a fixable message
    /// and "HTTP 401".
    #[test]
    fn the_unauthorized_message_names_the_reset_token_trap() {
        let message = auth_failure(401).to_string();
        assert!(message.contains("Reset Token"), "{message}");
        let forbidden = auth_failure(403).to_string();
        assert!(forbidden.contains("Read Message History"), "{forbidden}");
    }
}
