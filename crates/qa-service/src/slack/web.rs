//! Slack Web API client — POST application/x-www-form-urlencoded with bot
//! token; responses are JSON with {"ok": bool, "error": "...", ...} envelope.

use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use totsuka_core::Secret;

use super::{SlackClient, SlackMessage, SlackPostResult};

pub struct HttpSlackClient {
    client: Client,
    endpoint: String,
    bot_token: Secret<String>,
}

impl HttpSlackClient {
    pub fn new(bot_token: Secret<String>, override_endpoint: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint: override_endpoint
                .unwrap_or_else(|| "https://slack.com/api".into())
                .trim_end_matches('/')
                .to_string(),
            bot_token,
        }
    }

    async fn post_form(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, QaError> {
        let url = format!("{}/{}", self.endpoint, method);
        let resp = self
            .client
            .post(&url)
            .header(
                "authorization",
                format!("Bearer {}", self.bot_token.expose()),
            )
            .header(
                "content-type",
                "application/x-www-form-urlencoded; charset=utf-8",
            )
            .form(params)
            .send()
            .await?;
        let v: Value = resp.json().await?;
        if !v["ok"].as_bool().unwrap_or(false) {
            return Err(QaError::Slack(format!(
                "{method}: {}",
                v["error"].as_str().unwrap_or("unknown")
            )));
        }
        Ok(v)
    }
}

#[derive(Deserialize)]
struct PostMessageResp {
    ts: String,
}

#[async_trait]
impl SlackClient for HttpSlackClient {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError> {
        let mut params: Vec<(&str, &str)> = vec![("channel", channel), ("text", text)];
        if let Some(t) = thread_ts {
            params.push(("thread_ts", t));
        }
        let v = self.post_form("chat.postMessage", &params).await?;
        let parsed: PostMessageResp = serde_json::from_value(v)
            .map_err(|e| QaError::Slack(format!("postMessage parse: {e}")))?;
        Ok(SlackPostResult { ts: parsed.ts })
    }

    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError> {
        let mut params: Vec<(&str, &str)> =
            vec![("channel", channel), ("user", user), ("text", text)];
        if let Some(t) = thread_ts {
            params.push(("thread_ts", t));
        }
        self.post_form("chat.postEphemeral", &params).await?;
        Ok(())
    }

    async fn conversation_history(
        &self,
        channel: &str,
        oldest: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError> {
        let limit_s = limit.to_string();
        let mut params: Vec<(&str, &str)> = vec![("channel", channel), ("limit", &limit_s)];
        if let Some(o) = oldest {
            params.push(("oldest", o));
        }
        let v = self.post_form("conversations.history", &params).await?;
        parse_messages(channel, &v)
    }

    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError> {
        let v = self
            .post_form(
                "conversations.replies",
                &[("channel", channel), ("ts", thread_ts)],
            )
            .await?;
        parse_messages(channel, &v)
    }

    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError> {
        self.post_form(
            "reactions.add",
            &[("channel", channel), ("timestamp", ts), ("name", name)],
        )
        .await?;
        Ok(())
    }

    async fn bot_user_id(&self) -> Result<String, QaError> {
        let v = self.post_form("auth.test", &[]).await?;
        v["user_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| QaError::Slack("auth.test: missing user_id".into()))
    }
}

fn parse_messages(channel: &str, v: &Value) -> Result<Vec<SlackMessage>, QaError> {
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let ts = m["ts"].as_str().unwrap_or("").to_string();
        out.push(SlackMessage {
            channel: channel.into(),
            user: m["user"].as_str().unwrap_or("").to_string(),
            text: m["text"].as_str().unwrap_or("").to_string(),
            ts: ts.clone(),
            thread_ts: m["thread_ts"].as_str().map(str::to_string),
            event_id: ts, // history msgs have no event_id; use ts for dedupe
        });
    }
    Ok(out)
}
