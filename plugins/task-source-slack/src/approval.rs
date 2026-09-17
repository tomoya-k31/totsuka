//! The approval flow (#107): `result/publish` turns an agent-generated reply
//! (mechanically prefixed with a `<@sender_id>` mention of the asker) into a
//! [`Draft`] presented **once** — an ephemeral inside the mention's thread —
//! and the approve/reject `block_actions` finish it. It used to be two
//! surfaces (the thread plus a self-DM record); [ADR-0074] retired the second
//! because two button surfaces had to be kept in step and drifted in practice.
//! In the draft flow only an approval posts to the thread; a
//! `result/publish` carrying `delivery = direct` (#548, ADR-0057) skips the
//! draft and posts immediately. Either way the post is under the operator's
//! own name (user token).
//!
//! Failure posture:
//! - the one surface failing to post keeps the draft text in the error log
//!   (the only way back) and **sends no nudge** — pointing the operator at
//!   buttons that were never posted is worse than silence;
//! - a failed approval send keeps the draft `Pending` and tells the operator
//!   via an ephemeral notice, so the button can simply be pressed again;
//! - stale buttons (restart, TTL, eviction) degrade to an "expired" notice —
//!   posted inside the original mention thread when the button value carries
//!   the thread coordinates (#121), at the pressed surface otherwise;
//! - a press on a non-`Pending` draft is the double-send guard, and it
//!   **repaints** the pressed surface to the final state rather than only
//!   answering: reaching that branch is evidence the buttons are still up,
//!   and `block_actions` arrive at-least-once through the Event Gateway, so a
//!   redelivery lands there with nobody having pressed twice.

use serde_json::{Value, json};

use crate::config::SlackConfig;
use crate::draft::{Draft, DraftStatus};
use crate::pipeline::SharedState;
use crate::slack_api::{PostEphemeral, PostMessage, SlackApi};
use crate::transport::SlackTransport;

/// Slack caps a section block's text at 3000 characters; clip below that and
/// leave room for the truncation note.
const BLOCK_TEXT_LIMIT: usize = 2900;

/// Slack caps the cumulative text of all `markdown` blocks in one payload at
/// 12,000 characters. Compared against the byte length, which over-counts
/// multibyte text relative to any unit Slack could be counting in (bytes ≥
/// UTF-16 units ≥ characters) — so a text that passes here can never be the
/// one Slack rejects for size, only fall back earlier than strictly needed.
const MARKDOWN_BLOCK_LIMIT: usize = 12_000;

/// Who a direct post goes out as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostAs {
    /// The operator's own name (user token) — `delivery = direct` on a
    /// mention-driven workflow (#548, ADR-0057). The gate changed, the
    /// identity did not.
    Operator,
    /// The bot (bot token) — a channel watch result (#617, ADR-0068).
    ///
    /// A watch fires on someone *posting*, so its result is not the operator
    /// answering anyone; posting it under their name would put automated
    /// messages out in their voice, which is the thing the approval gate
    /// exists to prevent. The bot must be a member of the watched channel, or
    /// Slack answers `not_in_channel`.
    Bot,
}

/// `result/publish` without a draft: post the reply into the thread
/// immediately, no buttons.
///
/// The mechanical `<@sender>` mention prefix is kept so the person who raised
/// the task is notified, exactly as an approved draft would have — for a
/// watch that is whoever posted the clip.
///
/// **The pending coordinates are never consumed** ([ADR-0078]). Since #242 a
/// task is a *conversation* and `Done` is reversible ([ADR-0015]): a message
/// that arrives while the agent is working requeues the conversation once that
/// dispatch ends, so one conversation reaches `result/publish` once per run.
/// Taking the entry therefore made the **first** publish the only one that
/// could land — every later run failed with "no pending Slack coordinates",
/// a failure the message then blamed on a plugin restart that had not
/// happened. On failure too the entry stays put and the error goes back to
/// the Orchestrator, whose publish-failure path keeps the task's worktree.
///
/// [ADR-0015]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0015-conversation-task-identity.md
/// [ADR-0078]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0078-pending-coordinates-outlive-publish.md
pub async fn publish_direct<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    task_id: &str,
    content: &str,
    post_as: PostAs,
    operator_user_id: &str,
) -> Result<(), String> {
    // Peek, never take: see above.
    let Some(pending) = state.pending(task_id) else {
        return Err(format!(
            "task {task_id} has no pending Slack coordinates (plugin restart, FIFO \
             eviction, or a rolled-back delivery) → the reply cannot be placed; \
             re-trigger from a fresh mention"
        ));
    };
    let text = sanitize_reply(content, post_as, operator_user_id, &pending.sender_id);
    if text.is_empty() {
        return Err(format!(
            "task {task_id} published an empty result → nothing to post as a reply"
        ));
    }
    let text = format!("<@{}> {text}", pending.sender_id);
    let message = PostMessage {
        channel: &pending.channel,
        text: &text,
        thread_ts: Some(&pending.reply_ts),
        unfurl_links: None,
        // Same rendering as an approved draft (#454, ADR-0046): the agent
        // writes GFM, and without the `markdown` block, headings and tables
        // render broken. The delivery mode changes the *gate*, never how the
        // same reply looks — a direct post that renders worse than an
        // approved one would punish exactly the workflows trusted enough to
        // skip approval. Oversized replies fall back to bare `text`, also as
        // the approve path does.
        blocks: reply_markdown_block(&text).map(|b| Value::Array(vec![b])),
    };
    let posted = match post_as {
        PostAs::Operator => api.chat_post_message(&message).await.map(|_| ()),
        PostAs::Bot => api.chat_post_message_bot(&message).await.map(|_| ()),
    };
    posted.map_err(|e| match post_as {
        PostAs::Bot => format!(
            "the watch result could not be posted as the bot: {e} → if this is \
             `not_in_channel`, invite the bot to the watched channel"
        ),
        PostAs::Operator => format!("direct reply could not be posted: {e}"),
    })?;
    Ok(())
}

/// `result/publish`: build a draft from the agent's `content`, store it, and
/// present it (the thread ephemeral — the one surface). `Err` is reserved for
/// requests that cannot become a draft at all (unknown task, empty reply);
/// a presentation failure is logged, not returned.
pub async fn publish_draft<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    state: &SharedState,
    task_id: &str,
    content: &str,
) -> Result<(), String> {
    // **Peeked, never taken** — the same rule `publish_direct` states in full
    // above: a conversation can be dispatched more than once since #242, so
    // publish is not its terminal step and consuming here would strand the
    // reply of every run but the first. The draft carries its own copy of the
    // coordinates, so the entry left behind is not a second source of truth
    // for this draft — it is what the *next* run of the conversation reads.
    let Some(pending) = state.pending(task_id) else {
        return Err(format!(
            "task {task_id} has no pending Slack coordinates (plugin restart, FIFO \
             eviction, or a rolled-back delivery) → the reply cannot be placed; \
             re-trigger from a fresh mention"
        ));
    };
    let text = sanitize_reply(
        content,
        PostAs::Operator,
        &config.target_user_id,
        &pending.sender_id,
    );
    if text.is_empty() {
        return Err(format!(
            "task {task_id} published an empty result → nothing to propose as a reply"
        ));
    }
    // Mechanically (not LLM-authored) prefix a mention of the asker, so the
    // reply notifies them like a normal Slack reply would.
    let text = format!("<@{}> {text}", pending.sender_id);

    let draft = Draft {
        task_id: task_id.to_string(),
        channel: pending.channel,
        reply_ts: pending.reply_ts,
        mention_ts: pending.mention_ts,
        sender_name: pending.sender_name,
        permalink: pending.permalink,
        text,
        status: DraftStatus::Pending,
        created_at: std::time::SystemTime::now(),
    };
    let draft_id = state.insert_draft(draft.clone());
    let blocks = draft_blocks(&draft, &draft_id, &config.source_name, Surface::Message);

    // **The only surface.** The self-DM record used to carry a second copy of
    // these buttons (#107); it was retired because two button surfaces had to
    // be kept in step and drifted in practice — a press cleared one and left
    // the other live, so the operator pressed again and got "already handled"
    // with the buttons still sitting there. One surface cannot disagree with
    // itself.
    let ephemeral = api
        .chat_post_ephemeral(&PostEphemeral {
            channel: &draft.channel,
            user: &config.target_user_id,
            text: "返信案が届きました。承認すると本人名義でスレッドに返信します。",
            thread_ts: Some(&draft.reply_ts),
            blocks: Some(blocks),
        })
        .await;

    if let Err(e) = &ephemeral {
        // Nowhere to press. Keep the full text in the log so the reply is
        // recoverable by hand — the bot DM's copy is clipped for preview.
        tracing::error!(
            task_id,
            draft_id,
            error = %e,
            draft_text = %draft.text,
            "the draft ephemeral could not be posted; the reply is only \
             recoverable from this log"
        );
    } else {
        // The ephemeral generates no Slack notification and never has — so
        // the bot DM is what makes the draft noticeable (#305). The reply
        // text rides along as a buttonless log (#456), which matters more now
        // that it is the only durable trace: the ephemeral is transient, and
        // once it is gone nothing else answers "what was it about to send?".
        // The nudge's `ts` is kept: a press records the ✅/❌ there and then
        // deletes the ephemeral (ADR-0074 amendment 7). Without a nudge there
        // is nowhere to record it, so the press keeps today's repaint.
        if let Some(nudge_ts) = crate::notify::send_nudge(
            api,
            state,
            &format!("{} さんへの返信案が届きました", draft.sender_name),
            draft.permalink.as_deref(),
            // The nudge is a `chat.postMessage` DM, so the rich block is fine.
            Some(vec![reply_preview_block(
                &draft.text,
                Surface::Message,
                draft.status,
            )]),
        )
        .await
        {
            state.set_draft_nudge_ts(&draft_id, nudge_ts);
        }
    }
    Ok(())
}

/// An approve/reject `block_actions` press (`value` = `button_value` JSON,
/// or a bare draft id from a pre-#121 button).
pub async fn handle_approval_action<T: SlackTransport>(
    api: &SlackApi<T>,
    state: &SharedState,
    config: &SlackConfig,
    payload: &Value,
    action_id: &str,
    value: &str,
    response_url: Option<&str>,
) {
    let (draft_id, coords) = parse_button_value(value);
    let draft_id = draft_id.as_str();
    let Some(draft) = state.draft(draft_id) else {
        // Restart, TTL expiry, or eviction: the button outlived its draft.
        tracing::info!(draft_id, action_id, "button pressed for an unknown draft");
        let text = "この下書きは期限切れです（再起動などで失われた可能性があります）。\
                    必要なら新しいメンションから再実行してください。";
        let mut thread_notified = false;
        if let Some((channel, ts)) = &coords {
            // #121: answer inside the original mention thread, where the
            // conversation lives — not only at the surface the press came from.
            let posted = api
                .chat_post_ephemeral(&PostEphemeral {
                    channel,
                    user: &config.target_user_id,
                    text,
                    thread_ts: Some(ts),
                    blocks: None,
                })
                .await;
            match posted {
                Ok(()) => thread_notified = true,
                Err(e) => tracing::warn!(draft_id, error = %e, "could not post the expiry \
                     notice into the thread; falling back to the pressed surface"),
            }
        }
        if !thread_notified {
            // Old-format value (no coordinates) or the thread post failed.
            notice(api, response_url, text).await;
        } else if press_channel(payload) != coords.as_ref().map(|(c, _)| c.as_str()) {
            // Pressed from somewhere other than the mention's thread — a
            // button that outlived a surface this build no longer creates,
            // or one carried into another channel. Without this the press
            // would look dead there, since the ephemeral above is only
            // visible inside the thread.
            notice(
                api,
                response_url,
                "この下書きは期限切れです。元のスレッドに案内を投稿しました。",
            )
            .await;
        }
        return;
    };
    if draft.status != DraftStatus::Pending {
        // The double-send guard. **It repaints the surface rather than just
        // answering**, because a second press is evidence the buttons are
        // still there — and buttons that survive a decision keep inviting the
        // press that produced this branch. Reaching it twice is normal, not
        // exceptional: `block_actions` arrive at-least-once through the Event
        // Gateway, so a redelivery lands here with nobody having pressed
        // anything a second time.
        tracing::info!(draft_id, action_id, ?draft.status, "draft already handled");
        match response_url {
            Some(url) => {
                let body = json!({
                    "replace_original": true,
                    "text": final_fallback(draft.status),
                    "blocks": draft_blocks(
                        &draft,
                        draft_id,
                        &config.source_name,
                        Surface::ResponseUrl,
                    ),
                });
                if let Err(e) = api.post_response_url(url, body).await {
                    tracing::warn!(draft_id, error = %e, "could not repaint an already-handled draft");
                }
            }
            None => tracing::warn!(
                draft_id,
                "an already-handled draft was pressed with no response_url; \
                 its buttons stay up"
            ),
        }
        return;
    }

    let status = match action_id {
        "approve_reply" => {
            // The `markdown` block renders the agent's Markdown properly
            // (#454); `text` stays the full reply as the notification/search
            // fallback. Oversized replies post as bare `text`, as before.
            let posted = api
                .chat_post_message(&PostMessage {
                    channel: &draft.channel,
                    text: &draft.text,
                    thread_ts: Some(&draft.reply_ts),
                    unfurl_links: None,
                    blocks: reply_markdown_block(&draft.text).map(|b| Value::Array(vec![b])),
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

    // **Replace the ephemeral in place; never delete it.** Deleting used to be
    // right when a self-DM record survived to carry the ✅/❌ outcome, but that
    // record is gone (#107 retired), so erasing this one would leave a reject
    // with no trace anywhere. What stays behind is the same block set with the
    // buttons swapped for the final state.
    let finalized = Draft { status, ..draft };
    // **`response_url` is the only way back to the surface now.** The record
    // type makes it optional (`GatewayRecord.response_url`), so a delivery
    // without one is contractually legal even though Slack always sends it
    // for a message button. Acting anyway is still right — the operator
    // decided, and refusing would drop a decision that was already made
    // (for an approval the reply is posted by this point) — but it must not
    // pass for success: the buttons stay live and nothing else can clear
    // them. The double-press guard keeps a second press from re-sending.
    match response_url {
        Some(url) => {
            let body = json!({
                "replace_original": true,
                "text": final_fallback(status),
                "blocks": draft_blocks(
                    &finalized,
                    draft_id,
                    &config.source_name,
                    Surface::ResponseUrl,
                ),
            });
            if let Err(e) = api.post_response_url(url, body).await {
                tracing::warn!(draft_id, error = %e, "could not finalize the pressed draft view");
            }
        }
        None => tracing::warn!(
            draft_id,
            ?status,
            "the press carried no response_url, so the draft was decided but its \
             buttons could not be cleared; a second press is refused as handled"
        ),
    }
}

/// The approve/reject button `value`: the draft id plus the mention thread's
/// coordinates, so a stale press can still be answered inside that thread
/// (#121 — the draft itself may be long gone by press time).
fn button_value(draft_id: &str, draft: &Draft) -> String {
    json!({ "d": draft_id, "c": draft.channel, "ts": draft.reply_ts }).to_string()
}

/// Parse a button `value` into `(draft_id, thread coordinates)`. Pre-#121
/// buttons carry the bare draft id (they outlive deploys on old messages),
/// so anything that isn't `{"d": …}` JSON is taken as a draft id verbatim.
fn parse_button_value(value: &str) -> (String, Option<(String, String)>) {
    if let Ok(parsed) = serde_json::from_str::<Value>(value)
        && let Some(d) = parsed.get("d").and_then(Value::as_str)
    {
        let coords = match (
            parsed.get("c").and_then(Value::as_str),
            parsed.get("ts").and_then(Value::as_str),
        ) {
            (Some(c), Some(ts)) => Some((c.to_string(), ts.to_string())),
            _ => None,
        };
        return (d.to_string(), coords);
    }
    (value.to_string(), None)
}

/// The channel a `block_actions` press happened in.
///
/// `pub(crate)` so [`crate::gateway_contract`] can assert that the payload it
/// rebuilds out of a flattened Pub/Sub record is read by the very function
/// that reads Slack's own — rather than by a second copy of this lookup.
pub(crate) fn press_channel(payload: &Value) -> Option<&str> {
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

/// Where a block set is headed, because the two surfaces do not accept the
/// same blocks.
///
/// **`response_url` refuses the `markdown` block.** It answers HTTP 500 with
/// an **empty body** — no `ok: false`, no error code, nothing to branch on.
/// Measured live on 2026-09-15 by posting a draft's blocks twice, once each
/// way, on three separate drafts: replies of 33 / 308 / 1426 characters sent
/// 638 / 1445 / 3487 bytes with the `markdown` block (all refused) and
/// 662 / 1469 / 3511 bytes with that one block swapped for a `section` (all
/// accepted). **Every accepted payload is the larger of its pair**, and the
/// outcome does not move while the payload grows 5.5x, so this is the block
/// type and not a size limit.
///
/// `chat.postMessage` and `chat.postEphemeral` take it fine (#454, verified
/// live 2026-08-14), which is why the difference belongs to the surface rather
/// than to the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// `chat.postEphemeral` / `chat.postMessage` — the `markdown` block works.
    Message,
    /// A `response_url` write — it does not.
    ResponseUrl,
}

/// The reply as a Block Kit `markdown` block (#454). The agent writes
/// GitHub-flavored Markdown; posted as bare `text` Slack reads it as mrkdwn —
/// a different dialect — so `**bold**`, fence language tags, `[t](url)` links,
/// headings and tables all render broken. The `markdown` block accepts
/// standard Markdown and Slack translates it into `rich_text`/`table` blocks
/// server-side; `<@user>` mentions survive as real mentions (both verified
/// live against a user token, 2026-08-14).
///
/// `None` when the text could exceed the cumulative cap — the caller keeps
/// today's plain-`text` behavior, which has no practical size limit.
fn reply_markdown_block(text: &str) -> Option<Value> {
    (text.len() <= MARKDOWN_BLOCK_LIMIT).then(|| json!({ "type": "markdown", "text": text }))
}

/// The reply text as a display block: the `markdown` block when it fits **and
/// the surface accepts it**, the clipped mrkdwn section otherwise.
///
/// Shared by the draft preview surfaces and the bot-DM log (#456) so every
/// rendering of the reply makes the same markdown-vs-fallback decision. The
/// `Surface` argument is the second reason to fall back and it is not a
/// preference: a `markdown` block sent to a `response_url` fails the whole
/// write (see [`Surface`]), which on the draft surface means the buttons never
/// clear.
fn reply_preview_block(text: &str, surface: Surface, status: DraftStatus) -> Value {
    let markdown = match surface {
        Surface::Message => reply_markdown_block(text),
        Surface::ResponseUrl => None,
    };
    markdown.unwrap_or_else(|| {
        json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": clipped(text, status) },
        })
    })
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
fn draft_blocks(draft: &Draft, draft_id: &str, source_name: &str, surface: Surface) -> Value {
    let mut header = format!(
        "📝 *{}* さんからのメンションへの返信案です。",
        draft.sender_name
    );
    if let Some(link) = &draft.permalink {
        header.push_str(&format!(" <{link}|元メッセージを開く>"));
    }

    // On the message surface the preview must show what approval will send:
    // the same `markdown` block when the text fits, the same clipped mrkdwn
    // section when the approve path will fall back to a plain-`text` post.
    //
    // The `response_url` rendering cannot match it, because that surface
    // refuses the block outright. What the finalized view loses is its rich
    // rendering — and, **for a long enough reply, some of the text**: the
    // section falls back through `clipped`, which stops at
    // `BLOCK_TEXT_LIMIT` **characters**, while the `markdown` block it
    // replaces allows `MARKDOWN_BLOCK_LIMIT` **bytes**. A reply between the
    // two (over ~2,900 characters, under 12,000 bytes — so roughly 2,900 to
    // 4,000 characters of Japanese) shows in full on the message surface and
    // clipped here.
    //
    // That is still the right trade, but it is a real cost and not just a
    // cosmetic one. It buys a surface that appears at all: before this, the
    // whole write failed and the buttons stayed up. And nothing is lost that
    // the operator cannot reach — an approved reply is already in the thread
    // in full, as a real `markdown` block, and a rejected one was never going
    // anywhere. The note `clipped` appends says which of those happened.
    let mut blocks = vec![
        json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": header },
        }),
        reply_preview_block(&draft.text, surface, draft.status),
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
                    "value": button_value(draft_id, draft),
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
                    "value": button_value(draft_id, draft),
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

/// Clip `text` to Slack's section-block limit, with a note that says what
/// became of the rest — which depends on what was decided.
///
/// The old note said "承認時は全文が送信されます" unconditionally. On a
/// **rejected** surface that is simply false (nothing was sent, and nothing
/// will be), and on a sent one it is in the wrong tense. A truncation note is
/// the one place a reader looks to find out whether they are missing
/// something, so it must not answer a question the surface has already
/// settled.
fn clipped(text: &str, status: DraftStatus) -> String {
    if text.chars().count() <= BLOCK_TEXT_LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(BLOCK_TEXT_LIMIT).collect();
    let note = match status {
        DraftStatus::Pending => "表示上省略。承認時は全文が送信されます",
        DraftStatus::Sent => "表示上省略。スレッドに送信された返信は全文です",
        DraftStatus::Rejected => "表示上省略。返信は送信されていません",
    };
    format!("{head}\n…（{note}）")
}

/// The reply text to post: log noise trimmed off the edges, then the mention
/// tags the agent echoed from its prompt removed (#632), in the order the
/// mechanical `<@sender>` prefix expects.
///
/// Which tags are echoes depends on who posts. A reply going out **as the
/// operator** can never legitimately mention the operator, so that tag goes
/// wherever it sits. A reply going out **as the bot** (a watched channel,
/// #617) is another author's voice, and "ask <@operator>" is real content
/// there. For both identities the caller prefixes the asker's mention, so an
/// asker (or, as the operator, a self) tag in front of the text is an echo —
/// a third party addressed at the head ("<@X> さんに聞いてください") is not,
/// and stays.
fn sanitize_reply(
    content: &str,
    post_as: PostAs,
    operator_user_id: &str,
    sender_id: &str,
) -> String {
    let text = extract_reply(content);
    match post_as {
        PostAs::Operator => strip_leading_mentions(
            &remove_mention_of(&text, operator_user_id),
            &[sender_id, operator_user_id],
        ),
        PostAs::Bot => strip_leading_mentions(&text, &[sender_id]),
    }
}

/// Drop every `<@user>` / `<@user|label>` tag of `user_id` from `text` (#632).
///
/// The agent sees the mention it is answering in its prompt and sometimes
/// echoes the operator's own tag back into the reply — which is then posted
/// *as* that operator, so the reply mentions its own author. There is no
/// legitimate reason for a reply to carry a mention of the account posting
/// it, so the tag goes wherever it sits. One adjacent space goes with it, so
/// `foo <@U_ME> bar` reads `foo bar` rather than `foo  bar`.
///
/// Third-party mentions are left alone: "ask <@U_OTHER>" is real content.
pub(crate) fn remove_mention_of(text: &str, user_id: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("<@") {
        let (head, tail) = rest.split_at(pos);
        out.push_str(head);
        match mention_tag_len(tail) {
            Some(len) if mention_tag_user(&tail[..len]) == user_id => {
                let after = &tail[len..];
                if let Some(after) = after.strip_prefix(' ') {
                    rest = after;
                } else {
                    if out.ends_with(' ') {
                        out.pop();
                    }
                    rest = after;
                }
            }
            Some(len) => {
                out.push_str(&tail[..len]);
                rest = &tail[len..];
            }
            None => {
                out.push_str("<@");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Drop the run of mention tags of `echo_ids` a reply *starts* with (#632).
///
/// The asker's mention is prefixed mechanically by the caller, so the asker's
/// own tag in front of the agent's text is an echo of the prompt (`<@B> <@B>
/// …`), and so is the poster's. Only those ids, and only at the head: a tag
/// of anyone else — even in front — is the agent addressing someone, and a
/// mention in the middle of a sentence is content either way.
pub(crate) fn strip_leading_mentions(text: &str, echo_ids: &[&str]) -> String {
    let mut rest = text.trim_start();
    while let Some(len) = mention_tag_len(rest) {
        if !echo_ids.contains(&mention_tag_user(&rest[..len])) {
            break;
        }
        rest = rest[len..].trim_start();
    }
    rest.to_string()
}

/// Length of the mention tag `s` starts with, or `None` when `s` does not
/// start with one. A tag is `<@` + a non-empty id (ASCII alphanumerics; `_`
/// is admitted for the `U_ME`-style ids the test fixtures use) + optionally
/// `|label` + `>`, on one line.
fn mention_tag_len(s: &str) -> Option<usize> {
    let body = s.strip_prefix("<@")?;
    let id_len = body
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if id_len == 0 {
        return None;
    }
    let after_id = &body[id_len..];
    let close = if let Some(labelled) = after_id.strip_prefix('|') {
        let label_len = labelled.find(['>', '<', '\n'])?;
        if !labelled[label_len..].starts_with('>') {
            return None;
        }
        1 + label_len
    } else if after_id.starts_with('>') {
        0
    } else {
        return None;
    };
    Some(2 + id_len + close + 1)
}

/// The user id inside a tag `mention_tag_len` accepted.
fn mention_tag_user(tag: &str) -> &str {
    let body = &tag[2..tag.len() - 1];
    body.split('|').next().unwrap_or(body)
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

    fn draft_of(text: &str, status: DraftStatus) -> Draft {
        Draft {
            task_id: "slack:C1:100.0".into(),
            channel: "C1".into(),
            reply_ts: "100.0".into(),
            mention_ts: "100.1".into(),
            sender_name: "アリス".into(),
            permalink: None,
            text: text.into(),
            status,
            created_at: std::time::SystemTime::now(),
        }
    }

    /// Every block type present in `blocks`, in order.
    fn block_types(blocks: &Value) -> Vec<&str> {
        blocks
            .as_array()
            .expect("blocks is an array")
            .iter()
            .filter_map(|b| b.get("type").and_then(Value::as_str))
            .collect()
    }

    /// **Nothing headed for a `response_url` may carry a `markdown` block.**
    ///
    /// Slack refuses it there with HTTP 500 and an **empty body** — no
    /// `ok: false`, no error code — so the failure arrives as an opaque
    /// status, and on the draft surface it means the whole rewrite is lost and
    /// the approve/reject buttons never clear. That is what shipped in #684:
    /// the press repainted the ephemeral instead of deleting it, which sent
    /// `blocks` down a path that had only ever carried `delete_original`.
    ///
    /// Measured live 2026-09-15 on three drafts, both ways each: replies of
    /// 33 / 308 / 1426 characters were refused at 638 / 1445 / 3487 bytes with
    /// the `markdown` block and accepted at 662 / 1469 / 3511 bytes with a
    /// `section` in its place. **Every accepted payload is the larger of its
    /// pair**, across a 5.5x range, so it is the block type and not a size
    /// limit. Both the deciding press and the already-handled repaint behave
    /// the same way.
    ///
    /// **This test is the only thing that catches a regression here.** The
    /// transport is faked everywhere else, so a `markdown` block reaching a
    /// `response_url` passes every other test in this repo and then fails in
    /// production, silently, on a surface nobody is watching.
    #[test]
    fn response_url_payloads_never_carry_a_markdown_block() {
        for status in [
            DraftStatus::Pending,
            DraftStatus::Sent,
            DraftStatus::Rejected,
        ] {
            let draft = draft_of("**太字** と `コード`", status);
            let blocks = draft_blocks(&draft, "d1", "slack", Surface::ResponseUrl);
            assert!(
                !block_types(&blocks).contains(&"markdown"),
                "{status:?}: a markdown block would fail the whole write: {blocks}"
            );
        }
    }

    /// …and the message surface keeps it, because that is where the preview
    /// has to show what approval will actually send (#454). Dropping it
    /// everywhere would have "fixed" the 500 by regressing the preview.
    #[test]
    fn the_message_surface_still_previews_with_a_markdown_block() {
        let draft = draft_of("**太字** と `コード`", DraftStatus::Pending);
        let blocks = draft_blocks(&draft, "d1", "slack", Surface::Message);
        assert!(
            block_types(&blocks).contains(&"markdown"),
            "the preview must render what will be posted: {blocks}"
        );
    }

    /// The two surfaces otherwise agree: same header, same footer, same
    /// buttons-or-final-state. Only the reply block differs, so a future
    /// change that drops the ✅/❌ context from the `response_url` rendering —
    /// the trace ADR-0074 kept the surface *for* — fails here.
    #[test]
    fn the_surfaces_differ_only_in_how_the_reply_is_rendered() {
        let draft = draft_of("やっておきます", DraftStatus::Rejected);
        let message = draft_blocks(&draft, "d1", "slack", Surface::Message);
        let response_url = draft_blocks(&draft, "d1", "slack", Surface::ResponseUrl);

        // Compared as whole blocks, not as type names: `section` vs `section`
        // says nothing about whether the ❌ context still carries the ❌, and
        // that context is what ADR-0074 kept the surface *for*.
        let (m, r) = (
            message.as_array().expect("blocks"),
            response_url.as_array().expect("blocks"),
        );
        assert_eq!(m.len(), r.len(), "{message}\n{response_url}");
        assert_eq!(m[0], r[0], "header");
        assert_eq!(&m[2..], &r[2..], "final state and footer");
        assert_ne!(m[1], r[1], "only the reply block differs");
        // …and the one that differs differs in the way this change is about.
        assert_eq!(m[1]["type"], "markdown");
        assert_eq!(r[1]["type"], "section");
    }

    /// #632: the operator's own tag goes wherever it sits, with one adjacent
    /// space, and a third party's stays.
    #[test]
    fn the_operators_own_mention_is_removed_wherever_it_sits() {
        assert_eq!(
            remove_mention_of("<@U_ME> こんにちは", "U_ME"),
            "こんにちは"
        );
        assert_eq!(
            remove_mention_of("<@U_ME|tomoya> こんにちは", "U_ME"),
            "こんにちは"
        );
        assert_eq!(remove_mention_of("foo <@U_ME> bar", "U_ME"), "foo bar");
        assert_eq!(remove_mention_of("foo <@U_ME>", "U_ME"), "foo");
        assert_eq!(remove_mention_of("foo\n<@U_ME>\nbar", "U_ME"), "foo\n\nbar");
        assert_eq!(
            remove_mention_of("ask <@U_OTHER> first", "U_ME"),
            "ask <@U_OTHER> first"
        );
        // A look-alike id is a different user.
        assert_eq!(remove_mention_of("<@U_MEX> hi", "U_ME"), "<@U_MEX> hi");
        // Not a tag at all: left byte-for-byte.
        assert_eq!(
            remove_mention_of("a <@ b <@U_ME c", "U_ME"),
            "a <@ b <@U_ME c"
        );
    }

    /// #632: only the asker's / poster's tags at the head are echoes; a third
    /// party addressed at the head, and any mention inside a sentence, stay.
    #[test]
    fn leading_echo_mentions_are_stripped_but_addressed_ones_survive() {
        let ids = &["U_B", "U_A"];
        assert_eq!(strip_leading_mentions("<@U_B> <@U_A> 本文", ids), "本文");
        assert_eq!(strip_leading_mentions("  <@U_B|b>\n本文", ids), "本文");
        assert_eq!(
            strip_leading_mentions("本文 <@U_B> です", ids),
            "本文 <@U_B> です"
        );
        assert_eq!(strip_leading_mentions("本文", ids), "本文");
        // Addressing a third party at the head is the agent's content.
        assert_eq!(
            strip_leading_mentions("<@U_X> さんに聞いてください", ids),
            "<@U_X> さんに聞いてください"
        );
        // …even behind an echoed asker tag.
        assert_eq!(
            strip_leading_mentions("<@U_B> <@U_X> さんに聞いてください", ids),
            "<@U_X> さんに聞いてください"
        );
    }

    /// The two together are what the publish paths apply, in that order:
    /// self anywhere, then whatever tags are left in front.
    #[test]
    fn the_echoed_prefix_from_the_live_run_collapses_to_the_body() {
        let echoed = "<@U_ME> このリポジトリを確認しました。";
        let text = sanitize_reply(echoed, PostAs::Operator, "U_ME", "U_B");
        assert_eq!(text, "このリポジトリを確認しました。");
        let doubled = "<@U_B> <@U_ME> 本文";
        let text = sanitize_reply(doubled, PostAs::Operator, "U_ME", "U_B");
        assert_eq!(text, "本文");
    }

    /// The identity decides which tags are echoes: as the operator, a mention
    /// of the operator is always one; as the bot, only the leading run is.
    #[test]
    fn a_bot_post_keeps_an_inner_mention_of_the_operator() {
        let content = "<@U_ASKER> まず <@U_ME> に確認してください。";
        assert_eq!(
            sanitize_reply(content, PostAs::Operator, "U_ME", "U_ASKER"),
            "まず に確認してください。"
        );
        assert_eq!(
            sanitize_reply(content, PostAs::Bot, "U_ME", "U_ASKER"),
            "まず <@U_ME> に確認してください。"
        );
        // As the bot, a leading tag of the operator is not an echo of the
        // prefix either — it is addressed.
        assert_eq!(
            sanitize_reply("<@U_ME> 対応をお願いします", PostAs::Bot, "U_ME", "U_ASKER"),
            "<@U_ME> 対応をお願いします"
        );
    }

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
        assert_eq!(clipped(short, DraftStatus::Pending), short);
        let long = "あ".repeat(BLOCK_TEXT_LIMIT + 1);
        let clip = clipped(&long, DraftStatus::Pending);
        assert!(clip.contains("省略"), "{clip}");
        assert!(clip.chars().count() < long.chars().count() + 40);
        assert_eq!(
            clipped(short, DraftStatus::Rejected),
            short,
            "short text keeps no note"
        );
    }

    /// **The truncation note must not answer a question the surface already
    /// settled.** It said "承認時は全文が送信されます" on every surface,
    /// including a rejected one where nothing was sent and nothing will be —
    /// and the note is the one place a reader looks to find out whether they
    /// are missing something.
    #[test]
    fn the_truncation_note_says_what_became_of_the_rest() {
        let long = "あ".repeat(BLOCK_TEXT_LIMIT + 1);
        assert!(
            clipped(&long, DraftStatus::Pending).contains("承認時は全文が送信されます"),
            "a pending draft is still a proposal"
        );
        let rejected = clipped(&long, DraftStatus::Rejected);
        assert!(rejected.contains("送信されていません"), "{rejected}");
        assert!(
            !rejected.contains("承認時"),
            "nothing is going to be approved any more: {rejected}"
        );
        let sent = clipped(&long, DraftStatus::Sent);
        assert!(sent.contains("送信された返信は全文です"), "{sent}");
        assert!(!sent.contains("承認時"), "it already went out: {sent}");
    }

    #[test]
    fn button_value_round_trips_through_parse() {
        let draft = Draft {
            task_id: "C1:100.0".into(),
            channel: "C1".into(),
            reply_ts: "100.0".into(),
            mention_ts: "100.0".into(),
            sender_name: "sender".into(),
            permalink: None,
            text: "返信案".into(),
            status: DraftStatus::Pending,
            created_at: std::time::SystemTime::now(),
        };
        let value = button_value("18f3-1", &draft);
        assert_eq!(
            parse_button_value(&value),
            (
                "18f3-1".to_string(),
                Some(("C1".to_string(), "100.0".to_string()))
            )
        );
    }

    #[test]
    fn parse_button_value_accepts_the_pre_121_bare_draft_id() {
        assert_eq!(parse_button_value("ffff-1"), ("ffff-1".to_string(), None));
        // JSON without "d" is not ours either: treat it as an opaque draft id.
        assert_eq!(
            parse_button_value(r#"{"task":"x"}"#),
            (r#"{"task":"x"}"#.to_string(), None)
        );
    }

    #[test]
    fn parse_button_value_tolerates_missing_coordinates() {
        assert_eq!(
            parse_button_value(r#"{"d":"18f3-1","c":"C1"}"#),
            ("18f3-1".to_string(), None)
        );
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
