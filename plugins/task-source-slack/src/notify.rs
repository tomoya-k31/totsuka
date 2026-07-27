//! The bot-DM notification nudge (#305).
//!
//! Both presentation surfaces the plugin uses — the in-thread ephemeral and
//! the self-DM record — generate **no** Slack notification: ephemerals never
//! notify, and a self-DM posted under the operator's own name is their own
//! message. So the operator misses drafts and pickers unless they happen to
//! be looking. When a `bot_token` is configured, a short bot→operator DM
//! carries the native push/badge instead (desktop and mobile), while every
//! real post stays on the user token.

use crate::pipeline::SharedState;
use crate::slack_api::{PostMessage, SlackApi};
use crate::transport::SlackTransport;

/// Best-effort bot-DM nudge: never returns an error and never blocks the
/// caller's flow — a failed (or unconfigured) nudge costs only the
/// notification, the draft/picker surfaces are untouched. No-op when the bot
/// DM channel is unresolved (no `bot_token`, or startup resolution failed).
pub async fn send_nudge<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    text: &str,
    permalink: Option<&str>,
) {
    let Some(channel) = state.bot_dm_channel() else {
        tracing::debug!("no bot DM channel; skipping the notification nudge");
        return;
    };
    let mut body = format!("🔔 {text}");
    if let Some(link) = permalink {
        body.push_str(&format!(" <{link}|スレッドを開く>"));
    }
    if let Err(e) = api
        .chat_post_message_bot(&PostMessage {
            channel: &channel,
            text: &body,
            thread_ts: None,
            unfurl_links: Some(false),
            blocks: None,
        })
        .await
    {
        tracing::warn!(error = %e, "could not send the bot notification nudge");
    }
}
