//! The orca CLI seam: the boundary between the adapter logic and the real
//! `orca` subprocess.
//!
//! [`OrcaAgent`](crate::agent::OrcaAgent) is generic over [`OrcaCli`] so its
//! dispatch/attach/cancel/state logic is tested against a fake, while production
//! shells out to the real `orca` binary with `--json`.
//!
//! # The `--json` envelope (verified live on orca 1.4.205)
//!
//! Every `--json` answer is one object on stdout:
//!
//! ```text
//! {"id": "<request uuid>", "ok": true,  "result": {…}, "_meta": {…}}
//! {"id": "<request uuid>", "ok": false, "error": {"code": "…", "message": "…", "data": …}}
//! ```
//!
//! A refusal still prints its envelope **on stdout**, with exit status 1, so the
//! exit status alone does not say what went wrong — `error.code` does
//! (`selector_not_found`, `terminal_handle_stale`, `terminal_exited`,
//! `timeout`, …). [`ProcessCli`] unwraps the envelope, so callers see
//! `result` or an [`OrcaError::Orca`] carrying that code, and never the
//! top-level `id`: that is the id of the **CLI request**, which the plugin
//! before this rewrite mistook for a worktree id.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::OrcaError;

/// Runs an `orca` subcommand and returns its unwrapped `--json` result.
pub trait OrcaCli: Clone + Send + Sync + 'static {
    /// Run `orca <args>` and return the envelope's `result`. `args` should
    /// already include the subcommand and `--json`.
    fn run(&self, args: Vec<String>) -> impl Future<Output = Result<Value, OrcaError>> + Send;
}

/// The production CLI: spawns the real `orca` binary.
#[derive(Clone)]
pub struct ProcessCli {
    bin: String,
    timeout: Duration,
}

/// Extra time a command that blocks on purpose is given beyond its own
/// timeout, so orca's own answer arrives before ours fires.
const WAIT_MARGIN: Duration = Duration::from_secs(10);

impl ProcessCli {
    /// A CLI driver invoking `bin` (a program name on PATH or an absolute path),
    /// killing any single invocation that runs longer than `timeout`.
    pub fn new(bin: impl Into<String>, timeout: Duration) -> Self {
        Self {
            bin: bin.into(),
            timeout,
        }
    }

    /// The deadline for one invocation: the configured timeout, stretched for
    /// a command that blocks on purpose so the plugin never cuts short a wait
    /// it asked for — `terminal wait --timeout-ms <ms>` and
    /// `terminal send --wait-submit <seconds>`.
    fn deadline_for(&self, args: &[String]) -> Duration {
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.parse::<u64>().ok())
        };
        let asked = value("--timeout-ms")
            .map(Duration::from_millis)
            .or_else(|| value("--wait-submit").map(Duration::from_secs))
            .map(|d| d + WAIT_MARGIN);
        match asked {
            Some(asked) => asked.max(self.timeout),
            None => self.timeout,
        }
    }
}

impl OrcaCli for ProcessCli {
    async fn run(&self, args: Vec<String>) -> Result<Value, OrcaError> {
        let command = args
            .iter()
            .take_while(|a| !a.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let child = tokio::process::Command::new(&self.bin)
            .args(&args)
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(self.deadline_for(&args), child)
            .await
            .map_err(|_| OrcaError::Timeout(command.clone()))?
            .map_err(|source| OrcaError::Spawn {
                bin: self.bin.clone(),
                source,
            })?;
        interpret(
            output.status.success(),
            output.status.code().unwrap_or(-1),
            &output.stdout,
            &output.stderr,
        )
    }
}

/// Turn one invocation's exit status and output into its result.
///
/// Split out of [`ProcessCli::run`] so the envelope rules are unit-tested
/// without spawning anything.
pub fn interpret(
    success: bool,
    code: i32,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Value, OrcaError> {
    let stdout = stdout.trim_ascii();
    // An envelope, whatever the exit status: a refusal prints one too, and its
    // `error.code` is worth more than the bare status.
    if let Ok(envelope) = serde_json::from_slice::<Value>(stdout)
        && let Some(ok) = envelope.get("ok").and_then(Value::as_bool)
    {
        if ok {
            return Ok(envelope.get("result").cloned().unwrap_or(Value::Null));
        }
        let error = envelope.get("error");
        let field = |name: &str| {
            error
                .and_then(|e| e.get(name))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        return Err(OrcaError::Orca {
            code: field("code"),
            message: field("message"),
        });
    }
    if !success {
        let stderr = String::from_utf8_lossy(stderr).chars().take(500).collect();
        return Err(OrcaError::CliFailed { code, stderr });
    }
    if stdout.is_empty() {
        return Ok(Value::Null);
    }
    // Valid JSON without an envelope: an orca older than the envelope, or a
    // command that never had one. Passed through rather than refused.
    serde_json::from_slice(stdout).map_err(|e| OrcaError::InvalidJson(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ok_envelope_yields_its_result_not_the_request_id() {
        let out = br#"{"id":"req-1","ok":true,"result":{"terminal":{"handle":"term_1"}}}"#;
        let result = interpret(true, 0, out, b"").unwrap();
        assert_eq!(result["terminal"]["handle"], "term_1");
        assert!(
            result.get("id").is_none(),
            "the CLI request id must not leak"
        );
    }

    #[test]
    fn a_refusal_carries_orcas_error_code_despite_the_exit_status() {
        let out = br#"{"id":"r","ok":false,"error":{"code":"terminal_handle_stale","message":"terminal_handle_stale"}}"#;
        match interpret(false, 1, out, b"") {
            Err(OrcaError::Orca { code, .. }) => assert_eq!(code, "terminal_handle_stale"),
            other => panic!("expected an Orca error, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_without_an_envelope_keeps_stderr() {
        match interpret(false, 2, b"", b"boom") {
            Err(OrcaError::CliFailed { code, stderr }) => {
                assert_eq!(code, 2);
                assert_eq!(stderr, "boom");
            }
            other => panic!("expected CliFailed, got {other:?}"),
        }
    }

    #[test]
    fn a_wait_is_given_its_own_timeout_plus_a_margin() {
        let cli = ProcessCli::new("orca", Duration::from_secs(30));
        let wait: Vec<String> = ["terminal", "wait", "--timeout-ms", "120000"]
            .map(String::from)
            .to_vec();
        assert_eq!(cli.deadline_for(&wait), Duration::from_secs(130));
        // `--wait-submit` is in seconds, and a prompt that takes longer than
        // `request_timeout_secs` to start a turn must not be killed mid-wait.
        let send: Vec<String> = ["terminal", "send", "--wait-submit", "60"]
            .map(String::from)
            .to_vec();
        assert_eq!(cli.deadline_for(&send), Duration::from_secs(70));
        let show: Vec<String> = ["terminal", "show"].map(String::from).to_vec();
        assert_eq!(cli.deadline_for(&show), Duration::from_secs(30));
    }
}
