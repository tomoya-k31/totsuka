use crate::error::QaError;
use async_trait::async_trait;

pub mod envelope;
pub mod mock;
pub mod socket;
pub mod web;

pub use mock::MockSlackClient;
pub use web::HttpSlackClient;

#[derive(Debug, Clone, PartialEq)]
pub struct SlackMessage {
    pub channel: String,
    pub user: String,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlackPostResult {
    pub ts: String,
}

#[async_trait]
pub trait SlackClient: Send + Sync + 'static {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError>;

    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError>;

    async fn conversation_history(
        &self,
        channel: &str,
        oldest: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError>;

    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError>;

    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError>;

    /// conversations.open — 質問者との DM チャンネル ID を解決する。
    /// Bot Token Scope `im:write` が必要。
    async fn open_dm(&self, user: &str) -> Result<String, QaError>;

    /// chat.getPermalink — メッセージへの永続リンク。追加スコープ不要。
    async fn permalink(&self, channel: &str, message_ts: &str) -> Result<String, QaError>;

    /// conversations.join — 公開チャンネルへ self-join(冪等)。要 channels:join。
    /// private では method_not_supported_for_channel_type / channel_not_found で失敗する。
    async fn join_channel(&self, channel: &str) -> Result<(), QaError>;

    /// conversations.invite — users をチャンネルに招待。user トークン(xoxp)の
    /// クライアントで呼び、bot を private チャンネルへ入れる用途。要 groups:write。
    /// already_in_channel は成功扱い(冪等)。
    async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError>;

    /// chat.delete — join システムメッセージの best-effort 削除に使用。
    /// user トークン(管理者)のクライアントで呼ぶ。
    async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError>;

    /// Resolve the bot's own user id (`auth.test`). Called once at startup;
    /// mention detection depends on it, so failure should abort boot.
    async fn bot_user_id(&self) -> Result<String, QaError>;
}
