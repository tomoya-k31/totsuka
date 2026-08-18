//! UDS hook-receiving server (#136): a driving adapter that accepts
//! `POST /agent-events` from agent-CLI hooks (Claude Code today) and
//! normalizes each request into an [`AgentSignal`] submitted through a
//! [`SignalPort`]. The pre-rename `/claude-events` path (≤0.2.2, #196) is
//! still accepted: every non-`/focus` path is signal ingestion (E-08).
//!
//! # Why a hand-rolled server
//!
//! The transport is deliberately minimal — a Unix domain socket plus a tiny
//! HTTP/1.1 reader — so it adds no new dependency and no attack surface beyond
//! what the hook scripts need:
//!
//! - **Socket**: [`UnixListener`] at `[hooks].socket_path` (default
//!   `${XDG_RUNTIME_DIR}/totsuka/agent-events.sock`), created `0600`. Stale
//!   sockets are unlinked before bind and on shutdown.
//! - **Parser**: read headers up to `\r\n\r\n`, then `Content-Length` body
//!   bytes. Chunked transfer-encoding is rejected by design (the hook scripts
//!   send fixed-length `curl --data` POSTs). The method is **not** inspected
//!   (deliberately — same-user 0600 + Bearer make method routing pure surface),
//!   and the path only minimally: the exact path `/focus` — whatever the
//!   method — is the control endpoint (F-94, below); **every other path** is
//!   signal ingestion (E-08 forward-compat for the hook scripts is unchanged).
//! - **Auth** (E-03): `Authorization: Bearer <token>`, constant-time compared
//!   to the resolved `[hooks].auth_token_ref`. A mismatch is `401` + a warning;
//!   the listener stays up.
//! - **Body cap**: requests over [`MAX_BODY_BYTES`] get `413`.
//! - **Normalization**: the JSON body becomes an [`AgentSignal`]; unknown
//!   fields are tolerated and preserved verbatim in
//!   [`AgentSignal::payload`](crate::domain::signal::AgentSignal::payload)
//!   (E-08). A missing/unparseable `job_id` is `400` (E-09: a signal is never
//!   correlated by guessing). Everything valid is submitted, then answered
//!   `200` immediately (E-04).
//! - **Lifecycle**: one request per connection, then close (no keep-alive).
//!
//! ## Wire contract (`POST /agent-events`)
//!
//! ```json
//! {
//!   "job_id": "job-42-7",              // required; TOTSUKA_JOB_ID echoed back
//!   "session_id": "abc123",            // tool-native session id (optional)
//!   "prompt_id": "p-1",                // idempotency-key component (optional)
//!   "hook_event_name": "Stop",         // Stop|Notification|QuestionPending|SessionStart|SessionEnd
//!   "status": "completed",             // Stop: completed|needs_input|failed|unknown
//!   "reason": "...",                   // optional
//!   "last_assistant_message": "...",   // Stop (optional)
//!   "transcript_path": "...",          // Stop (optional)
//!   "message": "...",                  // Notification / QuestionPending (optional)
//!   "background_tasks": ["..."]        // Stop: non-empty ⇒ heartbeat (still working)
//! }
//! ```
//!
//! Any additional fields are accepted and kept in the audit payload.
//!
//! ## Control endpoint (`POST /focus`, F-94)
//!
//! `{"task_id": 42}` (a JSON number or numeric string) asks the engine to
//! bring the task's pane to the foreground (`session/focus` via the task's
//! agent plugin). Unlike a signal this is request-response: the reply is
//! `200` with a JSON body `{"focused": bool, "reason"?: string}` — "not
//! focused" is a normal answer (pane gone, capability missing), never an
//! error status. Only an engine that is no longer answering (run loop shut
//! down) is `503`, same as signal ingestion. Auth and body caps are identical
//! to signal ingestion.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::domain::signal::{AgentSignal, JobId, SignalEvent, SignalSource, StopStatus};
use crate::ports::secret::SecretString;
use crate::ports::signal_ingress::{FocusPort, SignalPort};

/// Maximum request-body size (1 MiB). Larger bodies are refused with `413`.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Maximum request-head size (headers block). A head this large is malformed
/// for our purposes and refused with `413`.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Whole-request deadline (read + submit + reply). A client that connects but
/// never finishes sending must not pin a task forever, even under the same-user
/// 0600 threat model (a wedged `curl` or a buggy hook script would otherwise
/// leak one task per stuck connection).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Bind the hook socket, replacing any stale socket file, with `0600` perms.
///
/// The parent directory is created if missing. Returns the bound listener; the
/// caller (`Engine::run`) hands it to [`serve`].
pub fn bind(socket_path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove a stale socket left by a previous run so `bind` does not fail with
    // EADDRINUSE. Only unlink when the path is *actually* a socket, so a
    // misconfigured `socket_path` can never delete an unrelated regular file,
    // directory, or symlink target. Absence is fine; anything else is refused.
    match std::fs::symlink_metadata(socket_path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_socket() {
                std::fs::remove_file(socket_path)?;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "hook socket path {} exists and is not a socket; refusing to remove it",
                        socket_path.display()
                    ),
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(socket_path)?;
    set_socket_perms_0600(socket_path)?;
    Ok(listener)
}

/// Restrict the socket to owner read/write (`0600`) so only the same user can
/// connect (E-03: the socket permission is the first authentication layer).
fn set_socket_perms_0600(socket_path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
}

/// Serve hook POSTs until `shutdown` flips to `true` (or its sender drops),
/// then unlink the socket. Each accepted connection is handled on its own task.
///
/// `sink` receives every valid, authenticated signal; `focus` answers
/// `POST /focus` control requests (F-94). `auth_token` is the expected Bearer
/// token; `None` disables the check (0600 socket only).
pub async fn serve<P, F>(
    listener: UnixListener,
    socket_path: PathBuf,
    sink: P,
    focus: F,
    auth_token: Option<SecretString>,
    mut shutdown: watch::Receiver<bool>,
) where
    P: SignalPort + Clone + Send + 'static,
    F: FocusPort + Clone + Send + 'static,
{
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let sink = sink.clone();
                    let focus = focus.clone();
                    let auth_token = auth_token.clone();
                    tokio::spawn(async move {
                        let handled = tokio::time::timeout(
                            REQUEST_TIMEOUT,
                            handle_connection(stream, &sink, &focus, auth_token.as_ref()),
                        )
                        .await;
                        match handled {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => tracing::warn!("hook connection I/O error: {e}"),
                            Err(_) => tracing::warn!(
                                "hook connection dropped: no complete request within {}s",
                                REQUEST_TIMEOUT.as_secs()
                            ),
                        }
                    });
                }
                Err(e) => {
                    // A transient accept error must not tear down the listener.
                    tracing::warn!("hook listener accept failed: {e}");
                }
            },
            changed = shutdown.changed() => {
                // Sender dropped (Err) or flipped to `true` ⇒ stop.
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    // Best-effort unlink so the next run binds cleanly.
    if let Err(e) = std::fs::remove_file(&socket_path)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(socket = %socket_path.display(), "failed to unlink hook socket: {e}");
    }
    tracing::debug!(socket = %socket_path.display(), "hook receiver stopped");
}

/// Read one request, authenticate, route, and reply. Any protocol problem is
/// answered with the appropriate 4xx and returns `Ok` (the connection is
/// spent, not an I/O failure).
async fn handle_connection<P: SignalPort, F: FocusPort>(
    mut stream: UnixStream,
    sink: &P,
    focus: &F,
    auth_token: Option<&SecretString>,
) -> io::Result<()> {
    let request = match read_request(&mut stream).await {
        Ok(req) => req,
        Err(ReadError::TooLarge) => {
            return write_response(&mut stream, 413, "Payload Too Large").await;
        }
        Err(ReadError::Malformed(reason)) => {
            tracing::warn!("malformed hook request: {reason}");
            return write_response(&mut stream, 400, "Bad Request").await;
        }
        Err(ReadError::Io(e)) => return Err(e),
    };

    // Auth (E-03): a mismatch is 401; the listener keeps running.
    if let Some(expected) = auth_token
        && !bearer_matches(&request.headers, expected)
    {
        tracing::warn!("hook POST rejected: Bearer token missing or mismatched");
        return write_response(&mut stream, 401, "Unauthorized").await;
    }

    // The control endpoint (F-94); every other path is signal ingestion (E-08).
    if request.path == "/focus" {
        return handle_focus(&mut stream, focus, &request.body).await;
    }

    // Normalize the JSON body → AgentSignal.
    let signal = match parse_signal(&request.body) {
        Ok(signal) => signal,
        Err(reason) => {
            tracing::warn!("hook POST body rejected: {reason}");
            return write_response(&mut stream, 400, "Bad Request").await;
        }
    };

    // Submit, then answer 200 immediately (E-04: verification is async).
    match sink.submit(signal).await {
        Ok(_ack) => write_response(&mut stream, 200, "OK").await,
        Err(e) => {
            tracing::error!("hook signal submit failed: {e}");
            write_response(&mut stream, 503, "Service Unavailable").await
        }
    }
}

/// Answer a `POST /focus` control request (F-94): parse `{"task_id": …}`, ask
/// the engine through the [`FocusPort`], and reply the outcome as JSON. Waits
/// for the engine (request-response, unlike signal ingestion) — the
/// connection-level [`REQUEST_TIMEOUT`] bounds the wait.
async fn handle_focus<F: FocusPort>(
    stream: &mut UnixStream,
    focus: &F,
    body: &[u8],
) -> io::Result<()> {
    let task_id = match parse_focus_task_id(body) {
        Ok(id) => id,
        Err(reason) => {
            tracing::warn!("focus request rejected: {reason}");
            return write_response(stream, 400, "Bad Request").await;
        }
    };
    match focus.focus(task_id).await {
        Ok(outcome) => {
            let body = serde_json::to_string(&outcome)
                .unwrap_or_else(|_| r#"{"focused":false}"#.to_string());
            write_json_response(stream, &body).await
        }
        Err(e) => {
            tracing::error!("focus request failed: {e}");
            write_response(stream, 503, "Service Unavailable").await
        }
    }
}

/// Extract `task_id` from a focus request body. Accepts a JSON number or a
/// numeric string (`{"task_id": 42}` / `{"task_id": "42"}` — the notifier's
/// `click_command` template renders it as text).
fn parse_focus_task_id(body: &[u8]) -> Result<i64, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let field = value
        .get("task_id")
        .ok_or_else(|| "missing `task_id`".to_string())?;
    match field {
        serde_json::Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| format!("`task_id` is not an integer: {n}")),
        serde_json::Value::String(s) => s
            .parse()
            .map_err(|_| format!("`task_id` is not an integer: {s:?}")),
        other => Err(format!("`task_id` must be a number or string: {other}")),
    }
}

/// A parsed request: the request path, lower-cased header names, and the raw
/// body bytes. Of the request line only the path is kept, and it is inspected
/// only for the exact control endpoint `/focus` (F-94) — the method and any
/// other path stay uninterpreted (E-08).
struct Request {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Why reading a request stopped early.
enum ReadError {
    /// Head or body exceeded its cap ⇒ 413.
    TooLarge,
    /// The request did not parse ⇒ 400.
    Malformed(String),
    /// Underlying socket failure ⇒ propagate.
    Io(io::Error),
}

/// Read headers up to `\r\n\r\n`, then `Content-Length` body bytes.
async fn read_request(stream: &mut UnixStream) -> Result<Request, ReadError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];

    // Accumulate until the header terminator appears.
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(ReadError::TooLarge);
        }
        let n = stream.read(&mut chunk).await.map_err(ReadError::Io)?;
        if n == 0 {
            return Err(ReadError::Malformed(
                "connection closed before headers completed".into(),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| ReadError::Malformed("non-UTF-8 request head".into()))?;
    let mut lines = head.split("\r\n");
    // Keep only the path off the request line (routing the `/focus` control
    // endpoint, F-94); the method and version are not inspected (E-08).
    let request_line = lines
        .next()
        .ok_or_else(|| ReadError::Malformed("empty request".into()))?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| ReadError::Malformed(format!("invalid header line: {line:?}")))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    // Chunked bodies are unsupported by design.
    if headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"))
    {
        return Err(ReadError::Malformed(
            "chunked transfer-encoding is not supported".into(),
        ));
    }

    let content_length: usize = match headers.iter().find(|(n, _)| n == "content-length") {
        Some((_, v)) => v
            .parse()
            .map_err(|_| ReadError::Malformed(format!("invalid Content-Length: {v:?}")))?,
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(ReadError::TooLarge);
    }

    // Bytes already buffered past the header terminator are the body's start.
    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    // A client that pipelined extra bytes is not our concern (one request per
    // connection); trim to the declared length.
    body.truncate(content_length);
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let want = remaining.min(chunk.len());
        let n = stream
            .read(&mut chunk[..want])
            .await
            .map_err(ReadError::Io)?;
        if n == 0 {
            return Err(ReadError::Malformed(
                "connection closed before body completed".into(),
            ));
        }
        body.extend_from_slice(&chunk[..n]);
    }

    Ok(Request {
        path,
        headers,
        body,
    })
}

/// Whether the `Authorization` header carries the expected Bearer token
/// (constant-time compared to avoid a timing side channel).
fn bearer_matches(headers: &[(String, String)], expected: &SecretString) -> bool {
    headers
        .iter()
        .find(|(n, _)| n == "authorization")
        .and_then(|(_, v)| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|token| constant_time_eq(token.as_bytes(), expected.expose().as_bytes()))
        .unwrap_or(false)
}

/// Length-checked constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Normalize a JSON body into an [`AgentSignal`].
///
/// Only `job_id` is mandatory (E-09); everything else is best-effort and the
/// full body is kept verbatim in `payload` for the audit trail and for the
/// engine (#138) to re-interpret. Rich state-machine interpretation is out of
/// scope here.
///
/// `pub(crate)` so the engine's spool-replay path
/// ([`Engine::replay_spool`](crate::run::Engine)) normalizes a spooled NDJSON
/// line through the *same* code as a live POST — one canonical wire contract,
/// no drift.
pub(crate) fn parse_signal(body: &[u8]) -> Result<AgentSignal, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "JSON body must be an object".to_string())?;

    let job_id_str = obj
        .get("job_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing or non-string `job_id`".to_string())?;
    let job_id: JobId = job_id_str
        .parse()
        .map_err(|e| format!("unparseable `job_id`: {e}"))?;

    let tool_session_id = str_field(obj, "session_id").unwrap_or_default();
    let prompt_id = str_field(obj, "prompt_id").unwrap_or_default();
    let event = normalize_event(obj, &tool_session_id);

    Ok(AgentSignal {
        source: SignalSource::AgentHook,
        job_id,
        tool_session_id,
        prompt_id,
        event,
        payload: value,
    })
}

/// Derive a [`SignalEvent`] from the hook body. Unknown/absent event names map
/// to [`SignalEvent::Heartbeat`] — the most non-committal outcome, so a
/// forward-compatible future hook type never triggers a false completion.
fn normalize_event(
    obj: &serde_json::Map<String, serde_json::Value>,
    session_id: &str,
) -> SignalEvent {
    match str_field(obj, "hook_event_name").as_deref() {
        Some("Stop") => {
            // An intermediate Stop with background work still running only
            // proves liveness (#131 D-12).
            let has_background = obj
                .get("background_tasks")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty());
            if has_background {
                return SignalEvent::Heartbeat;
            }
            SignalEvent::Stop {
                status: parse_status(str_field(obj, "status").as_deref()),
                reason: str_field(obj, "reason"),
                last_assistant_message: str_field(obj, "last_assistant_message"),
                transcript_path: str_field(obj, "transcript_path"),
            }
        }
        Some("Notification") => SignalEvent::Notification {
            message: str_field(obj, "message"),
        },
        Some("QuestionPending") => SignalEvent::QuestionPending {
            message: str_field(obj, "message"),
        },
        Some("SessionStart") => SignalEvent::SessionStart {
            tool_session_id: session_id.to_string(),
        },
        Some("SessionEnd") => SignalEvent::SessionEnd {
            reason: str_field(obj, "reason"),
        },
        _ => SignalEvent::Heartbeat,
    }
}

/// Map an explicit `status` marker string to a [`StopStatus`]; anything else is
/// [`StopStatus::Unknown`].
fn parse_status(status: Option<&str>) -> StopStatus {
    match status.map(str::to_ascii_lowercase).as_deref() {
        Some("completed") => StopStatus::Completed,
        Some("needs_input") => StopStatus::NeedsInput,
        Some("failed") => StopStatus::Failed,
        _ => StopStatus::Unknown,
    }
}

/// A non-empty string field from a JSON object, or `None`.
fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The index of the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Write a `200` response with a JSON body (the `/focus` outcome) and close
/// the write half.
async fn write_json_response(stream: &mut UnixStream, body: &str) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

/// Write a minimal `Connection: close` HTTP/1.1 response and close the write
/// half.
async fn write_response(stream: &mut UnixStream, status: u16, reason: &str) -> io::Result<()> {
    let body = format!("{status} {reason}\n");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    // Half-close so the client sees EOF promptly.
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The wire contract must stay identical on both sides: a payload shaped
    /// exactly like the one `on-stop.sh` emits (`hook_event_name` + uppercase
    /// `status` + `background_tasks`) must normalize to the right
    /// `SignalEvent`/`StopStatus`. This is the cross-issue guard that a rename
    /// on either side (script or parser) can never silently diverge again.
    #[test]
    fn on_stop_sh_completed_payload_deserializes_to_stop_completed() {
        // Byte-for-byte the object `on-stop.sh`'s `stop_payload COMPLETED` jq
        // filter produces for a final Stop with a COMPLETED marker.
        let body = br#"{"job_id":"job-42-7","session_id":"cc-1","prompt_id":"p-1","hook_event_name":"Stop","ts":"2026-07-18T00:00:00Z","status":"COMPLETED","reason":"","last_assistant_message":"done <<STATUS:COMPLETED>>","transcript_path":"/t.jsonl","background_tasks":[]}"#;
        let sig = parse_signal(body).expect("on-stop.sh payload must parse");
        assert_eq!(sig.job_id, JobId::new(42, 7));
        assert_eq!(sig.tool_session_id, "cc-1");
        assert_eq!(sig.prompt_id, "p-1");
        match sig.event {
            SignalEvent::Stop {
                status: StopStatus::Completed,
                last_assistant_message,
                transcript_path,
                ..
            } => {
                assert_eq!(
                    last_assistant_message.as_deref(),
                    Some("done <<STATUS:COMPLETED>>")
                );
                assert_eq!(transcript_path.as_deref(), Some("/t.jsonl"));
            }
            other => panic!("expected Stop/Completed, got {other:?}"),
        }
    }

    /// The heartbeat shape `on-stop.sh` emits for an intermediate Stop:
    /// `hook_event_name: "Stop"` with a non-empty `background_tasks`.
    #[test]
    fn on_stop_sh_heartbeat_payload_deserializes_to_heartbeat() {
        let body = br#"{"job_id":"job-1-2","session_id":"s","prompt_id":"","hook_event_name":"Stop","ts":"t","status":"","reason":"","last_assistant_message":"working <<STATUS:COMPLETED>>","transcript_path":"","background_tasks":[{"id":"bg1"}]}"#;
        let sig = parse_signal(body).expect("heartbeat payload must parse");
        assert!(
            matches!(sig.event, SignalEvent::Heartbeat),
            "non-empty background_tasks ⇒ heartbeat, got {:?}",
            sig.event
        );
    }

    /// The uppercase `NEEDS_INPUT`/`FAILED`/`UNKNOWN` the script emits map onto
    /// the right status (the parser lower-cases, so casing never matters).
    #[test]
    fn on_stop_sh_status_casing_is_tolerated() {
        for (raw, want) in [
            ("NEEDS_INPUT", StopStatus::NeedsInput),
            ("FAILED", StopStatus::Failed),
            ("UNKNOWN", StopStatus::Unknown),
        ] {
            let body =
                format!(r#"{{"job_id":"job-3-4","hook_event_name":"Stop","status":"{raw}"}}"#);
            let sig = parse_signal(body.as_bytes()).unwrap();
            match sig.event {
                SignalEvent::Stop { status, .. } => assert_eq!(status, want, "raw {raw}"),
                other => panic!("expected Stop, got {other:?}"),
            }
        }
    }

    /// The other three scripts' `hook_event_name` values normalize correctly.
    #[test]
    fn on_session_and_notification_payloads_deserialize() {
        let notif = parse_signal(
            br#"{"job_id":"job-1-1","session_id":"s","hook_event_name":"Notification","message":"grant permission?"}"#,
        )
        .unwrap();
        assert!(matches!(
            notif.event,
            SignalEvent::Notification { message: Some(_) }
        ));
        let start = parse_signal(
            br#"{"job_id":"job-1-1","session_id":"cc-9","hook_event_name":"SessionStart","source":"startup"}"#,
        )
        .unwrap();
        match start.event {
            SignalEvent::SessionStart { tool_session_id } => {
                assert_eq!(tool_session_id, "cc-9")
            }
            other => panic!("expected SessionStart, got {other:?}"),
        }
        let end = parse_signal(
            br#"{"job_id":"job-1-1","session_id":"cc-9","hook_event_name":"SessionEnd","reason":"clear"}"#,
        )
        .unwrap();
        assert!(matches!(
            end.event,
            SignalEvent::SessionEnd { reason: Some(_) }
        ));
    }

    /// The shape `on-ask-user-question.sh` (claude PreToolUse) and
    /// `totsuka-opencode.js` (`tool.execute.before`) emit for an open question
    /// dialog (#487). The per-question `prompt_id` must survive — it is the
    /// idempotency-key component that keeps a second question from being
    /// dropped as a duplicate of the first.
    #[test]
    fn question_pending_payload_deserializes_with_its_prompt_id() {
        let sig = parse_signal(
            br#"{"job_id":"job-42-7","session_id":"cc-1","prompt_id":"toolu_01","hook_event_name":"QuestionPending","ts":"t","message":"Approve completion?"}"#,
        )
        .unwrap();
        assert_eq!(sig.prompt_id, "toolu_01");
        match sig.event {
            SignalEvent::QuestionPending { message } => {
                assert_eq!(message.as_deref(), Some("Approve completion?"))
            }
            other => panic!("expected QuestionPending, got {other:?}"),
        }
        // An empty message is carried as None, same as Notification.
        let bare = parse_signal(
            br#"{"job_id":"job-42-7","hook_event_name":"QuestionPending","message":""}"#,
        )
        .unwrap();
        assert!(matches!(
            bare.event,
            SignalEvent::QuestionPending { message: None }
        ));
        // Forward compatibility still holds: an unknown event name from some
        // future script degrades to a liveness bump, never a state change.
        let future =
            parse_signal(br#"{"job_id":"job-42-7","hook_event_name":"QuestionAnswered"}"#).unwrap();
        assert!(matches!(future.event, SignalEvent::Heartbeat));
    }

    /// A [`SignalPort`] fake that records every submitted signal.
    #[derive(Clone, Default)]
    struct RecordingSink {
        received: Arc<Mutex<Vec<AgentSignal>>>,
    }

    impl RecordingSink {
        fn signals(&self) -> Vec<AgentSignal> {
            self.received.lock().unwrap().clone()
        }
    }

    impl SignalPort for RecordingSink {
        fn submit(
            &self,
            signal: AgentSignal,
        ) -> impl std::future::Future<
            Output = Result<
                crate::ports::signal_ingress::SignalAck,
                crate::ports::signal_ingress::SignalError,
            >,
        > + Send {
            self.received.lock().unwrap().push(signal);
            async move { Ok(crate::ports::signal_ingress::SignalAck) }
        }
    }

    /// A [`FocusPort`] fake: records the asked task ids and answers a canned
    /// outcome (`focused: id > 0`, so tests can drive both answers).
    #[derive(Clone, Default)]
    struct RecordingFocus {
        asked: Arc<Mutex<Vec<i64>>>,
    }

    impl FocusPort for RecordingFocus {
        fn focus(
            &self,
            task_id: i64,
        ) -> impl std::future::Future<
            Output = Result<
                crate::ports::signal_ingress::FocusOutcome,
                crate::ports::signal_ingress::SignalError,
            >,
        > + Send {
            self.asked.lock().unwrap().push(task_id);
            let outcome = if task_id > 0 {
                crate::ports::signal_ingress::FocusOutcome::focused()
            } else {
                crate::ports::signal_ingress::FocusOutcome::not("pane is gone")
            };
            async move { Ok(outcome) }
        }
    }

    /// A unique, short socket path under the system temp dir (macOS caps
    /// `sun_path` at ~104 bytes, so the name is kept small).
    fn temp_socket() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("tsk-hook-{}-{}.sock", std::process::id(), n))
    }

    /// Spawn a server bound to a fresh socket; return the socket path, the
    /// sink, the focus fake, and the shutdown handle.
    fn spawn_server(
        auth_token: Option<SecretString>,
    ) -> (
        PathBuf,
        RecordingSink,
        RecordingFocus,
        watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket_path = temp_socket();
        let listener = bind(&socket_path).expect("bind");
        let sink = RecordingSink::default();
        let focus = RecordingFocus::default();
        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = tokio::spawn(serve(
            listener,
            socket_path.clone(),
            sink.clone(),
            focus.clone(),
            auth_token,
            stop_rx,
        ));
        (socket_path, sink, focus, stop_tx, handle)
    }

    /// Send a raw request over a fresh connection and return the status line.
    async fn send_raw(socket_path: &Path, request: &[u8]) -> String {
        let mut stream = UnixStream::connect(socket_path).await.expect("connect");
        stream.write_all(request).await.expect("write");
        stream.flush().await.expect("flush");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let text = String::from_utf8_lossy(&response);
        text.lines().next().unwrap_or_default().to_string()
    }

    /// Build a POST with an optional Bearer header and the given JSON body.
    fn post(bearer: Option<&str>, body: &str) -> Vec<u8> {
        let auth = bearer
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        format!(
            "POST /agent-events HTTP/1.1\r\n\
             Host: localhost\r\n\
             {auth}\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             \r\n\
             {body}",
            len = body.len(),
        )
        .into_bytes()
    }

    async fn stop(stop_tx: watch::Sender<bool>, handle: tokio::task::JoinHandle<()>) {
        let _ = stop_tx.send(true);
        let _ = handle.await;
    }

    #[test]
    fn bind_refuses_to_remove_a_non_socket_file() {
        let path = temp_socket();
        // A regular file (e.g. a misconfigured socket_path) must never be deleted.
        std::fs::write(&path, b"important").expect("seed file");
        let err = bind(&path).expect_err("bind must refuse a non-socket path");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&path).expect("file still present"),
            b"important",
            "bind must not touch a non-socket file"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn bind_replaces_a_stale_socket() {
        let path = temp_socket();
        let first = bind(&path).expect("first bind");
        drop(first); // leaves the socket file on disk
        // A stale socket left by a previous run is unlinked and rebound cleanly.
        let _second = bind(&path).expect("second bind replaces the stale socket");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn valid_post_returns_200_and_submits() {
        let (socket, sink, _focus, stop_tx, handle) =
            spawn_server(Some(SecretString::new("t0ken")));
        let body = r#"{"job_id":"job-42-7","session_id":"s1","hook_event_name":"Stop","status":"completed"}"#;
        let status = send_raw(&socket, &post(Some("t0ken"), body)).await;
        assert!(status.contains("200"), "status was {status:?}");

        let signals = sink.signals();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].job_id, JobId::new(42, 7));
        assert_eq!(signals[0].tool_session_id, "s1");
        assert!(matches!(
            signals[0].event,
            SignalEvent::Stop {
                status: StopStatus::Completed,
                ..
            }
        ));
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn bearer_mismatch_returns_401_and_does_not_submit() {
        let (socket, sink, _focus, stop_tx, handle) =
            spawn_server(Some(SecretString::new("right")));
        let body = r#"{"job_id":"job-1-1","hook_event_name":"Stop"}"#;
        let status = send_raw(&socket, &post(Some("wrong"), body)).await;
        assert!(status.contains("401"), "status was {status:?}");
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn missing_bearer_returns_401() {
        let (socket, sink, _focus, stop_tx, handle) =
            spawn_server(Some(SecretString::new("right")));
        let body = r#"{"job_id":"job-1-1","hook_event_name":"Stop"}"#;
        let status = send_raw(&socket, &post(None, body)).await;
        assert!(status.contains("401"), "status was {status:?}");
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn malformed_json_returns_400() {
        let (socket, sink, _focus, stop_tx, handle) = spawn_server(None);
        let status = send_raw(&socket, &post(None, "{not json")).await;
        assert!(status.contains("400"), "status was {status:?}");
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn missing_job_id_returns_400() {
        let (socket, sink, _focus, stop_tx, handle) = spawn_server(None);
        let status = send_raw(&socket, &post(None, r#"{"hook_event_name":"Stop"}"#)).await;
        assert!(status.contains("400"), "status was {status:?}");
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn unknown_fields_are_accepted_with_200() {
        let (socket, sink, _focus, stop_tx, handle) = spawn_server(None);
        // A future hook adds fields we do not know: E-08 says accept and keep
        // them verbatim in the payload.
        let body = r#"{"job_id":"job-9-9","hook_event_name":"Stop","future_field":{"nested":true},"another":42}"#;
        let status = send_raw(&socket, &post(None, body)).await;
        assert!(status.contains("200"), "status was {status:?}");

        let signals = sink.signals();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].payload["future_field"]["nested"], true);
        assert_eq!(signals[0].payload["another"], 42);
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn oversize_body_returns_413() {
        let (socket, sink, _focus, stop_tx, handle) = spawn_server(None);
        // Declare a Content-Length above the cap; the server refuses before
        // reading the body.
        let request = format!(
            "POST /agent-events HTTP/1.1\r\nContent-Length: {len}\r\n\r\n",
            len = MAX_BODY_BYTES + 1,
        );
        let status = send_raw(&socket, request.as_bytes()).await;
        assert!(status.contains("413"), "status was {status:?}");
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn duplicate_submits_are_stateless() {
        // Idempotency is the DB layer's job (D-05): two identical POSTs both
        // return 200 and both submit — the adapter holds no dedup state.
        let (socket, sink, _focus, stop_tx, handle) = spawn_server(None);
        let body = r#"{"job_id":"job-3-4","hook_event_name":"Stop","status":"completed"}"#;
        assert!(send_raw(&socket, &post(None, body)).await.contains("200"));
        assert!(send_raw(&socket, &post(None, body)).await.contains("200"));
        assert_eq!(sink.signals().len(), 2);
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn socket_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (socket, _sink, _focus, stop_tx, handle) = spawn_server(None);
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "socket mode was {:o}", mode & 0o777);
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn shutdown_unlinks_the_socket() {
        let (socket, _sink, _focus, stop_tx, handle) = spawn_server(None);
        assert!(socket.exists());
        stop(stop_tx, handle).await;
        assert!(!socket.exists(), "socket should be unlinked on shutdown");
    }

    /// Send a raw request and return the **whole** response text (status line
    /// + headers + body), for the `/focus` outcome-body assertions.
    async fn send_raw_full(socket_path: &Path, request: &[u8]) -> String {
        let mut stream = UnixStream::connect(socket_path).await.expect("connect");
        stream.write_all(request).await.expect("write");
        stream.flush().await.expect("flush");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Build a `POST /focus` control request.
    fn focus_post(bearer: Option<&str>, body: &str) -> Vec<u8> {
        let auth = bearer
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        format!(
            "POST /focus HTTP/1.1\r\n\
             Host: localhost\r\n\
             {auth}\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             \r\n\
             {body}",
            len = body.len(),
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn focus_route_answers_the_outcome_as_json() {
        let (socket, sink, focus, stop_tx, handle) = spawn_server(None);
        let response = send_raw_full(&socket, &focus_post(None, r#"{"task_id":42}"#)).await;
        assert!(response.contains("200"), "response was {response:?}");
        assert!(
            response.contains(r#"{"focused":true}"#),
            "response was {response:?}"
        );
        assert_eq!(focus.asked.lock().unwrap().as_slice(), &[42]);
        // A control request is never a signal.
        assert!(sink.signals().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn focus_route_accepts_a_numeric_string_task_id() {
        // The notifier's click_command template renders the id as text.
        let (socket, _sink, focus, stop_tx, handle) = spawn_server(None);
        let response = send_raw_full(&socket, &focus_post(None, r#"{"task_id":"7"}"#)).await;
        assert!(response.contains("200"), "response was {response:?}");
        assert_eq!(focus.asked.lock().unwrap().as_slice(), &[7]);
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn focus_route_reports_a_degraded_outcome_with_its_reason() {
        // The fake answers `focused: false` for non-positive ids — the reply is
        // still 200 (degradation is a normal answer, F-94).
        let (socket, _sink, _focus, stop_tx, handle) = spawn_server(None);
        let response = send_raw_full(&socket, &focus_post(None, r#"{"task_id":0}"#)).await;
        assert!(response.contains("200"), "response was {response:?}");
        assert!(
            response.contains(r#""focused":false"#) && response.contains("pane is gone"),
            "response was {response:?}"
        );
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn focus_route_requires_the_bearer_token() {
        // The control endpoint sits behind the same auth as signal ingestion
        // (E-03): no token, no focus.
        let (socket, _sink, focus, stop_tx, handle) =
            spawn_server(Some(SecretString::new("right")));
        let status = send_raw(&socket, &focus_post(None, r#"{"task_id":1}"#)).await;
        assert!(status.contains("401"), "status was {status:?}");
        assert!(focus.asked.lock().unwrap().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn focus_route_rejects_a_missing_task_id() {
        let (socket, _sink, focus, stop_tx, handle) = spawn_server(None);
        let status = send_raw(&socket, &focus_post(None, r#"{"job_id":"job-1-1"}"#)).await;
        assert!(status.contains("400"), "status was {status:?}");
        assert!(focus.asked.lock().unwrap().is_empty());
        stop(stop_tx, handle).await;
    }

    #[tokio::test]
    async fn non_focus_paths_stay_signal_ingestion() {
        // E-08: the path is inspected only for the exact `/focus`; any other
        // path (today's `/agent-events`, a future one) is signal ingestion.
        let (socket, sink, focus, stop_tx, handle) = spawn_server(None);
        let body = r#"{"job_id":"job-5-6","hook_event_name":"Stop","status":"completed"}"#;
        let request = format!(
            "POST /some/future/path HTTP/1.1\r\nContent-Length: {len}\r\n\r\n{body}",
            len = body.len(),
        );
        let status = send_raw(&socket, request.as_bytes()).await;
        assert!(status.contains("200"), "status was {status:?}");
        assert_eq!(sink.signals().len(), 1);
        assert!(focus.asked.lock().unwrap().is_empty());
        stop(stop_tx, handle).await;
    }

    /// Real-client smoke test. Run locally with:
    /// `cargo test -p orchestrator-core hook_uds -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "requires the `curl` binary; run manually"]
    async fn curl_unix_socket_smoke() {
        let (socket, sink, _focus, stop_tx, handle) =
            spawn_server(Some(SecretString::new("smoke")));
        let output = tokio::process::Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--unix-socket")
            .arg(&socket)
            .arg("-H")
            .arg("Authorization: Bearer smoke")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("--data")
            .arg(r#"{"job_id":"job-7-1","hook_event_name":"Stop","status":"completed"}"#)
            .arg("http://localhost/agent-events")
            .output()
            .await
            .expect("run curl");
        assert!(output.status.success(), "curl failed: {output:?}");
        assert_eq!(sink.signals().len(), 1);
        stop(stop_tx, handle).await;
    }
}
