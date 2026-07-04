//! SelfMention 回答前のチャンネル参加確保。
//! 公開: conversations.join(bot)。private: conversations.invite(user トークン、
//! メンバーである本人名義で bot を招待)。両方失敗なら DM のみで回答する。
//! private チャンネルも ID が `C` 始まりのため事前判別はせず、試行順で解決する。

use crate::slack::SlackClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEntry {
    /// bot がチャンネルに入った(or 元々居た)— エフェメラル可。
    Full,
    /// 参加手段なし — DM だけで回答する。
    DmOnly,
}

/// join → invite → DmOnly の試行フォールバック。すべて best-effort で、
/// 失敗は warn ログに落として先へ進む(呼び出し元にエラーは返さない)。
pub async fn ensure_channel_access(
    bot: &dyn SlackClient,
    user: Option<&dyn SlackClient>,
    channel: &str,
    bot_user_id: &str,
) -> ChannelEntry {
    match bot.join_channel(channel).await {
        Ok(()) => return ChannelEntry::Full,
        Err(e) => {
            tracing::debug!(error=%e, channel, "join failed; trying invite (likely private)");
        }
    }
    let Some(user) = user else {
        tracing::warn!(
            channel,
            "join failed and no user token; answering via DM only"
        );
        return ChannelEntry::DmOnly;
    };
    match user.invite_users(channel, bot_user_id).await {
        Ok(()) => ChannelEntry::Full,
        Err(e) => {
            tracing::warn!(error=%e, channel, "invite failed; answering via DM only");
            ChannelEntry::DmOnly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack::MockSlackClient;

    #[tokio::test]
    async fn public_channel_joins_directly() {
        let bot = MockSlackClient::new();
        let user = MockSlackClient::new();
        let e = ensure_channel_access(&bot, Some(&user), "C1", "UBOT").await;
        assert_eq!(e, ChannelEntry::Full);
        assert_eq!(bot.joins(), vec!["C1".to_string()]);
        assert!(user.invites().is_empty(), "join succeeded; no invite");
    }

    #[tokio::test]
    async fn private_channel_falls_back_to_invite() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let user = MockSlackClient::new();
        let e = ensure_channel_access(&bot, Some(&user), "C2", "UBOT").await;
        assert_eq!(e, ChannelEntry::Full);
        assert_eq!(user.invites(), vec![("C2".to_string(), "UBOT".to_string())]);
    }

    #[tokio::test]
    async fn both_fail_means_dm_only() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let user = MockSlackClient::new();
        user.set_fail_invite(true);
        let e = ensure_channel_access(&bot, Some(&user), "C3", "UBOT").await;
        assert_eq!(e, ChannelEntry::DmOnly);
    }

    #[tokio::test]
    async fn no_user_client_means_dm_only_on_join_failure() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let e = ensure_channel_access(&bot, None, "C4", "UBOT").await;
        assert_eq!(e, ChannelEntry::DmOnly);
    }
}
