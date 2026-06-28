use super::{NotifyPayload, NotifySink, SinkError, SinkId};
use totsuka_core::NotifyKind;

pub struct LogSink;

#[async_trait::async_trait]
impl NotifySink for LogSink {
    fn id(&self) -> SinkId {
        SinkId::Log
    }
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError> {
        tracing::warn!(
            target: "notify",
            kind = kind.as_snake(),
            title = %payload.title,
            body  = %payload.body,
            link  = ?payload.link,
            "notification"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_sink_sends_notification() {
        let sink = LogSink;
        let payload = NotifyPayload {
            title: "test title".to_string(),
            body: "test body".to_string(),
            ..Default::default()
        };
        let result = sink.send(NotifyKind::HumanGate1, &payload).await;
        assert!(result.is_ok());
    }
}
