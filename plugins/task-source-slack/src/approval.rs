//! The approval flow (#107): `result/publish` turns an agent-generated reply
//! into a [`Draft`] presented twice — an ephemeral inside the mention's
//! thread and a persistent self-DM record — and the approve/reject
//! `block_actions` finish it. Only an approval posts to the thread, under
//! the operator's own name (user token).
//!
//! Failure posture:
//! - one presentation surface failing to post is logged and tolerated (the
//!   other still carries the buttons); both failing keeps the draft text in
//!   the error log, so the reply is never silently lost;
//! - a failed approval send keeps the draft `Pending` and tells the operator
//!   via an ephemeral notice, so the button can simply be pressed again;
//! - stale buttons (restart, TTL, eviction) degrade to an "expired" notice,
//!   and non-`Pending` drafts to an "already handled" notice — the
//!   double-send guard.

use serde_json::{Value, json};

use crate::config::SlackConfig;
use crate::draft::{Draft, DraftStatus};
use crate::pipeline::SharedState;
use crate::slack_api::{PostEphemeral, PostMessage, SlackApi, UpdateMessage};
use crate::transport::SlackTransport;

/// Slack caps a section block's text at 3000 characters; clip below that and
/// leave room for the truncation note.
const BLOCK_TEXT_LIMIT: usize = 2900;

/// `result/publish`: build a draft from the agent's `content`, store it, and
/// present it (thread ephemeral + self-DM record). `Err` is reserved for
/// requests that cannot become a draft at all (unknown task, empty reply);
/// presentation failures are logged, not returned.
pub async fn publish_draft<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    state: &SharedState,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    // Validate the content BEFORE consuming the pending entry: a rejected
    // publish must leave the coordinates in place so a retry can still land.
    let text = extract_reply(content);
    if text.is_empty() {
        return Err(format!(
            "task {task_id} published an empty result → nothing to propose as a reply"
        ));
    }
    // Publish is the task's terminal step: consume the pending entry.
    let Some(pending) = state.take_pending(task_id) else {
        return Err(format!(
            "task {task_id} has no pending Slack coordinates (plugin restarted since the \
             mention?) → the reply cannot be placed; re-trigger from a fresh mention"
        ));
    };

    let draft = Draft {
        task_id: task_id.to_string(),
        channel: pending.channel,
        reply_ts: pending.reply_ts,
        mention_ts: pending.mention_ts,
        sender_name: pending.sender_name,
        permalink: pending.permalink,
        text,
        dm_ts: None,
        status: DraftStatus::Pending,
        created_at: std::time::Instant::now(),
    };
    let draft_id = state.insert_draft(draft.clone());
    let blocks = draft_blocks(&draft, &draft_id, &config.source_name);

    // Surface 1: the ephemeral inside the mention's thread (operator-only).
    let ephemeral = api
        .chat_post_ephemeral(&PostEphemeral {
            channel: &draft.channel,
            user: &config.target_user_id,
            text: "返信案が届きました。承認すると本人名義でスレッドに返信します。",
            thread_ts: Some(&draft.reply_ts),
            blocks: Some(blocks.clone()),
        })
        .await;
    if let Err(e) = &ephemeral {
        tracing::warn!(task_id, draft_id, error = %e, "could not post the in-thread draft \
             ephemeral; the self-DM record still carries the buttons");
    }

    // Surface 2: the self-DM record (survives restarts as plain text).
    let dm = match state.self_dm_channel() {
        Some(dm_channel) => {
            let posted = api
                .chat_post_message(&PostMessage {
                    channel: &dm_channel,
                    text: &format!(
                        "{} さんへの返信案が届きました（task {task_id}）",
                        draft.sender_name
                    ),
                    thread_ts: None,
                    unfurl_links: Some(false),
                    blocks: Some(blocks),
                })
                .await;
            match posted {
                Ok(ts) => {
                    state.set_draft_dm_ts(&draft_id, ts);
                    Ok(())
                }
                Err(e) => {
                    tracing::warn!(task_id, draft_id, error = %e, "could not post the self-DM \
                         draft record; the in-thread ephemeral still carries the buttons");
                    Err(())
                }
            }
        }
        None => {
            tracing::warn!(
                task_id,
                draft_id,
                "self-DM channel unknown (startup resolution \
                 failed); skipping the draft record"
            );
            Err(())
        }
    };

    if ephemeral.is_err() && dm.is_err() {
        // Neither surface exists: the draft has no buttons anywhere. Keep the
        // full text in the log so the reply is recoverable by hand.
        tracing::error!(
            task_id,
            draft_id,
            draft_text = %draft.text,
            "both draft presentations failed; the reply is only recoverable from this log"
        );
    }
    Ok(())
}

/// An approve/reject `block_actions` press (`value` = draft id).
pub async fn handle_approval_action<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    source_name: &str,
    payload: &Value,
    action_id: &str,
    draft_id: &str,
    response_url: Option<&str>,
) {
    let Some(draft) = state.draft(draft_id) else {
        // Restart, TTL expiry, or eviction: the button outlived its draft.
        tracing::info!(draft_id, action_id, "button pressed for an unknown draft");
        notice(
            api,
            response_url,
            "この下書きは期限切れです（再起動などで失われた可能性があります）。\
             必要なら新しいメンションから再実行してください。",
        )
        .await;
        return;
    };
    if draft.status != DraftStatus::Pending {
        // The double-send guard: a second press on either surface.
        tracing::info!(draft_id, action_id, ?draft.status, "draft already handled");
        let state_label = match draft.status {
            DraftStatus::Sent => "✅ 送信済み",
            _ => "❌ 却下済み",
        };
        notice(
            api,
            response_url,
            &format!("この下書きは処理済みです（{state_label}）。二重送信は行われません。"),
        )
        .await;
        return;
    }

    let status = match action_id {
        "approve_reply" => {
            let posted = api
                .chat_post_message(&PostMessage {
                    channel: &draft.channel,
                    text: &draft.text,
                    thread_ts: Some(&draft.reply_ts),
                    unfurl_links: None,
                    blocks: None,
                })
                .await;
            if let Err(e) = posted {
                // Keep the draft Pending: the buttons stay live, so the
                // operator can retry once the cause (archive, permission,
                // network) is gone.
                tracing::warn!(draft_id, error = %e, "approved reply could not be posted; \
                     draft stays pending for a retry");
                notice(
                    api,
                    response_url,
                    &format!(
                        "返信の送信に失敗しました: {e}\n下書きは残っています。\
                         原因を解消してからもう一度ボタンを押してください。"
                    ),
                )
                .await;
                return;
            }
            tracing::info!(draft_id, task_id = %draft.task_id, "approved reply posted");
            DraftStatus::Sent
        }
        _ => {
            tracing::info!(draft_id, task_id = %draft.task_id, "draft rejected");
            DraftStatus::Rejected
        }
    };
    state.set_draft_status(draft_id, status);

    // Re-render both surfaces in their final state (✅/❌, no buttons).
    let finalized = Draft { status, ..draft };
    let blocks = draft_blocks(&finalized, draft_id, source_name);
    if let Some(url) = response_url {
        let body = json!({
            "replace_original": true,
            "text": final_fallback(status),
            "blocks": blocks.clone(),
        });
        if let Err(e) = api.post_response_url(url, body).await {
            tracing::warn!(draft_id, error = %e, "could not rewrite the pressed draft view");
        }
    }
    // The self-DM record — unless the press came from it (then the
    // response_url rewrite above already covered it).
    let pressed_in_dm = press_channel(payload) == state.self_dm_channel().as_deref();
    if let Some(dm_ts) = &finalized.dm_ts
        && let Some(dm_channel) = state.self_dm_channel()
        && !pressed_in_dm
    {
        let update = UpdateMessage {
            channel: &dm_channel,
            ts: dm_ts,
            text: final_fallback(status),
            blocks: Some(blocks),
        };
        if let Err(e) = api.chat_update(&update).await {
            tracing::warn!(draft_id, error = %e, "could not update the self-DM draft record");
        }
    }
}

/// The channel a `block_actions` press happened in.
fn press_channel(payload: &Value) -> Option<&str> {
    payload
        .pointer("/container/channel_id")
        .or_else(|| payload.pointer("/channel/id"))
        .and_then(Value::as_str)
}

/// Post an operator-only notice next to the pressed button, keeping the
/// original message intact (best-effort, like every `response_url` write).
async fn notice<T: SlackTransport>(api: &SlackApi<T>, response_url: Option<&str>, text: &str) {
    let Some(url) = response_url else { return };
    let body = json!({
        "replace_original": false,
        "response_type": "ephemeral",
        "text": text,
    });
    if let Err(e) = api.post_response_url(url, body).await {
        tracing::warn!(error = %e, "could not post the draft notice");
    }
}

/// The notification-fallback text of a finalized draft view.
fn final_fallback(status: DraftStatus) -> &'static str {
    match status {
        DraftStatus::Sent => "✅ 返信を送信しました",
        _ => "❌ 返信案を却下しました",
    }
}

/// The Block Kit rendering of a draft: detection header, reply text,
/// then — depending on status — the approve/reject buttons or the final
/// ✅/❌ state, plus a context footer (draft id / source).
fn draft_blocks(draft: &Draft, draft_id: &str, source_name: &str) -> Value {
    let mut header = format!(
        "📝 *{}* さんからのメンションへの返信案です。",
        draft.sender_name
    );
    if let Some(link) = &draft.permalink {
        header.push_str(&format!(" <{link}|元メッセージを開く>"));
    }

    let mut blocks = vec![
        json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": header },
        }),
        json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": clipped(&draft.text) },
        }),
    ];
    match draft.status {
        DraftStatus::Pending => blocks.push(json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "action_id": "approve_reply",
                    "style": "primary",
                    "text": { "type": "plain_text", "text": "承認して返信" },
                    "value": draft_id,
                    "confirm": {
                        "title": { "type": "plain_text", "text": "返信を送信" },
                        "text": {
                            "type": "mrkdwn",
                            "text": "この返信案を *本人名義* でスレッドに送信します。よろしいですか？"
                        },
                        "confirm": { "type": "plain_text", "text": "送信する" },
                        "deny": { "type": "plain_text", "text": "やめる" }
                    }
                },
                {
                    "type": "button",
                    "action_id": "reject_reply",
                    "style": "danger",
                    "text": { "type": "plain_text", "text": "却下" },
                    "value": draft_id,
                }
            ]
        })),
        DraftStatus::Sent => blocks.push(json!({
            "type": "context",
            "elements": [{
                "type": "mrkdwn",
                "text": "✅ *送信済み* — 本人名義でスレッドに返信しました"
            }]
        })),
        DraftStatus::Rejected => blocks.push(json!({
            "type": "context",
            "elements": [{
                "type": "mrkdwn",
                "text": "❌ *却下済み* — 返信は送信されていません"
            }]
        })),
    }
    blocks.push(json!({
        "type": "context",
        "elements": [{
            "type": "mrkdwn",
            "text": format!("draft: {draft_id} · source: {source_name}"),
        }]
    }));
    Value::Array(blocks)
}

/// Clip `text` to Slack's section-block limit, noting that approval still
/// sends the full text.
fn clipped(text: &str) -> String {
    if text.chars().count() <= BLOCK_TEXT_LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(BLOCK_TEXT_LIMIT).collect();
    format!("{head}\n…（表示上省略。承認時は全文が送信されます）")
}

/// The agent's published content is its accumulated plan-mode output, which
/// can carry log-ish noise around the actual reply. Trim noise lines
/// defensively from both *edges* only — never from the middle, where a reply
/// could legitimately quote a log — and fall back to the whole trimmed
/// content if that would erase everything.
fn extract_reply(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut start = 0;
    let mut end = lines.len();
    while start < end && is_noise_line(lines[start]) {
        start += 1;
    }
    while end > start && is_noise_line(lines[end - 1]) {
        end -= 1;
    }
    let core = lines[start..end].join("\n").trim().to_string();
    if core.is_empty() {
        content.trim().to_string()
    } else {
        core
    }
}

/// Whether a line looks like process noise rather than reply prose: blank,
/// a log-level prefix (`INFO:`, `[WARN]`, …), or an ISO-date prefix.
fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let bare = trimmed.strip_prefix('[').unwrap_or(trimmed);
    for level in ["INFO", "WARN", "WARNING", "ERROR", "DEBUG", "TRACE"] {
        if let Some(rest) = bare.strip_prefix(level)
            && rest.starts_with([' ', ':', ']'])
        {
            return true;
        }
    }
    starts_with_iso_date(bare)
}

/// `YYYY-MM-DD…` — the shape of a timestamped log line.
fn starts_with_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_reply_trims_log_noise_from_the_edges_only() {
        let content = "\
2026-07-15T10:00:00Z starting agent
[INFO] repository cloned
デプロイ失敗の原因は環境変数の欠落です。

ERROR: と出ていた行は 2026-07-01 のリリースが原因でした。
`.env.example` を参照してください。
DEBUG: shutting down
";
        let reply = extract_reply(content);
        assert!(reply.starts_with("デプロイ失敗の原因"), "{reply}");
        assert!(reply.ends_with("を参照してください。"), "{reply}");
        // Interior lines survive even when they quote log-ish text.
        assert!(reply.contains("ERROR: と出ていた行は"), "{reply}");
    }

    #[test]
    fn extract_reply_falls_back_when_everything_looks_like_noise() {
        let content = "INFO: only logs here\nWARN: nothing else";
        assert_eq!(extract_reply(content), content.trim());
        assert_eq!(extract_reply("   \n\n"), "");
    }

    #[test]
    fn clipped_notes_the_truncation() {
        let short = "短い返信";
        assert_eq!(clipped(short), short);
        let long = "あ".repeat(BLOCK_TEXT_LIMIT + 1);
        let clip = clipped(&long);
        assert!(clip.contains("省略"), "{clip}");
        assert!(clip.chars().count() < long.chars().count() + 40);
    }

    #[test]
    fn press_channel_reads_container_then_channel() {
        let payload = json!({ "container": { "channel_id": "D1" } });
        assert_eq!(press_channel(&payload), Some("D1"));
        let payload = json!({ "channel": { "id": "C1" } });
        assert_eq!(press_channel(&payload), Some("C1"));
        assert_eq!(press_channel(&json!({})), None);
    }
}
