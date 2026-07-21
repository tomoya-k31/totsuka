//! `totsuka focus <task-id>` — bring the task's pane to the foreground
//! (F-94 click-to-focus).
//!
//! Pane focus must go through the **running** orchestrator: only it owns the
//! agent plugin subprocess that can decode the opaque `session_id` into a pane
//! handle (F-37), so the CLI POSTs `/focus` to the hook/control UDS and prints
//! the outcome.
//!
//! Degradation is deliberate and **quiet** (always exit 0): this command is
//! the notification's click target (terminal-notifier `-execute`), so a
//! stopped orchestrator, a missing config, or a vanished pane must not turn a
//! click into an error — the GUI app activation (`-activate`) has already
//! happened, and the pane focus is simply skipped with a short note.

use std::collections::HashMap;
use std::path::Path;

use crate::common::{CliError, Cx, hook_socket_path, secret_resolver};

/// Ask the running orchestrator to focus the task's pane. Never fails: every
/// degraded outcome is a printed note and a clean exit (see module docs).
pub fn run(cx: &Cx, task_id: i64) -> Result<(), CliError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cfg = match cx.load_config(&env) {
        Ok(cfg) => cfg,
        Err(e) => return skipped(format!("{e}")),
    };
    let socket = match hook_socket_path(cx, &cfg, &env) {
        Ok(path) => path,
        Err(e) => return skipped(format!("{e}")),
    };
    if !is_socket(&socket) {
        return skipped(format!(
            "orchestrator is not running (no socket at {})",
            socket.display()
        ));
    }
    // A configured-but-unresolvable token means the running receiver would
    // answer an unexplained 401 — name the real cause instead of trying bare.
    let token = match &cfg.hooks.auth_token_ref {
        Some(reference) => match secret_resolver(&env).resolve(reference) {
            Ok(secret) => Some(secret),
            Err(e) => {
                return skipped(format!(
                    "[hooks].auth_token_ref did not resolve ({e}) → fix the reference; \
                     the control endpoint rejects unauthenticated requests"
                ));
            }
        },
        None => None,
    };
    match post_focus(&socket, token.as_ref().map(|t| t.expose()), task_id) {
        Ok((200, body)) => report_outcome(task_id, &body),
        Ok((status, _)) => skipped(format!(
            "control endpoint at {} answered {status}",
            socket.display()
        )),
        Err(e) => skipped(format!("could not reach the orchestrator: {e}")),
    }
}

/// Print the engine's `{"focused": bool, "reason"?: string}` answer.
fn report_outcome(task_id: i64, body: &str) -> Result<(), CliError> {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    if parsed.get("focused").and_then(|v| v.as_bool()) == Some(true) {
        println!("task {task_id}: pane focused");
    } else {
        let reason = parsed
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown reason");
        println!("task {task_id}: pane not focused — {reason}");
    }
    Ok(())
}

/// The quiet degradation path: a note on stdout, exit 0 (the click already
/// activated the app; only the pane focus is skipped).
fn skipped(reason: String) -> Result<(), CliError> {
    println!("focus skipped: {reason}");
    Ok(())
}

/// Whether `path` is an existing Unix domain socket (a live receiver's).
#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_socket(_path: &Path) -> bool {
    false
}

/// POST `/focus` over the UDS and return `(status, body)`.
#[cfg(unix)]
fn post_focus(
    socket_path: &Path,
    token: Option<&str>,
    task_id: i64,
) -> std::io::Result<(u16, String)> {
    use std::io::{Read, Write};
    use std::time::Duration;

    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;
    // The engine answers as soon as its run loop processes the request; the
    // server side also enforces its own 10s whole-request deadline.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;

    let body = format!(r#"{{"task_id":{task_id}}}"#);
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /focus HTTP/1.1\r\n\
         Host: localhost\r\n\
         {auth}\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_response(&response).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no HTTP status line in reply",
        )
    })
}

#[cfg(not(unix))]
fn post_focus(
    _socket_path: &Path,
    _token: Option<&str>,
    _task_id: i64,
) -> std::io::Result<(u16, String)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the UDS control endpoint is only supported on Unix",
    ))
}

/// Split a raw HTTP/1.1 reply into its status code and body.
#[cfg(unix)]
fn parse_response(response: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(response);
    let status: u16 = text
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Some((status, body))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn parse_response_splits_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 16\r\n\r\n{\"focused\":true}";
        let (status, body) = parse_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"focused":true}"#);
    }

    #[test]
    fn parse_response_rejects_garbage() {
        assert!(parse_response(b"not http").is_none());
    }
}
