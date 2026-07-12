//! JSON-RPC dispatch for the stdio server (F-51/F-90).
//!
//! Requests (`initialize` / `config/validate` / `shutdown`) get a response;
//! `notify` is a fire-and-forget notification (no response, F-93) whose delivery
//! is spawned so a slow/failing send never blocks the read loop. Generic over a
//! [`SenderFactory`] so the whole surface is tested against a recording fake.

use plugin_protocol::jsonrpc::{Error, Response, error_code, to_line};
use plugin_protocol::methods::{
    ConfigValidateResult, InitializeParams, InitializeResult, NotifierEvent, NotifyParams,
};
use plugin_protocol::{Capabilities, RequestId, method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::NotifierConfig;
use crate::sender::{Notice, NotificationSender};

/// Builds a notification sender from config. Abstracted so the server is tested
/// against a recording fake.
pub trait SenderFactory {
    /// The sender this factory produces.
    type Sender: NotificationSender;
    /// Build a sender for `config`.
    fn build(&self, config: &NotifierConfig) -> Self::Sender;
}

/// The result of handling one input line.
pub struct Reply {
    /// The response line to write (absent for notifications).
    pub line: Option<String>,
    /// Whether the server should exit after this line (`shutdown`).
    pub shutdown: bool,
}

impl Reply {
    fn none() -> Self {
        Self {
            line: None,
            shutdown: false,
        }
    }
    fn respond(response: Response) -> Self {
        Self {
            line: to_line(&response).ok(),
            shutdown: false,
        }
    }
}

/// The macOS notifier stdio server.
pub struct Server<F: SenderFactory> {
    factory: F,
    config: NotifierConfig,
    sender: Option<F::Sender>,
}

impl<F: SenderFactory> Server<F> {
    /// A fresh, uninitialized server using `factory` to build senders.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            config: NotifierConfig::defaults(),
            sender: None,
        }
    }

    /// Parse and dispatch one NDJSON line. A `notify` notification is delivered
    /// fire-and-forget and yields no response; requests get a response.
    pub async fn handle_line(&mut self, line: &str) -> Reply {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Reply::none();
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Reply::respond(Response::error_without_id(Error::new(
                error_code::PARSE_ERROR,
                "request was not valid JSON",
            )));
        };
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let params = value.get("params").cloned().unwrap_or(Value::Null);

        // `notify` is a notification (no id): deliver and never reply (F-93).
        if method == method::NOTIFY {
            self.handle_notify(params);
            return Reply::none();
        }
        // Any other notification (no id) is ignored.
        let Some(id) = value.get("id").map(request_id) else {
            return Reply::none();
        };
        match method {
            method::INITIALIZE => self.initialize(id, params),
            method::CONFIG_VALIDATE => self.config_validate(id, params).await,
            method::SHUTDOWN => Reply {
                line: to_line(&Response::result(id, Value::Null)).ok(),
                shutdown: true,
            },
            other => Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method: {other}"),
                ),
            )),
        }
    }

    fn initialize(&mut self, id: RequestId, params: Value) -> Reply {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return Reply::respond(Response::error(id, e)),
        };
        let config: NotifierConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid macos notifier config: {e}"),
                    ),
                ));
            }
        };
        self.sender = Some(self.factory.build(&config));
        self.config = config;
        Reply::respond(Response::result(id, capabilities_result()))
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let config: NotifierConfig = match params
            .get("config")
            .cloned()
            .ok_or(())
            .and_then(|c| serde_json::from_value(c).map_err(|_| ()))
        {
            Ok(c) => c,
            Err(()) => return ok_validate(id, vec!["config does not parse".into()]),
        };
        // Confirm the notification tool is runnable, without posting a visible
        // notification (F-59).
        let mut errors = Vec::new();
        let sender = self.factory.build(&config);
        if let Err(e) = sender.probe().await {
            errors.push(format!("cannot post notifications → {e}"));
        }
        ok_validate(id, errors)
    }

    /// Deliver a `notify` event (F-90), fire-and-forget: filtered out or failing
    /// sends are logged, never surfaced, so the notifier can't affect tasks.
    fn handle_notify(&self, params: Value) {
        let notify: NotifyParams = match serde_json::from_value(params) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring malformed notify params");
                return;
            }
        };
        if !self
            .config
            .filter
            .allows(notify.workflow.as_deref(), notify.event)
        {
            tracing::debug!(event = ?notify.event, workflow = ?notify.workflow, "notification filtered out");
            return;
        }
        let Some(sender) = self.sender.clone() else {
            tracing::warn!("notify received before initialize; dropping");
            return;
        };
        let notice = format_notice(&notify);
        // Fire-and-forget (F-93): deliver on a task; a failure is only logged.
        tokio::spawn(async move {
            if let Err(e) = sender.send(notice).await {
                tracing::warn!(error = %e, "failed to post notification");
            }
        });
    }
}

/// Format a notification from an event: an at-a-glance headline plus the task
/// title / workflow and any body (e.g. a question excerpt).
fn format_notice(notify: &NotifyParams) -> Notice {
    let (icon, label) = event_label(notify.event);
    let subtitle = match &notify.workflow {
        Some(wf) => format!("{} · {wf}", notify.title),
        None => notify.title.clone(),
    };
    let body = notify
        .body
        .clone()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| match &notify.task_id {
            Some(id) => format!("タスク {id}"),
            None => label.to_string(),
        });
    Notice {
        title: format!("{icon} {label}"),
        subtitle,
        body,
    }
}

/// The icon + Japanese label for an event.
fn event_label(event: NotifierEvent) -> (&'static str, &'static str) {
    match event {
        NotifierEvent::WaitingInput => ("⏳", "入力待ち"),
        NotifierEvent::Done => ("✅", "完了"),
        NotifierEvent::Failed => ("❌", "失敗"),
        NotifierEvent::Pending => ("🔔", "確認待ち"),
    }
}

/// A notifier declares no feature capabilities; it only receives `notify`.
fn capabilities_result() -> Value {
    serde_json::to_value(InitializeResult {
        plugin_version: plugin_version(),
        capabilities: Capabilities::default(),
    })
    .unwrap_or(Value::Null)
}

/// This plugin's version, from Cargo. Falls back to `0.0.0` if unparseable.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(semver::Version::new(0, 0, 0))
}

/// Deserialize params, returning an INVALID_PARAMS error on failure.
fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, Error> {
    serde_json::from_value(params.clone())
        .map_err(|e| Error::new(error_code::INVALID_PARAMS, format!("invalid params: {e}")))
}

/// A `config/validate` success reply (validity is in the payload).
fn ok_validate(id: RequestId, errors: Vec<String>) -> Reply {
    let result = ConfigValidateResult {
        valid: errors.is_empty(),
        errors,
    };
    Reply::respond(Response::result(
        id,
        serde_json::to_value(result).unwrap_or(Value::Null),
    ))
}

/// Convert a JSON id value into a [`RequestId`].
fn request_id(id: &Value) -> RequestId {
    if let Some(n) = id.as_i64() {
        RequestId::Number(n)
    } else if let Some(s) = id.as_str() {
        RequestId::Str(s.to_string())
    } else {
        RequestId::Str(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_notice_with_workflow_and_body() {
        let notice = format_notice(&NotifyParams {
            event: NotifierEvent::WaitingInput,
            task_id: Some("T1".into()),
            workflow: Some("impl".into()),
            title: "Fix bug".into(),
            body: Some("Delete the file? (y/n)".into()),
        });
        assert_eq!(notice.title, "⏳ 入力待ち");
        assert_eq!(notice.subtitle, "Fix bug · impl");
        assert_eq!(notice.body, "Delete the file? (y/n)");
    }

    #[test]
    fn body_falls_back_to_task_id() {
        let notice = format_notice(&NotifyParams {
            event: NotifierEvent::Done,
            task_id: Some("T9".into()),
            workflow: None,
            title: "Ship it".into(),
            body: None,
        });
        assert_eq!(notice.title, "✅ 完了");
        assert_eq!(notice.subtitle, "Ship it");
        assert_eq!(notice.body, "タスク T9");
    }
}
