//! Decide whether a Slack message should be treated as a question for the
//! QA pipeline. Three trigger paths:
//!   * Mention: text contains `<@bot_user_id>` (top-level invocation)
//!   * SelfMention: text contains mention of `self_mention_user_id` — the
//!     author may be ANYONE except that user (colleagues asking the owner)
//!   * Thread continuation: thread_ts present AND mapping already exists
//!
//! Mention / thread continuation require the author to be in
//! allowed_user_ids; SelfMention deliberately does not. Bot-authored
//! messages are already filtered upstream by the envelope parser.

use crate::slack::SlackMessage;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Mention,
    SelfMention,
    ThreadContinuation,
    None,
}

pub struct QuestionFilter {
    allowed_user_ids: HashSet<String>,
    bot_user_id: String,
    self_mention_user_id: String,
}

impl QuestionFilter {
    pub fn new(
        allowed_user_ids: Vec<String>,
        bot_user_id: String,
        self_mention_user_id: String,
    ) -> Self {
        Self {
            allowed_user_ids: allowed_user_ids.into_iter().collect(),
            bot_user_id,
            self_mention_user_id,
        }
    }

    pub fn evaluate(&self, msg: &SlackMessage, existing_mapping: bool) -> Trigger {
        let allowed = self.allowed_user_ids.contains(&msg.user);
        if allowed && msg.text.contains(&format!("<@{}>", self.bot_user_id)) {
            return Trigger::Mention;
        }
        // SelfMention は allowed_user_ids 外の同僚が対象。自分の発言では発火しない。
        if !self.self_mention_user_id.is_empty()
            && msg.user != self.self_mention_user_id
            && msg
                .text
                .contains(&format!("<@{}>", self.self_mention_user_id))
        {
            return Trigger::SelfMention;
        }
        if allowed && msg.thread_ts.is_some() && existing_mapping {
            return Trigger::ThreadContinuation;
        }
        Trigger::None
    }
}
