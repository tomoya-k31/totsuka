//! The orca CLI seam: the boundary between the adapter logic and the real
//! `orca` subprocess.
//!
//! [`OrcaAgent`](crate::agent::OrcaAgent) is generic over [`OrcaCli`] so its
//! dispatch/attach/cancel/state logic is tested against a fake, while production
//! shells out to the real `orca` binary with `--json`.

use std::future::Future;

use serde_json::Value;

use crate::error::OrcaError;

/// Runs an `orca` subcommand and returns its parsed `--json` output.
pub trait OrcaCli: Clone + Send + Sync + 'static {
    /// Run `orca <args>` and parse stdout as JSON. `args` should already include
    /// the subcommand and `--json`.
    fn run(&self, args: Vec<String>) -> impl Future<Output = Result<Value, OrcaError>> + Send;
}

/// The production CLI: spawns the real `orca` binary.
#[derive(Clone)]
pub struct ProcessCli {
    bin: String,
}

impl ProcessCli {
    /// A CLI driver invoking `bin` (a program name on PATH or an absolute path).
    pub fn new(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }
}

impl OrcaCli for ProcessCli {
    async fn run(&self, args: Vec<String>) -> Result<Value, OrcaError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(&args)
            .output()
            .await
            .map_err(|source| OrcaError::Spawn {
                bin: self.bin.clone(),
                source,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(500)
                .collect();
            return Err(OrcaError::CliFailed {
                code: output.status.code().unwrap_or(-1),
                stderr,
            });
        }
        // Some commands (e.g. a bare success) emit no JSON; treat as null.
        let stdout = output.stdout.trim_ascii();
        if stdout.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(stdout).map_err(|e| OrcaError::InvalidJson(e.to_string()))
    }
}
