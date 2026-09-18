//! Errors from driving the orca CLI.

/// An error running or interpreting an `orca` CLI invocation.
#[derive(Debug, thiserror::Error)]
pub enum OrcaError {
    /// The `orca` binary could not be spawned (not installed / wrong path).
    #[error(
        "cannot run the orca CLI (`{bin}`) → is orca installed and on PATH? set `orca_bin` in `[orca]` of config.toml: {source}"
    )]
    Spawn {
        /// The binary we tried to run.
        bin: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// One invocation outlived `request_timeout_secs` and was killed.
    #[error("`orca {0}` timed out → is the Orca app running? (`orca status`)")]
    Timeout(String),
    /// orca answered with `ok: false` — the code is orca's own
    /// (`selector_not_found`, `terminal_handle_stale`, …).
    #[error("orca error ({code}): {message}")]
    Orca {
        /// orca's `error.code`.
        code: String,
        /// orca's `error.message`.
        message: String,
    },
    /// `orca` exited non-zero without printing an envelope.
    #[error("orca exited with code {code}: {stderr}")]
    CliFailed {
        /// Process exit code (or -1 if terminated by signal).
        code: i32,
        /// Captured stderr (truncated).
        stderr: String,
    },
    /// The `--json` output could not be parsed.
    #[error("orca --json output was not valid JSON: {0}")]
    InvalidJson(String),
    /// The output was valid JSON but not the shape we expected.
    #[error("orca returned an unexpected response: {0}")]
    InvalidResponse(String),
    /// orca does not know the dispatch worktree, so no terminal can be opened
    /// in it. orca discovers a registered repository's git worktrees on its
    /// own — including the ones totsuka cuts — so this means the repository
    /// is not registered.
    #[error(
        "orca does not know the worktree {path} → register its repository with \
         `orca repo add --path <repository>` (orca then sees every worktree of it)"
    )]
    WorktreeUnknown {
        /// The dispatch worktree path.
        path: String,
    },
    /// `task/dispatch` arrived without a `tool_launch` (see the herdr plugin's
    /// identical error, #411).
    #[error(
        "task/dispatch carried no `tool_launch`: this plugin launches exactly the argv the \
         Orchestrator resolves and has no local fallback, so there is nothing to launch. This \
         is an Orchestrator-side tool resolution failure — check `[tools]` / `default_tool`."
    )]
    MissingToolLaunch,
    /// The resumed session could not be brought back (→ `SESSION_UNRESUMABLE`).
    #[error("the agent session could not be resumed: {0}")]
    SessionUnresumable(String),
}

impl OrcaError {
    /// orca's error code, when orca answered with one.
    fn code(&self) -> Option<&str> {
        match self {
            OrcaError::Orca { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Whether the referenced terminal (or worktree) no longer exists.
    ///
    /// `terminal_handle_stale` is what orca answers for a handle it has no
    /// record of — measured with a made-up handle — and the `*_not_found`
    /// family covers selectors. An *exited* terminal is **not** missing: orca
    /// keeps its record (`connected: false`), see [`is_exited`](Self::is_exited).
    pub fn is_missing(&self) -> bool {
        self.code().is_some_and(|code| {
            code == "terminal_handle_stale" || code == "not_found" || code.ends_with("_not_found")
        })
    }

    /// Whether the terminal's process has exited (`terminal_exited`): its
    /// record is still there, but nothing runs in it any more.
    pub fn is_exited(&self) -> bool {
        self.code() == Some("terminal_exited")
    }

    /// Whether the terminal is gone in either sense — no record, or no process.
    pub fn is_gone(&self) -> bool {
        self.is_missing() || self.is_exited()
    }

    /// Whether a `terminal wait` ran out of its own `--timeout-ms`.
    pub fn is_wait_timeout(&self) -> bool {
        self.code() == Some("timeout")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orca(code: &str) -> OrcaError {
        OrcaError::Orca {
            code: code.into(),
            message: String::new(),
        }
    }

    #[test]
    fn gone_covers_a_stale_handle_and_an_exited_process_but_not_a_timeout() {
        assert!(orca("terminal_handle_stale").is_missing());
        assert!(orca("selector_not_found").is_missing());
        assert!(!orca("terminal_exited").is_missing());
        assert!(orca("terminal_exited").is_gone());
        assert!(!orca("timeout").is_gone());
        assert!(orca("timeout").is_wait_timeout());
        // A bare CLI failure says nothing about the terminal.
        let bare = OrcaError::CliFailed {
            code: 1,
            stderr: "terminal not found".into(),
        };
        assert!(!bare.is_gone());
    }
}
