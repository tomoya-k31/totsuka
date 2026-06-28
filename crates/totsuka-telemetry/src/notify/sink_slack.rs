use super::{NotifyPayload, NotifySink, SinkError, SinkId};
use serde_json::json;
use std::time::Duration;
use totsuka_core::{NotifyKind, Secret};

pub struct SlackSink {
    webhook_url: Secret<String>,
    default_channel: String,
    client: reqwest::Client,
}

impl SlackSink {
    pub fn new(webhook_url: Secret<String>, default_channel: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            webhook_url,
            default_channel,
            client,
        }
    }
}

#[async_trait::async_trait]
impl NotifySink for SlackSink {
    fn id(&self) -> SinkId {
        SinkId::Slack
    }
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError> {
        let url = self.webhook_url.expose();
        if url.is_empty() {
            return Ok(());
        }
        let body = json!({
            "channel": self.default_channel,
            "text": format!("*[{}]* {}\n{}", kind.as_snake(), payload.title, payload.body),
            "attachments": payload.fields.iter().map(|(k, v)| {
                json!({ "title": k, "value": v, "short": true })
            }).collect::<Vec<_>>(),
        });
        let res = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SinkError::Http(e.to_string()))?;
        if !res.status().is_success() {
            return Err(SinkError::Http(format!("slack http {}", res.status())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_url_is_noop() {
        let s = SlackSink::new(Secret::new(String::new()), "#x".into());
        let r = s
            .send(NotifyKind::HumanGate1, &NotifyPayload::default())
            .await;
        assert!(r.is_ok());
    }
}
