//! Typed wrappers over the four Discord REST routes this plugin uses.
//! Everything above this layer talks to Discord only through [`DiscordApi`].

use serde_json::{Value, json};

use crate::error::DiscordError;
use crate::transport::{DiscordTransport, HttpMethod};

/// One message out of a channel's history, or a live `MESSAGE_CREATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMessage {
    /// Snowflake id — also the id of the thread started from it, if any.
    pub id: String,
    /// Channel the message was posted in.
    pub channel_id: String,
    /// Author's user id, absent only from payloads that carry no author.
    pub author_id: Option<String>,
    /// Whether the author is a bot or webhook. Either means "not a human
    /// post", and both must be excluded — a webhook message carries no `bot`
    /// flag on its author.
    pub author_is_bot: bool,
    /// Message body. Empty unless the `MESSAGE_CONTENT` intent is enabled.
    pub content: String,
    /// The type field: only `0` (DEFAULT) and `19` (REPLY) are ordinary user
    /// posts; joins, pins, thread-created notices and the rest are system
    /// messages wearing the same shape.
    pub kind: u64,
}

/// Discord's `type` for an ordinary post.
const MESSAGE_TYPE_DEFAULT: u64 = 0;
/// Discord's `type` for a post that replies to another message. Still an
/// ordinary post — the reply is a UI relationship, not a thread.
const MESSAGE_TYPE_REPLY: u64 = 19;

impl DiscordMessage {
    /// Whether this is a plain human post, as opposed to a system message
    /// (joins, pins, "started a thread") or a bot/webhook post.
    pub fn is_human_post(&self) -> bool {
        !self.author_is_bot && matches!(self.kind, MESSAGE_TYPE_DEFAULT | MESSAGE_TYPE_REPLY)
    }
}

/// Parse one message object, whether it came from a REST history page or a
/// live `MESSAGE_CREATE` dispatch — the two carry the same shape, which is
/// what lets one filter table serve both paths.
pub fn parse_message_payload(value: &Value) -> DiscordMessage {
    let author = value.get("author");
    DiscordMessage {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        channel_id: value
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        author_id: author
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        // `webhook_id` is the other half: a webhook post's author has no
        // `bot` flag, so checking only that would let one through.
        author_is_bot: author
            .and_then(|a| a.get("bot"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || value.get("webhook_id").is_some(),
        content: value
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        kind: value.get("type").and_then(Value::as_u64).unwrap_or(0),
    }
}

/// Discord REST client, generic over its transport for testability.
pub struct DiscordApi<T> {
    transport: T,
}

impl<T: DiscordTransport> DiscordApi<T> {
    /// A client over `transport`.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// `GET /users/@me` — the token guard. Answers the bot's own user id,
    /// which the watch filter needs so the plugin never reacts to its own
    /// posts even if Discord ever omitted the `bot` flag.
    pub async fn current_user_id(&self) -> Result<String, DiscordError> {
        let response = self
            .transport
            .call(HttpMethod::Get, "/users/@me", None, true)
            .await?;
        response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| DiscordError::InvalidResponse("`/users/@me` has no `id`".into()))
    }

    /// `GET /channels/{id}` — the channel's current name, for the rename
    /// check at startup.
    pub async fn channel_name(&self, channel_id: &str) -> Result<String, DiscordError> {
        let response = self
            .transport
            .call(
                HttpMethod::Get,
                &format!("/channels/{channel_id}"),
                None,
                true,
            )
            .await?;
        response
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                DiscordError::InvalidResponse(format!("`/channels/{channel_id}` has no `name`"))
            })
    }

    /// `GET /channels/{id}/messages` — the most recent messages, newest
    /// first, bounded by `limit` and by `after` (a snowflake).
    ///
    /// The age bound rides on `after` because a snowflake **encodes its
    /// creation time**: a synthetic snowflake for "now − max_age" is a valid
    /// lower bound with no extra round trip and no clock of Discord's to
    /// consult.
    pub async fn channel_messages(
        &self,
        channel_id: &str,
        limit: u32,
        after: &str,
    ) -> Result<Vec<DiscordMessage>, DiscordError> {
        let response = self
            .transport
            .call(
                HttpMethod::Get,
                &format!("/channels/{channel_id}/messages?limit={limit}&after={after}"),
                None,
                true,
            )
            .await?;
        let messages = response.as_array().ok_or_else(|| {
            DiscordError::InvalidResponse(
                "`/channels/{id}/messages` did not answer with an array".into(),
            )
        })?;
        Ok(messages.iter().map(parse_message_payload).collect())
    }

    /// `POST /channels/{id}/messages` — post into a channel (or a thread,
    /// which *is* a channel). Never idempotent: a replayed send posts twice.
    pub async fn create_message(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<String, DiscordError> {
        let response = self
            .transport
            .call(
                HttpMethod::Post,
                &format!("/channels/{channel_id}/messages"),
                Some(json!({ "content": content })),
                false,
            )
            .await?;
        response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| DiscordError::InvalidResponse("the sent message has no `id`".into()))
    }

    /// `POST /channels/{id}/messages/{message_id}/threads` — start a public
    /// thread from a message, so the result hangs off the post that caused
    /// it rather than landing loose in the channel.
    ///
    /// The new thread's id **equals the starter message's id**, so the caller
    /// already knows where to post; the return value is only useful as
    /// confirmation. Fails with 400 if a thread already exists there, which
    /// the caller treats as "use the message id anyway".
    pub async fn start_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
    ) -> Result<String, DiscordError> {
        let response = self
            .transport
            .call(
                HttpMethod::Post,
                &format!("/channels/{channel_id}/messages/{message_id}/threads"),
                Some(json!({ "name": name, "auto_archive_duration": 1440 })),
                false,
            )
            .await?;
        response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| DiscordError::InvalidResponse("the new thread has no `id`".into()))
    }
}

/// A synthetic snowflake whose embedded timestamp is `unix_millis`.
///
/// Discord snowflakes put the milliseconds since its own epoch in the top 42
/// bits, so a value built this way sorts exactly where a real message from
/// that instant would — which is all `after` needs.
pub fn snowflake_for(unix_millis: u64) -> String {
    /// Discord's epoch: 2015-01-01T00:00:00Z, in milliseconds.
    const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
    let since = unix_millis.saturating_sub(DISCORD_EPOCH_MS);
    (since << 22).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_post_is_human_and_the_others_are_not() {
        let base = DiscordMessage {
            id: "1".into(),
            channel_id: "C".into(),
            author_id: Some("U".into()),
            author_is_bot: false,
            content: "https://example.com".into(),
            kind: MESSAGE_TYPE_DEFAULT,
        };
        assert!(base.is_human_post());
        // A reply is still an ordinary post.
        assert!(
            DiscordMessage {
                kind: MESSAGE_TYPE_REPLY,
                ..base.clone()
            }
            .is_human_post()
        );
        // A system message ("started a thread", a join, a pin) is not.
        assert!(
            !DiscordMessage {
                kind: 18,
                ..base.clone()
            }
            .is_human_post()
        );
        assert!(
            !DiscordMessage {
                author_is_bot: true,
                ..base
            }
            .is_human_post()
        );
    }

    #[test]
    fn a_webhook_post_counts_as_a_bot_even_without_the_bot_flag() {
        let value = serde_json::json!({
            "id": "1", "channel_id": "C", "type": 0, "content": "x",
            "webhook_id": "W1",
            "author": { "id": "U1", "username": "custom name" },
        });
        let message = parse_message_payload(&value);
        assert!(
            message.author_is_bot,
            "a webhook author carries no `bot` flag; checking only that would let it through"
        );
        assert!(!message.is_human_post());
    }

    #[test]
    fn a_message_missing_optional_fields_still_parses() {
        let value = serde_json::json!({ "id": "1", "channel_id": "C", "type": 0 });
        let message = parse_message_payload(&value);
        assert_eq!(message.content, "");
        assert_eq!(message.author_id, None);
        assert!(!message.author_is_bot);
    }

    /// The `after` bound is a *time*, expressed as a snowflake. If the shift
    /// or the epoch were wrong the backfill would silently read the wrong
    /// window, so pin both against a known value.
    #[test]
    fn a_synthetic_snowflake_encodes_the_instant_it_was_built_for() {
        // Discord's own epoch maps to 0.
        assert_eq!(snowflake_for(1_420_070_400_000), "0");
        // One millisecond later is 1 << 22.
        assert_eq!(snowflake_for(1_420_070_400_001), (1u64 << 22).to_string());
        // Anything before the epoch saturates rather than wrapping.
        assert_eq!(snowflake_for(0), "0");
        // Later instants sort after earlier ones, which is the whole contract.
        let earlier: u64 = snowflake_for(1_700_000_000_000).parse().unwrap();
        let later: u64 = snowflake_for(1_700_000_001_000).parse().unwrap();
        assert!(later > earlier);
    }
}
