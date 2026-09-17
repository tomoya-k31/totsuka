//! The bot-DM notification nudge (#305).
//!
//! The plugin's one presentation surface — the in-thread ephemeral —
//! generates **no** Slack notification; ephemerals never do. So the operator
//! misses drafts and pickers unless they happen to be looking. When a
//! `bot_token` is configured, a short bot→operator DM
//! carries the native push/badge instead (desktop and mobile), while every
//! real post stays on the user token.
//!
//! A draft nudge additionally carries the reply text as `log_blocks` (#456):
//! the ephemeral is transient, so without a copy here the bot DM only ever
//! says a draft *existed* — once the ephemeral is gone, nothing in the feed
//! answers "what was it about to send?". The copy is a log, not a surface:
//! it never carries buttons, and the bot DM is still a notification feed
//! (ADR-0021).
//!
//! **A decision is written back onto the draft nudge** (ADR-0074 amendment 7)
//! — the one exception to "never rewritten", and the reason a nudge returns
//! its `ts`. Pressing a button now *deletes* the ephemeral instead of
//! repainting it, so without this edit a rejection would leave no trace
//! anywhere: nothing is posted to the thread, and the only surface is gone.
//! The edit is a `chat.update` on the bot's own message, so it adds no second
//! notification.

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
///
/// Returns the posted message's `ts` when a nudge actually went out, so a
/// draft can record where its decision will be written ([`record_decision`]).
/// `None` covers every reason there is no such message — unconfigured,
/// unresolved DM channel, failed post — and each of them is a caller's cue to
/// keep the ephemeral instead of deleting it.
pub async fn send_nudge<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    text: &str,
    permalink: Option<&str>,
    log_blocks: Option<Vec<Value>>,
) -> Option<String> {
    let Some(channel) = state.bot_dm_channel() else {
        tracing::debug!("no bot DM channel; skipping the notification nudge");
        return None;
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
    match api
        .chat_post_message_bot(&PostMessage {
            channel: &channel,
            text: &body,
            thread_ts: None,
            unfurl_links: Some(false),
            blocks,
        })
        .await
    {
        Ok(ts) => Some(ts),
        Err(e) => {
            tracing::warn!(error = %e, "could not send the bot notification nudge");
            None
        }
    }
}

/// Rewrite a draft's nudge DM to carry the operator's decision (ADR-0074
/// amendment 7). `true` is the caller's licence to delete the ephemeral: it
/// means the outcome is recorded somewhere that outlives it.
///
/// Best-effort in the same sense as [`send_nudge`] — it never fails the
/// approval flow — but unlike it, the outcome is reported, because the caller
/// has a different surface to fall back to.
pub async fn record_decision<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    nudge_ts: &str,
    text: &str,
    blocks: Vec<Value>,
) -> bool {
    let Some(channel) = state.bot_dm_channel() else {
        // The nudge was posted (its `ts` is right here), so the channel was
        // resolved once. Losing it since means a restart with `bot_token`
        // gone — the message is unreachable, not absent.
        tracing::debug!("no bot DM channel; the decision cannot be recorded on the nudge");
        return false;
    };
    let mut all = vec![json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": text },
    })];
    all.extend(blocks);
    match api
        .chat_update_bot(&crate::slack_api::UpdateMessage {
            channel: &channel,
            ts: nudge_ts,
            text,
            blocks: Some(Value::Array(all)),
        })
        .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "could not record the decision on the nudge DM");
            false
        }
    }
}
