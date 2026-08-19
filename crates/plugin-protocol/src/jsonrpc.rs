//! JSON-RPC 2.0 envelopes and framing (F-51).
//!
//! # Framing
//!
//! Messages are exchanged as **NDJSON**: exactly one JSON value per line,
//! terminated by `\n`, over the plugin's stdio. This is simpler than LSP-style
//! `Content-Length` headers and carries over unchanged to a future Unix-socket
//! transport. Use [`to_line`] to encode and split incoming bytes on `\n` to
//! decode.
//!
//! # Message kinds
//!
//! - [`Request`] — Orchestrator → Plugin, expects a [`Response`] with matching
//!   `id`.
//! - [`Response`] — carries exactly one of `result` or `error`.
//! - [`Notification`] — fire-and-forget, no `id`, no response (used for
//!   `state/subscribe` streams and `notify`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The JSON-RPC protocol version string (`"2.0"`).
pub const JSONRPC_VERSION: &str = "2.0";

/// A request identifier: a number or a string (JSON-RPC allows both).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id.
    Number(i64),
    /// String id.
    Str(String),
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Number(n)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        RequestId::Str(s)
    }
}

/// A JSON-RPC request (Orchestrator → Plugin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Correlates the response.
    pub id: RequestId,
    /// Method name (see [`method`](crate::method)).
    pub method: String,
    /// Method parameters, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Build a request with typed params serialized to JSON.
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC response (Plugin → Orchestrator). Exactly one of `result` /
/// `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The id of the request being answered. Per JSON-RPC 2.0 this is `null`
    /// when the id could not be determined (parse error / invalid request), so
    /// it is modelled as an `Option` that serializes as `null` when absent.
    #[serde(default)]
    pub id: Option<RequestId>,
    /// Result payload on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error payload on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    /// A successful response.
    pub fn result(id: impl Into<RequestId>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.into()),
            result: Some(result),
            error: None,
        }
    }

    /// An error response for a known request id.
    pub fn error(id: impl Into<RequestId>, error: Error) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.into()),
            result: None,
            error: Some(error),
        }
    }

    /// An error response whose id could not be determined (JSON-RPC `null`
    /// id) — used for parse errors and invalid requests.
    pub fn error_without_id(error: Error) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            result: None,
            error: Some(error),
        }
    }

    /// Whether this response carries an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// A JSON-RPC notification: no `id`, no response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Method name.
    pub method: String,
    /// Method parameters, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build a notification with typed params serialized to JSON.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Error {
    /// Numeric error code (see [`error_code`]).
    pub code: i64,
    /// Short human-readable message.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    /// Build an error with a code and message.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured data.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Standard and totsuka-specific JSON-RPC error codes.
///
/// `-32700..-32600` are the JSON-RPC standard codes; `-32000..-32099` is the
/// server-defined range, which totsuka uses for protocol-level failures.
pub mod error_code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON was not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal plugin error.
    pub const INTERNAL_ERROR: i64 = -32603;

    /// The plugin-specific configuration is invalid (F-59).
    ///
    /// `-32001` and `-32002` are **retired** (0.5.0) and must not be reused:
    /// `PROTOCOL_VERSION_MISMATCH` could never be sent, because the
    /// compatibility check happens host-side *before* the process is spawned
    /// (F-54), and `CAPABILITY_UNSUPPORTED` could never be sent either,
    /// because the Orchestrator only calls what a plugin declared. Both were
    /// unreachable by construction, not merely unused.
    pub const CONFIG_INVALID: i64 = -32003;
    /// The Orchestrator is draining/shutting down and not accepting
    /// `task/submit` (0.1.6). Retryable: back off and re-submit — the
    /// submission is idempotent, so a re-submit after a lost ack is answered
    /// with `duplicate`, never ingested twice.
    pub const NOT_ACCEPTING: i64 = -32004;
    /// A per-plugin in-flight budget for plugin-initiated requests is
    /// exhausted (0.1.6).
    ///
    /// The name predates `task/lookup` (0.2.4), which has its own separate
    /// budget and answers with this code too. **What to do about it is the
    /// method's contract, not this code's**: `task/submit` retries with
    /// backoff, same as [`NOT_ACCEPTING`], because the task must eventually
    /// get in; `task/lookup` degrades to resolving without the hint, because
    /// an unanswered lookup costs nothing but the work it would have saved.
    pub const SUBMIT_OVERLOADED: i64 = -32005;
    /// The session named by `resume_session_id` could not be resumed (0.2.4,
    /// #242) — it is gone, expired, or otherwise unusable.
    ///
    /// The contract is deliberately about **the session**, not about any
    /// backend's mechanism for resuming one: an agent plugin decides this from
    /// what it can observe on its own side, so a different multiplexer or CLI
    /// reports it the same way. Dispatching the *same* request without
    /// `resume_session_id` is expected to succeed, and the Orchestrator retries
    /// once that way — so a plugin returning this must leave nothing behind
    /// that would make the retry fail.
    ///
    /// Not retryable as-is: re-sending the identical request would fail
    /// identically.
    pub const SESSION_UNRESUMABLE: i64 = -32006;
}

/// Encode a serializable message as one NDJSON line (no trailing newline).
///
/// The caller appends `\n` when writing to the transport.
pub fn to_line<T: Serialize>(message: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_and_omits_absent_params() {
        let req = Request::new(1, crate::method::SHUTDOWN, None);
        let line = to_line(&req).unwrap();
        assert!(!line.contains("params"), "absent params must be omitted");
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.jsonrpc, "2.0");
    }

    #[test]
    fn response_carries_result_xor_error() {
        let ok = Response::result(RequestId::Str("a".into()), serde_json::json!({"ok": true}));
        assert!(!ok.is_error());
        let line = to_line(&ok).unwrap();
        assert!(!line.contains("error"));

        let err = Response::error(1, Error::new(error_code::METHOD_NOT_FOUND, "nope"));
        assert!(err.is_error());
        let back: Response = serde_json::from_str(&to_line(&err).unwrap()).unwrap();
        assert_eq!(back.error.unwrap().code, error_code::METHOD_NOT_FOUND);
    }

    #[test]
    fn error_without_id_serializes_null_id() {
        // Parse errors have no determinable id; JSON-RPC requires `"id": null`.
        let err = Response::error_without_id(Error::new(error_code::PARSE_ERROR, "bad json"));
        let value: serde_json::Value = serde_json::from_str(&to_line(&err).unwrap()).unwrap();
        assert!(value.get("id").is_some(), "id key must be present");
        assert!(value["id"].is_null(), "id must be null");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(back.id, None);
        assert_eq!(back.error.unwrap().code, error_code::PARSE_ERROR);
    }

    #[test]
    fn notification_has_no_id() {
        let note = Notification::new(crate::method::NOTIFY, Some(serde_json::json!({"e": 1})));
        let line = to_line(&note).unwrap();
        assert!(!line.contains("\"id\""));
        let back: Notification = serde_json::from_str(&line).unwrap();
        assert_eq!(back, note);
    }

    #[test]
    fn request_id_accepts_number_or_string() {
        let n: RequestId = serde_json::from_str("7").unwrap();
        assert_eq!(n, RequestId::Number(7));
        let s: RequestId = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(s, RequestId::Str("abc".into()));
    }
}
