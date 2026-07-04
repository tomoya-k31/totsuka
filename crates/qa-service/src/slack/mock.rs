use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MockState {
    posts: Vec<(
        String,
        Option<String>,
        String,
        String, /* returned ts */
    )>,
    ephemerals: Vec<(String, String, Option<String>, String)>,
    reactions: Vec<(String, String, String)>,
    history: HashMap<String, Vec<SlackMessage>>,
    replies: HashMap<(String, String), Vec<SlackMessage>>,
    next_post_ts: u64,
    fail_open_dm: bool,
    fail_permalink: bool,
    joins: Vec<String>,
    invites: Vec<(String, String)>,
    deletes: Vec<(String, String)>,
    fail_join: bool,
    fail_invite: bool,
    fail_delete: bool,
}

pub struct MockSlackClient {
    state: Mutex<MockState>,
}

impl Default for MockSlackClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSlackClient {
    pub fn new() -> Self {
        let s = MockState {
            next_post_ts: 17_500_000_000,
            ..MockState::default()
        };
        Self {
            state: Mutex::new(s),
        }
    }
    pub fn set_history(&self, channel: &str, msgs: Vec<SlackMessage>) {
        self.state
            .lock()
            .unwrap()
            .history
            .insert(channel.into(), msgs);
    }
    pub fn set_replies(&self, channel: &str, thread_ts: &str, msgs: Vec<SlackMessage>) {
        self.state
            .lock()
            .unwrap()
            .replies
            .insert((channel.into(), thread_ts.into()), msgs);
    }
    pub fn posts(&self) -> Vec<(String, Option<String>, String, String)> {
        self.state.lock().unwrap().posts.clone()
    }
    pub fn ephemerals(&self) -> Vec<(String, String, Option<String>, String)> {
        self.state.lock().unwrap().ephemerals.clone()
    }
    pub fn reactions(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().reactions.clone()
    }
    pub fn set_fail_open_dm(&self, fail: bool) {
        self.state.lock().unwrap().fail_open_dm = fail;
    }
    pub fn set_fail_permalink(&self, fail: bool) {
        self.state.lock().unwrap().fail_permalink = fail;
    }
    pub fn joins(&self) -> Vec<String> {
        self.state.lock().unwrap().joins.clone()
    }
    pub fn invites(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().invites.clone()
    }
    pub fn deletes(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().deletes.clone()
    }
    pub fn set_fail_join(&self, fail: bool) {
        self.state.lock().unwrap().fail_join = fail;
    }
    pub fn set_fail_invite(&self, fail: bool) {
        self.state.lock().unwrap().fail_invite = fail;
    }
    pub fn set_fail_delete(&self, fail: bool) {
        self.state.lock().unwrap().fail_delete = fail;
    }
}

#[async_trait]
impl SlackClient for MockSlackClient {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError> {
        let mut s = self.state.lock().unwrap();
        s.next_post_ts += 1;
        let ts = format!("{}.000000", s.next_post_ts);
        s.posts.push((
            channel.into(),
            thread_ts.map(str::to_string),
            text.into(),
            ts.clone(),
        ));
        Ok(SlackPostResult { ts })
    }
    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError> {
        self.state.lock().unwrap().ephemerals.push((
            channel.into(),
            user.into(),
            thread_ts.map(str::to_string),
            text.into(),
        ));
        Ok(())
    }
    async fn conversation_history(
        &self,
        channel: &str,
        _oldest: Option<&str>,
        _limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .history
            .get(channel)
            .cloned()
            .unwrap_or_default())
    }
    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .replies
            .get(&(channel.into(), thread_ts.into()))
            .cloned()
            .unwrap_or_default())
    }
    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError> {
        self.state
            .lock()
            .unwrap()
            .reactions
            .push((channel.into(), ts.into(), name.into()));
        Ok(())
    }
    async fn open_dm(&self, user: &str) -> Result<String, QaError> {
        if self.state.lock().unwrap().fail_open_dm {
            return Err(QaError::Slack("conversations.open: missing_scope".into()));
        }
        Ok(format!("D_{user}"))
    }

    async fn permalink(&self, channel: &str, message_ts: &str) -> Result<String, QaError> {
        if self.state.lock().unwrap().fail_permalink {
            return Err(QaError::Slack(
                "chat.getPermalink: message_not_found".into(),
            ));
        }
        Ok(format!(
            "https://mock.slack/archives/{channel}/p{}",
            message_ts.replace('.', "")
        ))
    }

    async fn join_channel(&self, channel: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_join {
            return Err(QaError::Slack(
                "conversations.join: method_not_supported_for_channel_type".into(),
            ));
        }
        s.joins.push(channel.into());
        Ok(())
    }

    async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_invite {
            return Err(QaError::Slack("conversations.invite: missing_scope".into()));
        }
        s.invites.push((channel.into(), users.into()));
        Ok(())
    }

    async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_delete {
            return Err(QaError::Slack("chat.delete: cant_delete_message".into()));
        }
        s.deletes.push((channel.into(), ts.into()));
        Ok(())
    }

    async fn bot_user_id(&self) -> Result<String, QaError> {
        Ok("UBOTMOCK".into())
    }
}
