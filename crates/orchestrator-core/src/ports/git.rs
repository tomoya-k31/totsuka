//! Git command runner port.
//!
//! All git invocations go through [`GitRunner`] so the worktree logic can be
//! unit-tested with a fake and the real implementation
//! ([`SystemGitRunner`](crate::adapters::git::SystemGitRunner)) shells out to
//! `git`.

use std::path::Path;

/// The captured result of a git invocation.
#[derive(Debug, Clone)]
pub struct GitOutput {
    /// Exit code (`None` if killed by a signal).
    pub status: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

impl GitOutput {
    /// Whether the command exited with status 0.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Runs `git` subcommands in a working directory.
pub trait GitRunner {
    /// Run `git <args>` with `cwd` as the working directory.
    fn run(&self, cwd: &Path, args: &[&str]) -> std::io::Result<GitOutput>;
}
