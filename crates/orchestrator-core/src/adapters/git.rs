//! Real [`GitRunner`] that shells out to the `git` binary.

use std::path::Path;
use std::process::Command;

use crate::ports::git::{GitOutput, GitRunner};

/// A [`GitRunner`] backed by the system `git`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGitRunner;

impl GitRunner for SystemGitRunner {
    fn run(&self, cwd: &Path, args: &[&str]) -> std::io::Result<GitOutput> {
        let output = Command::new("git").current_dir(cwd).args(args).output()?;
        Ok(GitOutput {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
