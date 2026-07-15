//! Typed wrappers over the Slack Web API methods this plugin uses. Everything
//! above this layer (mention detection, repo resolution, the approval flow)
//! talks to Slack only through [`SlackApi`].
//!
//! Every call goes through one shared error handler: credential-class error
//! codes (`invalid_auth` / `token_revoked` / `account_inactive`) are logged
//! with the TokenGuard's recovery guidance so a token that expires *while
//! running* is as actionable in the logs as one that fails at startup.

use serde_json::{Value, json};

use crate::error::{SlackError, app_auth_failure, auth_failure};
use crate::transport::{SlackTransport, TokenKind, expect_ok};

/// The identity behind a token, from `auth.test`.
#[derive(Debug, Clone)]
pub struct AuthIdentity {
    /// The authenticated user's id (`U…`).
    pub user_id: String,
}

/// One message out of a conversation history / thread.
#[derive(Debug, Clone)]
pub struct SlackMessage {
    /// Sender user id — absent for some system/bot messages.
    pub user: Option<String>,
    /// Message text (Slack mrkdwn).
    pub text: String,
    /// Message timestamp — Slack's message id within a channel.
    pub ts: String,
    /// Parent thread timestamp, when the message is in a thread.
    pub thread_ts: Option<String>,
    /// Message subtype (`message_changed`, `bot_message`, …), absent for a
    /// plain user post.
    pub subtype: Option<String>,
    /// Posting bot id, when a bot (or workflow) posted the message.
    pub bot_id: Option<String>,
}

/// Arguments for `chat.postMessage`.
#[derive(Debug, Clone)]
pub struct PostMessage<'a> {
    /// Target channel id.
    pub channel: &'a str,
    /// Message text (also the notification fallback when `blocks` is set).
    pub text: &'a str,
    /// Reply into this thread instead of posting top-level.
    pub thread_ts: Option<&'a str>,
    /// Disable link unfurling (the self-DM record sets `false`).
    pub unfurl_links: Option<bool>,
    /// Block Kit blocks.
    pub blocks: Option<Value>,
}

/// Arguments for `chat.postEphemeral`.
#[derive(Debug, Clone)]
pub struct PostEphemeral<'a> {
    /// Target channel id.
    pub channel: &'a str,
    /// The only user who will see the message.
    pub user: &'a str,
    /// Message text (fallback when `blocks` is set).
    pub text: &'a str,
    /// Show inside this thread.
    pub thread_ts: Option<&'a str>,
    /// Block Kit blocks.
    pub blocks: Option<Value>,
}

/// Arguments for `chat.update`.
#[derive(Debug, Clone)]
pub struct UpdateMessage<'a> {
    /// Channel of the message being updated.
    pub channel: &'a str,
    /// Timestamp of the message being updated.
    pub ts: &'a str,
    /// Replacement text.
    pub text: &'a str,
    /// Replacement Block Kit blocks.
    pub blocks: Option<Value>,
}

/// Slack Web API client, generic over its transport for testability.
pub struct SlackApi<T> {
    transport: T,
}

impl<T: SlackTransport> SlackApi<T> {
    /// A client over `transport`.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Unwrap the `ok` envelope; a credential-class failure is upgraded to
    /// [`SlackError::Auth`] with recovery guidance for the token that made
    /// the call (user vs. App-Level) and logged, so a token that expires
    /// mid-run shows up as actionably in the log as one failing at startup.
    fn checked(token: TokenKind, method: &str, response: Value) -> Result<Value, SlackError> {
        expect_ok(method, response).map_err(|e| match e {
            SlackError::Api { error, .. }
                if matches!(
                    error.as_str(),
                    "invalid_auth" | "token_revoked" | "account_inactive"
                ) =>
            {
                let guided = match token {
                    TokenKind::User => auth_failure(&error),
                    TokenKind::App => app_auth_failure(&error),
                };
                tracing::error!(method, "{guided}");
                guided
            }
            other => other,
        })
    }

    async fn call(
        &self,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> Result<Value, SlackError> {
        let response = self
            .transport
            .call(TokenKind::User, method, body, idempotent)
            .await?;
        Self::checked(TokenKind::User, method, response)
    }

    /// `auth.test` — who the user token authenticates as (TokenGuard).
    ///
    /// Stricter than [`checked`](Self::checked): here *every* `ok: false`
    /// code is credential-class — `auth.test` takes no arguments, so its only
    /// failure mode is the token itself (`token_expired`, `not_authed`, …) —
    /// keeping the TokenGuard's config-vs-internal error split intact.
    pub async fn auth_test(&self) -> Result<AuthIdentity, SlackError> {
        let response = self
            .transport
            .call(TokenKind::User, "auth.test", None, true)
            .await?;
        let response = expect_ok("auth.test", response).map_err(|e| match e {
            SlackError::Api { error, .. } => {
                let guided = auth_failure(&error);
                tracing::error!(method = "auth.test", "{guided}");
                guided
            }
            other => other,
        })?;
        let user_id = string_field(&response, "auth.test", "user_id")?;
        Ok(AuthIdentity { user_id })
    }

    /// `apps.connections.open` — a fresh Socket Mode WebSocket URL. The one
    /// method authenticated by the App-Level Token.
    ///
    /// Like [`auth_test`](Self::auth_test), *every* `ok: false` code is
    /// credential-class: the method takes no arguments, so `missing_scope`,
    /// `not_allowed_token_type`, `invalid_auth`, … all mean the xapp token
    /// (or the app config behind it) is wrong and will not fix itself —
    /// callers must fail fast with guidance instead of retrying forever.
    /// Transport-level replays are disabled (`idempotent: false`, except the
    /// always-safe 429) because the Socket Mode reconnect loop owns the
    /// retry policy for this call — two stacked backoff layers would make
    /// its failure counters lie.
    pub async fn apps_connections_open(&self) -> Result<String, SlackError> {
        let response = self
            .transport
            .call(TokenKind::App, "apps.connections.open", None, false)
            .await?;
        let response = expect_ok("apps.connections.open", response).map_err(|e| match e {
            SlackError::Api { error, .. } => {
                let guided = app_auth_failure(&error);
                tracing::error!(method = "apps.connections.open", "{guided}");
                guided
            }
            other => other,
        })?;
        string_field(&response, "apps.connections.open", "url")
    }

    /// `conversations.replies` — up to `limit` messages of the thread rooted
    /// at `thread_ts` (oldest first, parent included). `latest` bounds the
    /// window from above (inclusive): without it Slack pages from the *head*
    /// of the thread, so a long thread would yield its oldest messages, not
    /// the ones leading up to `latest`.
    pub async fn conversations_replies(
        &self,
        channel: &str,
        thread_ts: &str,
        limit: u32,
        latest: Option<&str>,
    ) -> Result<Vec<SlackMessage>, SlackError> {
        let response = self
            .call(
                "conversations.replies",
                Some(json!({
                    "channel": channel,
                    "ts": thread_ts,
                    "limit": limit,
                    "latest": latest,
                    "inclusive": latest.map(|_| true),
                })),
                true,
            )
            .await?;
        let messages = response
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                SlackError::InvalidResponse(
                    "`conversations.replies` response has no `messages` array".into(),
                )
            })?;
        Ok(messages.iter().map(parse_message).collect())
    }

    /// `conversations.open` with the operator's own user id — the self-DM
    /// channel where drafts are recorded. Idempotent by Slack semantics
    /// (opening an already-open IM returns the same channel).
    pub async fn conversations_open_self(&self, user_id: &str) -> Result<String, SlackError> {
        let response = self
            .call(
                "conversations.open",
                Some(json!({ "users": user_id })),
                true,
            )
            .await?;
        let channel = response
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SlackError::InvalidResponse(
                    "`conversations.open` response has no `channel.id`".into(),
                )
            })?;
        Ok(channel.to_string())
    }

    /// `conversations.info` — the channel's name (`#general` without the
    /// `#`). Callers cache the result; DMs and other unnamed conversations
    /// are an [`SlackError::InvalidResponse`], which callers fall back from.
    pub async fn conversations_info_name(&self, channel: &str) -> Result<String, SlackError> {
        let response = self
            .call(
                "conversations.info",
                Some(json!({ "channel": channel })),
                true,
            )
            .await?;
        let name = response
            .get("channel")
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SlackError::InvalidResponse(
                    "`conversations.info` response has no `channel.name`".into(),
                )
            })?;
        Ok(name.to_string())
    }

    /// `users.info` — a human-readable name for `user_id`: the display name
    /// when set, else the real name, else the account name.
    pub async fn users_info(&self, user_id: &str) -> Result<String, SlackError> {
        let response = self
            .call("users.info", Some(json!({ "user": user_id })), true)
            .await?;
        let user = response.get("user").ok_or_else(|| {
            SlackError::InvalidResponse("`users.info` response has no `user`".into())
        })?;
        let profile = user.get("profile");
        let name = [
            profile
                .and_then(|p| p.get("display_name"))
                .and_then(Value::as_str),
            profile
                .and_then(|p| p.get("real_name"))
                .and_then(Value::as_str),
            user.get("name").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        .find(|n| !n.is_empty())
        .ok_or_else(|| {
            SlackError::InvalidResponse("`users.info` response has no usable name".into())
        })?;
        Ok(name.to_string())
    }

    /// `chat.getPermalink` — the permalink recorded alongside a mention.
    pub async fn chat_get_permalink(
        &self,
        channel: &str,
        message_ts: &str,
    ) -> Result<String, SlackError> {
        let response = self
            .call(
                "chat.getPermalink",
                Some(json!({ "channel": channel, "message_ts": message_ts })),
                true,
            )
            .await?;
        string_field(&response, "chat.getPermalink", "permalink")
    }

    /// `chat.postMessage` — post as the operator (user token). Non-idempotent:
    /// never auto-retried on an ambiguous failure. Returns the new message ts.
    pub async fn chat_post_message(&self, message: &PostMessage<'_>) -> Result<String, SlackError> {
        let response = self
            .call(
                "chat.postMessage",
                Some(json!({
                    "channel": message.channel,
                    "text": message.text,
                    "thread_ts": message.thread_ts,
                    "unfurl_links": message.unfurl_links,
                    "blocks": message.blocks,
                })),
                false,
            )
            .await?;
        string_field(&response, "chat.postMessage", "ts")
    }

    /// `chat.postEphemeral` — visible only to `user`. Non-idempotent.
    pub async fn chat_post_ephemeral(&self, message: &PostEphemeral<'_>) -> Result<(), SlackError> {
        self.call(
            "chat.postEphemeral",
            Some(json!({
                "channel": message.channel,
                "user": message.user,
                "text": message.text,
                "thread_ts": message.thread_ts,
                "blocks": message.blocks,
            })),
            false,
        )
        .await?;
        Ok(())
    }

    /// `chat.update` — rewrite an existing message (the self-DM record's
    /// state transitions). Idempotent: re-applying the same content is safe.
    pub async fn chat_update(&self, update: &UpdateMessage<'_>) -> Result<(), SlackError> {
        self.call(
            "chat.update",
            Some(json!({
                "channel": update.channel,
                "ts": update.ts,
                "text": update.text,
                "blocks": update.blocks,
            })),
            true,
        )
        .await?;
        Ok(())
    }

    /// POST to an interaction's `response_url` — rewrites the ephemeral the
    /// button lived in. The URL is valid for 30 minutes / 5 uses, so failures
    /// are surfaced, never retried.
    pub async fn post_response_url(&self, url: &str, body: Value) -> Result<(), SlackError> {
        self.transport.post_url(url, body).await
    }
}

/// A required top-level string field of a Web API response.
fn string_field(response: &Value, method: &str, field: &str) -> Result<String, SlackError> {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| SlackError::InvalidResponse(format!("`{method}` response has no `{field}`")))
}

/// Parse one entry of a `messages` array, tolerating absent fields (Slack
/// message shapes vary by subtype).
fn parse_message(value: &Value) -> SlackMessage {
    let text = |field: &str| value.get(field).and_then(Value::as_str).map(str::to_string);
    SlackMessage {
        user: text("user"),
        text: text("text").unwrap_or_default(),
        ts: text("ts").unwrap_or_default(),
        thread_ts: text("thread_ts"),
        subtype: text("subtype"),
        bot_id: text("bot_id"),
    }
}
