//! Golden wire-fixture contract tests (#173).
//!
//! Every file under `tests/fixtures/` is a committed JSON-RPC envelope exactly
//! as it appears on the NDJSON transport (pretty-printed for review; the
//! comparison is structural, so whitespace does not matter). The fixtures are
//! the **backward-compatibility contract** of the plugin protocol:
//!
//! - Renaming or removing a protocol field makes the corresponding fixture
//!   fail to deserialize (or re-serialize differently), so the break surfaces
//!   here instead of in an external plugin.
//! - Changing serde attributes (`rename_all`, `skip_serializing_if`, …)
//!   changes the re-serialized shape and diverges from the fixture.
//! - Adding a field/method requires touching a fixture, so **every wire
//!   change shows up as a fixture diff in the PR** — reviewing that diff *is*
//!   the compatibility review.
//!
//! When a fixture diff appears in a PR, decide additive vs breaking and bump
//! `PROTOCOL_VERSION` accordingly — the procedure lives in
//! `ai-docs/components/plugin-protocol.md`.
//!
//! Each fixture is checked four ways:
//! 1. the envelope (`Request`/`Response`/`Notification`) deserializes and
//!    re-serializes to the exact same JSON value;
//! 2. the typed `params`/`result` payload deserializes and re-serializes to
//!    the exact same JSON value;
//! 3. the payload still deserializes with an unknown field injected
//!    (forward compat: a newer peer may send fields we don't know);
//! 4. enum wire values are pinned exhaustively in
//!    [`enum_wire_values_are_pinned`].

use std::fs;
use std::path::Path;

use plugin_protocol::jsonrpc::{Notification, Request, Response, error_code};
use plugin_protocol::manifest::{OutputCapability, PluginKind};
use plugin_protocol::method;
use plugin_protocol::methods::{
    AgentState, ConfigValidateParams, ConfigValidateResult, DiagnosticsSnapshotParams,
    DiagnosticsSnapshotResult, ExecutionMode, InitializeParams, InitializeResult, NotifierEvent,
    NotifyParams, ResultPublishParams, SessionAttachParams, SessionAttachResult,
    SessionFocusParams, SessionFocusResult, SessionListParams, SessionListResult,
    SessionReleaseParams, SessionReleaseResult, StateNotification, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams, TaskDispatchResult, TaskLookupParams, TaskLookupResult,
    TaskSubmitParams, TaskSubmitResult, TaskSubmitStatus, TaskUpdateStatusParams,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Load a committed fixture as a JSON value.
fn load(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}"))
}

/// Typed payload contract: the fixture payload deserializes into `T`,
/// re-serializes to the identical JSON value, and tolerates unknown fields
/// (forward compat).
fn check_payload<T: Serialize + DeserializeOwned>(name: &str, payload: &Value) {
    let typed: T = serde_json::from_value(payload.clone()).unwrap_or_else(|e| {
        panic!("{name}: payload no longer deserializes — a field was renamed/removed?: {e}")
    });
    assert_eq!(
        &serde_json::to_value(&typed).unwrap(),
        payload,
        "{name}: re-serialized payload diverged from the committed wire"
    );
    if let Value::Object(map) = payload {
        let mut extended = map.clone();
        extended.insert("__field_from_a_future_protocol".into(), Value::Bool(true));
        let tolerant: Result<T, _> = serde_json::from_value(Value::Object(extended));
        assert!(
            tolerant.is_ok(),
            "{name}: unknown fields must be tolerated (forward compat)"
        );
    }
}

/// Envelope contract for a request fixture: JSON-RPC 2.0, the expected
/// method, an `id`, and an exact envelope round-trip.
fn request_envelope(name: &str, method_name: &str, wire: &Value) -> Request {
    assert_eq!(wire["jsonrpc"], "2.0", "{name}");
    assert_eq!(wire["method"], method_name, "{name}");
    assert!(wire.get("id").is_some(), "{name}: requests carry an id");
    let envelope: Request = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{name}: envelope no longer deserializes: {e}"));
    assert_eq!(
        serde_json::to_value(&envelope).unwrap(),
        *wire,
        "{name}: re-serialized envelope diverged from the committed wire"
    );
    envelope
}

/// Envelope contract for a response fixture, with an exact round-trip.
/// Not usable for `"result": null` acks — see [`check_ack_response`].
fn response_envelope(name: &str, wire: &Value) -> Response {
    assert_eq!(wire["jsonrpc"], "2.0", "{name}");
    let envelope: Response = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{name}: envelope no longer deserializes: {e}"));
    assert_eq!(
        serde_json::to_value(&envelope).unwrap(),
        *wire,
        "{name}: re-serialized envelope diverged from the committed wire"
    );
    envelope
}

/// A request whose `params` payload is typed as `P`.
fn check_request<P: Serialize + DeserializeOwned>(name: &str, method_name: &str) {
    let wire = load(name);
    let envelope = request_envelope(name, method_name, &wire);
    let params = envelope
        .params
        .unwrap_or_else(|| panic!("{name}: {method_name} carries params"));
    check_payload::<P>(name, &params);
}

/// A request without params (`shutdown`): the key must be absent, not null.
fn check_bare_request(name: &str, method_name: &str) {
    let wire = load(name);
    let envelope = request_envelope(name, method_name, &wire);
    assert!(
        envelope.params.is_none() && wire.get("params").is_none(),
        "{name}: {method_name} must omit params entirely"
    );
}

/// A success response whose `result` payload is typed as `R`.
fn check_response<R: Serialize + DeserializeOwned>(name: &str) {
    let wire = load(name);
    let envelope = response_envelope(name, &wire);
    assert!(
        !envelope.is_error(),
        "{name}: success responses carry no error"
    );
    let result = envelope
        .result
        .unwrap_or_else(|| panic!("{name}: success responses carry a result"));
    check_payload::<R>(name, &result);
}

/// A `"result": null` ack (shutdown, task/update_status, …): the SDK writes
/// it via `Response::result(id, Value::Null)`, but reading it back
/// canonicalizes `Some(Null)` to `None` (an `Option<Value>` swallows JSON
/// null), so the envelope does **not** value-round-trip. Pin both directions
/// of that asymmetry explicitly.
fn check_ack_response(name: &str) {
    let wire = load(name);
    assert_eq!(wire["jsonrpc"], "2.0", "{name}");
    assert!(
        wire.get("result").is_some_and(Value::is_null),
        "{name}: acks carry a literal null result (JSON-RPC requires the member)"
    );
    let envelope: Response = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{name}: envelope no longer deserializes: {e}"));
    assert!(!envelope.is_error(), "{name}");
    assert!(
        envelope.result.is_none(),
        "{name}: readers canonicalize a null result to None"
    );
    let id = envelope.id.expect("acks answer a known request id");
    assert_eq!(
        serde_json::to_value(Response::result(id, Value::Null)).unwrap(),
        wire,
        "{name}: the writer-side ack shape diverged from the committed wire"
    );
}

/// An error response for a known request id, with the expected error code.
fn check_error_response(name: &str, code: i64) {
    let wire = load(name);
    let envelope = response_envelope(name, &wire);
    assert!(
        envelope.result.is_none(),
        "{name}: error responses carry no result"
    );
    let error = envelope
        .error
        .unwrap_or_else(|| panic!("{name}: error responses carry an error"));
    assert_eq!(error.code, code, "{name}");
}

/// A notification whose `params` payload is typed as `P`: no `id` key at all.
fn check_notification<P: Serialize + DeserializeOwned>(name: &str, method_name: &str) {
    let wire = load(name);
    assert_eq!(wire["jsonrpc"], "2.0", "{name}");
    assert_eq!(wire["method"], method_name, "{name}");
    assert!(
        wire.get("id").is_none(),
        "{name}: notifications never carry an id"
    );
    let envelope: Notification = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{name}: envelope no longer deserializes: {e}"));
    assert_eq!(
        serde_json::to_value(&envelope).unwrap(),
        wire,
        "{name}: re-serialized envelope diverged from the committed wire"
    );
    let params = envelope
        .params
        .unwrap_or_else(|| panic!("{name}: {method_name} carries params"));
    check_payload::<P>(name, &params);
}

// ---------------------------------------------------------------------------
// Common
// ---------------------------------------------------------------------------

#[test]
fn initialize_wire() {
    check_request::<InitializeParams>("initialize.request.json", method::INITIALIZE);
    check_response::<InitializeResult>("initialize.response.json");
}

#[test]
fn shutdown_wire() {
    check_bare_request("shutdown.request.json", method::SHUTDOWN);
    check_ack_response("shutdown.response.json");
}

#[test]
fn config_validate_wire() {
    check_request::<ConfigValidateParams>("config_validate.request.json", method::CONFIG_VALIDATE);
    check_response::<ConfigValidateResult>("config_validate.response.json");
}

// ---------------------------------------------------------------------------
// task_source
// ---------------------------------------------------------------------------

#[test]
fn task_submit_wire() {
    check_request::<TaskSubmitParams>("task_submit.request.json", method::TASK_SUBMIT);
    check_response::<TaskSubmitResult>("task_submit.response.json");
    check_response::<TaskSubmitResult>("task_submit_rejected.response.json");
    // Retryable conditions are JSON-RPC errors, never a TaskSubmitStatus.
    check_error_response(
        "task_submit_not_accepting.error.json",
        error_code::NOT_ACCEPTING,
    );
}

#[test]
fn task_lookup_wire() {
    check_request::<TaskLookupParams>("task_lookup.request.json", method::TASK_LOOKUP);
    check_response::<TaskLookupResult>("task_lookup.response.json");
    // `known: false` omits `repo` entirely — the shape a source sees for a
    // conversation it must resolve from scratch.
    check_response::<TaskLookupResult>("task_lookup_unknown.response.json");
}

/// An agent reporting an unusable session (0.2.4, #242). It is an *error*, not
/// a `TaskDispatchResult` variant: the dispatch did not happen, and the
/// Orchestrator's answer is to send it again without `resume_session_id`.
#[test]
fn session_unresumable_wire() {
    check_error_response(
        "session_unresumable.error.json",
        error_code::SESSION_UNRESUMABLE,
    );
}

#[test]
fn task_update_status_wire() {
    check_request::<TaskUpdateStatusParams>(
        "task_update_status.request.json",
        method::TASK_UPDATE_STATUS,
    );
    check_ack_response("task_update_status.response.json");
}

#[test]
fn result_publish_wire() {
    check_request::<ResultPublishParams>("result_publish.request.json", method::RESULT_PUBLISH);
    check_ack_response("result_publish.response.json");
}

// ---------------------------------------------------------------------------
// agent_ide
// ---------------------------------------------------------------------------

#[test]
fn task_dispatch_wire() {
    check_request::<TaskDispatchParams>("task_dispatch.request.json", method::TASK_DISPATCH);
    check_response::<TaskDispatchResult>("task_dispatch.response.json");
}

#[test]
fn task_cancel_wire() {
    check_request::<TaskCancelParams>("task_cancel.request.json", method::TASK_CANCEL);
    check_ack_response("task_cancel.response.json");
}

#[test]
fn session_attach_wire() {
    check_request::<SessionAttachParams>("session_attach.request.json", method::SESSION_ATTACH);
    check_response::<SessionAttachResult>("session_attach.response.json");
}

#[test]
fn state_subscribe_wire() {
    check_request::<StateSubscribeParams>("state_subscribe.request.json", method::STATE_SUBSCRIBE);
    check_ack_response("state_subscribe.response.json");
}

#[test]
fn state_notification_wire() {
    check_notification::<StateNotification>(
        "state_notification.notification.json",
        method::STATE_NOTIFICATION,
    );
}

#[test]
fn diagnostics_snapshot_wire() {
    check_request::<DiagnosticsSnapshotParams>(
        "diagnostics_snapshot.request.json",
        method::DIAGNOSTICS_SNAPSHOT,
    );
    check_response::<DiagnosticsSnapshotResult>("diagnostics_snapshot.response.json");
}

#[test]
fn session_focus_wire() {
    check_request::<SessionFocusParams>("session_focus.request.json", method::SESSION_FOCUS);
    check_response::<SessionFocusResult>("session_focus.response.json");
}

#[test]
fn session_release_wire() {
    check_request::<SessionReleaseParams>("session_release.request.json", method::SESSION_RELEASE);
    check_response::<SessionReleaseResult>("session_release.response.json");
}

#[test]
fn session_list_wire() {
    check_request::<SessionListParams>("session_list.request.json", method::SESSION_LIST);
    check_response::<SessionListResult>("session_list.response.json");
}

// ---------------------------------------------------------------------------
// notifier
// ---------------------------------------------------------------------------

#[test]
fn notify_wire() {
    check_notification::<NotifyParams>("notify.notification.json", method::NOTIFY);
}

// ---------------------------------------------------------------------------
// Envelope edge cases
// ---------------------------------------------------------------------------

/// Parse errors answer with `"id": null` — the key present, not omitted.
#[test]
fn parse_error_wire() {
    let name = "parse_error.response.json";
    let wire = load(name);
    assert!(
        wire.get("id").is_some_and(Value::is_null),
        "{name}: JSON-RPC requires a literal null id when it is unknown"
    );
    let envelope = response_envelope(name, &wire);
    assert!(envelope.id.is_none(), "{name}");
    assert_eq!(envelope.error.unwrap().code, error_code::PARSE_ERROR);
}

// ---------------------------------------------------------------------------
// Enum wire values
// ---------------------------------------------------------------------------

/// Every variant of every protocol enum, pinned to its snake_case wire
/// string. A renamed variant fails here; a **new** variant must be added
/// here (and to a fixture where applicable) in the same PR.
#[test]
fn enum_wire_values_are_pinned() {
    #[track_caller]
    fn pin<T: Serialize>(value: T, wire: &str) {
        assert_eq!(
            serde_json::to_value(&value).unwrap(),
            Value::String(wire.to_string())
        );
    }
    pin(TaskSubmitStatus::Accepted, "accepted");
    pin(TaskSubmitStatus::Duplicate, "duplicate");
    pin(TaskSubmitStatus::Rejected, "rejected");
    pin(ExecutionMode::Plan, "plan");
    pin(ExecutionMode::Implement, "implement");
    pin(AgentState::Idle, "idle");
    pin(AgentState::Running, "running");
    pin(AgentState::WaitingInput, "waiting_input");
    pin(AgentState::Done, "done");
    pin(AgentState::Failed, "failed");
    pin(NotifierEvent::WaitingInput, "waiting_input");
    pin(NotifierEvent::Done, "done");
    pin(NotifierEvent::Failed, "failed");
    pin(NotifierEvent::Pending, "pending");
    pin(NotifierEvent::Escalated, "escalated");
    pin(NotifierEvent::VerificationPending, "verification_pending");
    pin(PluginKind::TaskSource, "task_source");
    pin(PluginKind::AgentIde, "agent_ide");
    pin(PluginKind::Notifier, "notifier");
    pin(OutputCapability::Source, "source");
}
