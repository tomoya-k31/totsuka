use std::sync::Arc;

use crate::adapters::clock::ManualClock;
use crate::domain::task::{SourceTaskId, TaskId};
use crate::domain::workflow::WorkflowMode;

use super::{HookEventInsert, NewTask, TaskMessage, TaskMessageInsert};

/// Fixed test epoch (#174).
pub(super) const T0: &str = "2026-01-01T00:00:00Z";

/// A manually driven clock frozen at [`T0`], for exact-timestamp asserts.
pub(super) fn manual_clock() -> Arc<ManualClock> {
    let t0 =
        time::OffsetDateTime::parse(T0, &time::format_description::well_known::Rfc3339).unwrap();
    Arc::new(ManualClock::new(t0))
}

pub(super) fn sample_task() -> NewTask {
    NewTask {
        source: "github".to_string(),
        source_task_id: SourceTaskId("42".to_string()),
        workflow: "implement".to_string(),
        mode: WorkflowMode::Implement,
        repo: None,
        priority: 0,
        title: "Fix the bug".to_string(),
        url: Some("https://example.com/issues/42".to_string()),
        source_payload: Some(serde_json::json!({"labels": ["bug"]})),
        last_signal_at: None,
    }
}

/// A hook event with empty idempotency components (the common case: the
/// `(job_id, event)` pair carries the key).
pub(super) fn hook_event(
    task_id: TaskId,
    job_id: &str,
    event: &str,
    status: Option<&str>,
) -> HookEventInsert {
    HookEventInsert {
        job_id: job_id.to_string(),
        task_id,
        tool_session_id: String::new(),
        prompt_id: String::new(),
        event: event.to_string(),
        status: status.map(str::to_string),
        payload: "{}".to_string(),
    }
}

pub(super) fn message(task_id: TaskId, key: &str, body: &str) -> TaskMessageInsert {
    TaskMessageInsert {
        task_id,
        message_key: key.to_string(),
        author: Some("tomoya".to_string()),
        body: body.to_string(),
        url: Some(format!("https://example.com/{key}")),
        payload: format!(r#"{{"id":"conv","message_key":"{key}"}}"#),
    }
}

pub(super) fn keys(messages: &[TaskMessage]) -> Vec<&str> {
    messages.iter().map(|m| m.message_key.as_str()).collect()
}
