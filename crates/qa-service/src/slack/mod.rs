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
}
