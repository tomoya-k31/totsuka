//! The bot-DM notification nudge (#305).
//!
//! Both presentation surfaces the plugin uses — the in-thread ephemeral and
//! the self-DM record — generate **no** Slack notification: ephemerals never
//! notify, and a self-DM posted under the operator's own name is their own
//! message. So the operator misses drafts and pickers unless they happen to
//! be looking. When a `bot_token` is configured, a short bot→operator DM
//! carries the native push/badge instead (desktop and mobile), while every
//! real post stays on the user token.
//!
//! A draft nudge additionally carries the reply text as `log_blocks` (#456):
//! the ephemeral is transient, so without a copy here the bot DM only ever
//! says a draft *existed* — once the ephemeral is gone, nothing in the feed
//! answers "what was it about to send?". The copy is a log, not a surface:
//! no buttons, and never rewritten on approve/reject (the self-DM record
//! stays the one finalized ✅/❌ audit trail, per ADR-0021).

use serde_json::{Value, json};

use crate::pipeline::SharedState;
use crate::slack_api::{PostMessage, SlackApi};
use crate::transport::SlackTransport;

/// Best-effort bot-DM nudge: never returns an error and never blocks the
/// caller's flow — a failed (or unconfigured) nudge costs only the
/// notification, the draft/picker surfaces are untouched. No-op when the bot
/// DM channel is unresolved (no `bot_token`, or startup resolution failed).
///
/// `log_blocks` (Block Kit blocks) are appended below the nudge line in the
/// same message — one message, one notification (#456). `None` keeps the
/// plain one-line nudge (the picker path).
pub async fn send_nudge<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    text: &str,
    permalink: Option<&str>,
    log_blocks: Option<Vec<Value>>,
) {
    let Some(channel) = state.bot_dm_channel() else {
        tracing::debug!("no bot DM channel; skipping the notification nudge");
        return;
    };
    let mut body = format!("🔔 {text}");
    if let Some(link) = permalink {
        body.push_str(&format!(" <{link}|スレッドを開く>"));
    }
    // With blocks present `text` degrades to the notification fallback, so
    // the nudge line must be replicated as the leading block to stay visible.
    let blocks = log_blocks.map(|extra| {
        let mut all = vec![json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": body.clone() },
        })];
        all.extend(extra);
        Value::Array(all)
    });
    if let Err(e) = api
        .chat_post_message_bot(&PostMessage {
            channel: &channel,
            text: &body,
            thread_ts: None,
            unfurl_links: Some(false),
            blocks,
        })
        .await
    {
        tracing::warn!(error = %e, "could not send the bot notification nudge");
    }
}
