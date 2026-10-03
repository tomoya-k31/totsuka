//! The reject modal, opened from here (ADR-0112).
//!
//! A modal can only be opened with the press's `trigger_id`, which Slack
//! expires **3 seconds** after the press. A press reaches totsuka through
//! Pub/Sub, whose consumer backs off to as much as 20 seconds between empty
//! pulls, so totsuka cannot be the one to open it. This process sees the press
//! inside that window, so it opens the modal with the operator's bot token
//! and does not publish the press at all. Any failure — no token, an expired
//! trigger, Slack down — falls back to publishing the press as before, which
//! totsuka answers by rejecting on the spot: the alternative reply is lost,
//! the rejection is not.
//!
//! The modal's ids are a contract with `task-source-slack`'s
//! `approval::handle_view_submission`, which reads them back out of the
//! submission: `callback_id`, `alt_reply`/`alt_text` and `send_alt`/`send`.

use std::time::Duration;

use serde_json::{Value, json};

/// `action_id` of the draft's reject button.
pub const REJECT_ACTION_ID: &str = "reject_reply";

/// `callback_id` of the reject modal; a submission is recognised by it.
pub const REJECT_MODAL_CALLBACK_ID: &str = "reject_reply_modal";

/// How long opening the modal may take before falling back to publishing.
///
/// Small, because the fallback publish has to fit in what is left of Slack's
/// ~3-second acknowledgement window.
pub const MODAL_BUDGET: Duration = Duration::from_millis(1_000);

/// Longest alternative reply the modal accepts; the same bound the plugin's
/// own modal uses.
const ALT_REPLY_MAX_CHARS: usize = 2_900;

/// Opens a modal. A trait so tests need neither Slack nor a token.
pub trait ModalOpener: Send + Sync {
    /// `views.open` with `token`. `Err` carries Slack's error code or the
    /// transport failure, for the log.
    fn open(
        &self,
        token: &str,
        trigger_id: &str,
        view: &Value,
    ) -> impl Future<Output = Result<(), String>> + Send;
}

/// The real Slack Web API.
pub struct SlackModals {
    client: reqwest::Client,
    url: String,
}

impl SlackModals {
    /// `views.open` under `base_url` (`https://slack.com/api` in production).
    pub fn new(client: reqwest::Client, base_url: &str) -> Self {
        Self {
            client,
            url: format!("{}/views.open", base_url.trim_end_matches('/')),
        }
    }
}

impl ModalOpener for SlackModals {
    async fn open(&self, token: &str, trigger_id: &str, view: &Value) -> Result<(), String> {
        let response: Value = self
            .client
            .post(&self.url)
            .bearer_auth(token)
            .json(&json!({ "trigger_id": trigger_id, "view": view }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("not ok")
                .to_string())
        }
    }
}

/// `(trigger_id, private_metadata)` when `payload` is a reject press this
/// process can open the modal for.
///
/// The metadata is the button's `value` (`{"d","c","ts"}`) plus the press's
/// `response_url` as `r` — the shape the plugin's own modal carries, so the
/// submission is read the same way whoever opened it. A value that is not
/// that JSON is an old button; it is left to the publish path.
pub fn reject_press(payload: &Value) -> Option<(String, String)> {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return None;
    }
    let action = payload.pointer("/actions/0")?;
    if action.get("action_id").and_then(Value::as_str) != Some(REJECT_ACTION_ID) {
        return None;
    }
    let trigger_id = payload.get("trigger_id").and_then(Value::as_str)?;
    let mut metadata: Value =
        serde_json::from_str(action.get("value").and_then(Value::as_str)?).ok()?;
    // All three, as strings: the submission is routed by `c` / `ts`, and a
    // modal whose submission cannot be routed would swallow the rejection.
    for key in ["d", "c", "ts"] {
        metadata.get(key)?.as_str()?;
    }
    metadata["r"] = payload.get("response_url").cloned().unwrap_or(Value::Null);
    Some((trigger_id.to_string(), metadata.to_string()))
}

/// The reject modal. Unlike the plugin's, it cannot quote the draft: an
/// ephemeral's press carries no copy of the message it came from.
pub fn reject_modal(metadata: &str) -> Value {
    json!({
        "type": "modal",
        "callback_id": REJECT_MODAL_CALLBACK_ID,
        "private_metadata": metadata,
        "title": { "type": "plain_text", "text": "返信案を却下" },
        "submit": { "type": "plain_text", "text": "却下する" },
        "close": { "type": "plain_text", "text": "やめる" },
        "blocks": [
            {
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": "返信案を却下します。「やめる」なら承認待ちのまま残ります。"
                }]
            },
            {
                "type": "input",
                "block_id": "alt_reply",
                "optional": true,
                "label": { "type": "plain_text", "text": "代わりの返信" },
                "hint": {
                    "type": "plain_text",
                    "text": "本来こう返したかった文面。記録に残り、後から見返せます。下の「送信」にチェックしたときだけスレッドにも投稿します。"
                },
                "element": {
                    "type": "plain_text_input",
                    "action_id": "alt_text",
                    "multiline": true,
                    "max_length": ALT_REPLY_MAX_CHARS
                }
            },
            {
                "type": "input",
                "block_id": "send_alt",
                "optional": true,
                "label": { "type": "plain_text", "text": "送信" },
                "element": {
                    "type": "checkboxes",
                    "action_id": "send",
                    "options": [{
                        "text": { "type": "plain_text", "text": "代わりの返信をスレッドに送信する（本人名義）" },
                        "value": "send"
                    }]
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(action_id: &str, value: &str) -> Value {
        json!({
            "type": "block_actions",
            "trigger_id": "T.1",
            "response_url": "https://hooks.slack.test/r/1",
            "actions": [{ "action_id": action_id, "value": value }]
        })
    }

    #[test]
    fn only_a_reject_press_with_a_draft_value_opens_the_modal() {
        let value = r#"{"d":"draft-1","c":"C1","ts":"100.0"}"#;
        let (trigger, metadata) = reject_press(&press("reject_reply", value)).unwrap();
        assert_eq!(trigger, "T.1");
        let metadata: Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(metadata["d"], "draft-1");
        assert_eq!(metadata["r"], "https://hooks.slack.test/r/1");

        assert!(reject_press(&press("approve_reply", value)).is_none());
        assert!(
            reject_press(&press("reject_reply", "draft-1")).is_none(),
            "an old button"
        );
        assert!(
            reject_press(&press("reject_reply", r#"{"d":"draft-1"}"#)).is_none(),
            "no thread to route the submission to"
        );
        let mut no_trigger = press("reject_reply", value);
        no_trigger.as_object_mut().unwrap().remove("trigger_id");
        assert!(reject_press(&no_trigger).is_none());
    }
}
