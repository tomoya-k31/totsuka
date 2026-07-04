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
//!
//! self_mention 由来のマッピングでは素の返信で継続しない(auto モードでの
//! 公開リーク防止。継続は同僚の再メンションのみ)。

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

    pub fn evaluate(&self, msg: &SlackMessage, existing_mapping_origin: Option<&str>) -> Trigger {
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
        // self_mention 由来のスレッドは owner の素の返信では継続しない
        // (default_mode=auto での公開リーク防止)。継続は同僚の再メンション
        // (SelfMention、上のブロックで既に処理済み)のみ。
        // "owner" の明示一致で判定する — 未知の origin 値は fail-closed
        // (継続不可)に倒し、公開リークの再導入を防ぐ。
        if allowed && msg.thread_ts.is_some() && existing_mapping_origin == Some("owner") {
            return Trigger::ThreadContinuation;
        }
        Trigger::None
    }
}
