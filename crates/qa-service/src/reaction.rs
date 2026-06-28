//! Slack reaction_added → GitHub Project Inbox DraftIssue.
//! Only fires when reaction == [qa_service].reaction_trigger.

use crate::error::QaError;
use crate::gh_inbox::GhInboxClient;
use crate::slack::SlackClient;
use std::sync::Arc;

pub struct ReactionCtx {
    pub slack: Arc<dyn SlackClient>,
    pub inbox: Arc<GhInboxClient>,
    pub project_node_id: String,
    pub trigger_emoji: String,
}

pub async fn handle_reaction(
    ctx: &ReactionCtx,
    channel: &str,
    item_ts: &str,
    reaction: &str,
) -> Result<Option<String>, QaError> {
    if reaction != ctx.trigger_emoji {
        return Ok(None);
    }
    let msgs = ctx.slack.replies(channel, item_ts).await?;
    let original = msgs.iter().find(|m| m.ts == item_ts).ok_or_else(|| {
        QaError::Slack(format!("reacted message {item_ts} not found in {channel}"))
    })?;
    let title: String = original.text.chars().take(80).collect();
    let body = format!(
        "{}\n\nSource: Slack channel {}, ts {} (reacted by user)\n",
        original.text, channel, item_ts
    );
    let id = ctx
        .inbox
        .create_draft(&ctx.project_node_id, &title, &body)
        .await?;
    Ok(Some(id))
}
