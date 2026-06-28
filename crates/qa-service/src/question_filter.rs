//! Decide whether a Slack message should be treated as a question for the
//! QA pipeline. Two trigger paths:
//!   * Mention: text contains `<@bot_user_id>` (top-level invocation)
//!   * Thread continuation: thread_ts present AND mapping already exists
//!
//! Author must be in allowed_user_ids; bot-authored messages are already
//! filtered upstream by the envelope parser.

use crate::slack::SlackMessage;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Mention,
    ThreadContinuation,
    None,
}

pub struct QuestionFilter {
    allowed_user_ids: HashSet<String>,
    bot_user_id: String,
}

impl QuestionFilter {
    pub fn new(allowed_user_ids: Vec<String>, bot_user_id: String) -> Self {
        Self {
            allowed_user_ids: allowed_user_ids.into_iter().collect(),
            bot_user_id,
        }
    }

    pub fn evaluate(&self, msg: &SlackMessage, existing_mapping: bool) -> Trigger {
        if !self.allowed_user_ids.contains(&msg.user) {
            return Trigger::None;
        }
        if msg.text.contains(&format!("<@{}>", self.bot_user_id)) {
            return Trigger::Mention;
        }
        if msg.thread_ts.is_some() && existing_mapping {
            return Trigger::ThreadContinuation;
        }
        Trigger::None
    }
}
