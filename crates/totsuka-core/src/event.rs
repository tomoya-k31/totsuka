use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Github,
    Slack,
    Internal,
}

/// 内部表現 (型安全な domain 層)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    pub event_key: String,
    pub source: Source,
    #[serde(rename = "type")]
    pub event_type: String, // 例: "github.status_changed"
    pub payload: serde_json::Value,
}

/// bus に流す envelope (spec §7 bus envelope)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_key: String,
    pub source: Source,
    #[serde(rename = "type")]
    pub event_type: String,
    pub published_at: DateTime<Utc>,
    pub trace_id: Option<String>,
    pub payload: serde_json::Value,
}

impl EventEnvelope {
    pub fn from_domain(
        e: DomainEvent,
        published_at: DateTime<Utc>,
        trace_id: Option<String>,
    ) -> Self {
        Self {
            event_key: e.event_key,
            source: e.source,
            event_type: e.event_type,
            published_at,
            trace_id,
            payload: e.payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn envelope_roundtrip() {
        let de = DomainEvent {
            event_key: "gh:delivery:d1".into(),
            source: Source::Github,
            event_type: "github.status_changed".into(),
            payload: serde_json::json!({"to_status": "design"}),
        };
        let ts = Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap();
        let env = EventEnvelope::from_domain(de, ts, Some("trace-1".into()));
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"github.status_changed\""));
        let parsed: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.event_key, "gh:delivery:d1");
        assert_eq!(parsed.source, Source::Github);
    }
}
