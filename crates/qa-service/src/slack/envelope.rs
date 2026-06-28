//! Slack Socket Mode envelope parser. The Socket Mode endpoint serves a
//! WebSocket of JSON envelopes; events_api envelopes must be ACK'd by
//! sending `{"envelope_id": "..."}` back on the same socket.

use crate::error::QaError;
use serde_json::Value;

use super::SlackMessage;

#[derive(Debug, Clone, PartialEq)]
pub enum SlackEnvelope {
    Hello,
    Disconnect { reason: String },
    EventsApi { envelope_id: String, event: SlackEvent },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlackEvent {
    Message(SlackMessage),
    ReactionAdded {
        user: String,
        channel: String,
        item_ts: String,
        reaction: String,
        event_ts: String,
        event_id: String,
    },
    Other,
}

pub fn parse(raw: &str) -> Result<SlackEnvelope, QaError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| QaError::Slack(format!("envelope parse: {e}")))?;
    match v["type"].as_str() {
        Some("hello") => Ok(SlackEnvelope::Hello),
        Some("disconnect") => Ok(SlackEnvelope::Disconnect {
            reason: v["reason"].as_str().unwrap_or("unknown").into(),
        }),
        Some("events_api") => {
            let envelope_id = v["envelope_id"].as_str()
                .ok_or_else(|| QaError::Slack("events_api missing envelope_id".into()))?
                .to_string();
            let event = parse_event(&v["payload"]["event"], &v["payload"])?;
            Ok(SlackEnvelope::EventsApi { envelope_id, event })
        }
        Some(other) => Err(QaError::Slack(format!("unknown envelope type: {other}"))),
        None => Err(QaError::Slack("envelope missing type".into())),
    }
}

fn parse_event(ev: &Value, payload: &Value) -> Result<SlackEvent, QaError> {
    let event_id = payload["event_id"].as_str().unwrap_or("").to_string();
    match ev["type"].as_str() {
        Some("message") => {
            // Ignore bot messages, message_changed/deleted subtypes — only top-level
            // user messages reach the question pipeline.
            if ev["subtype"].is_string() || ev["bot_id"].is_string() {
                return Ok(SlackEvent::Other);
            }
            let ts = ev["ts"].as_str().unwrap_or("").to_string();
            Ok(SlackEvent::Message(SlackMessage {
                channel: ev["channel"].as_str().unwrap_or("").to_string(),
                user: ev["user"].as_str().unwrap_or("").to_string(),
                text: ev["text"].as_str().unwrap_or("").to_string(),
                ts: ts.clone(),
                thread_ts: ev["thread_ts"].as_str().map(str::to_string),
                event_id,
            }))
        }
        Some("reaction_added") => Ok(SlackEvent::ReactionAdded {
            user: ev["user"].as_str().unwrap_or("").to_string(),
            channel: ev["item"]["channel"].as_str().unwrap_or("").to_string(),
            item_ts: ev["item"]["ts"].as_str().unwrap_or("").to_string(),
            reaction: ev["reaction"].as_str().unwrap_or("").to_string(),
            event_ts: ev["event_ts"].as_str().unwrap_or("").to_string(),
            event_id,
        }),
        _ => Ok(SlackEvent::Other),
    }
}
