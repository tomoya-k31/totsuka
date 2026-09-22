//! Real [`GitRunner`] that shells out to the `git` binary.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ports::git::{GitOutput, GitRunner};

/// Default upper bound on a single git invocation (#764);
/// `[worktree].git_timeout_secs` overrides it.
///
/// Every git call runs synchronously on the `Engine` loop, so a git that never
/// returns stops dispatch, completion signals, the timeout sweep and health
/// publishing all at once — with nothing left to notice. The case that made it
/// real is `git fetch` over SSH after the host woke from sleep: ssh waited on
/// a dead connection for six hours. Past the deadline the whole process group
/// is killed and the call fails as [`std::io::ErrorKind::TimedOut`], which a
/// dispatch turns into its ordinary failure → automatic requeue.
///
/// Generous on purpose: it caps a hang, it does not police a slow-but-live
/// fetch. A dead SSH connection is better caught earlier, and precisely, by
/// the client's own `ServerAliveInterval` (→ operations guide), which this must
/// stay longer than so ssh's clearer error wins when both apply.
pub const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// A [`GitRunner`] backed by the system `git`.
#[derive(Debug, Clone, Copy)]
pub struct SystemGitRunner {
    timeout: Duration,
}

impl SystemGitRunner {
    /// A runner that kills a git still running after `timeout`. Zero means
    /// no limit — the same "`0` turns it off" as `timeout_secs`.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SystemGitRunner {
    fn default() -> Self {
        Self::with_timeout(DEFAULT_GIT_TIMEOUT)
    }
}

impl GitRunner for SystemGitRunner {
    fn run(&self, cwd: &Path, args: &[&str]) -> std::io::Result<GitOutput> {
        run_with_timeout(cwd, args, self.timeout)
    }
}

/// Run `git <args>` in `cwd`, killing it (and everything it spawned) once
/// `timeout` has passed (never, when it is zero).
fn run_with_timeout(cwd: &Path, args: &[&str], timeout: Duration) -> std::io::Result<GitOutput> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        // Nobody is there to answer a credential prompt, and in its own
        // process group git cannot read the terminal anyway — it would stop
        // on SIGTTIN and sit out the whole timeout. Fail at once instead.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own group, so the kill below also takes the `ssh` git spawned: ssh
    // holds git's stderr, and killing git alone would leave the read of that
    // pipe blocked for as long as ssh lives.
    #[cfg(unix)]
    crate::platform::unix::own_process_group(&mut cmd);
    let mut child = cmd.spawn()?;

    let deadline = (!timeout.is_zero()).then(|| Instant::now() + timeout);
    let (done, finished) = mpsc::channel();
    let stdout = drain(child.stdout.take(), done.clone());
    let stderr = drain(child.stderr.take(), done);
    // Both pipes reach EOF once git and all its children have exited — the
    // same point `Command::output()` returns at, so a finished git is not
    // held back by this.
    let timed_out = (0..2).any(|_| match deadline {
        Some(deadline) => finished
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err(),
        None => finished.recv().is_err(),
    });
    if timed_out {
        kill(&mut child);
    }
    let status = child.wait()?;
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if timed_out {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "`git {}` did not finish within {}s and was killed → check network and \
                 remote access; a dead SSH connection is the usual cause (set \
                 `ServerAliveInterval` in ~/.ssh/config)",
                args.join(" "),
                timeout.as_secs(),
            ),
        ));
    }
    Ok(GitOutput {
        status: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Read `pipe` to EOF on its own thread, signalling `done` when it gets there.
fn drain<R: Read + Send + 'static>(pipe: Option<R>, done: mpsc::Sender<()>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = done.send(());
        buf
    })
}

fn kill(child: &mut Child) {
    #[cfg(unix)]
    crate::platform::unix::kill_process_group(child.id());
    #[cfg(not(unix))]
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_that_never_finishes_is_killed_at_the_deadline() {
        // `core.sshCommand` stands in for an ssh stuck on a dead connection:
        // git hands it to a shell, the `#` comments out the host and command
        // git appends, and what is left sleeps far past the deadline. It is
        // git's *child*, so this also proves the group kill reaches it — kill
        // git alone and the sleep keeps stderr open for the full 30s.
        let started = Instant::now();
        let err = run_with_timeout(
            &std::env::temp_dir(),
            &[
                "-c",
                "core.sshCommand=sleep 30 #",
                "-c",
                "ssh.variant=simple",
                "ls-remote",
                "ssh://totsuka.invalid/repo.git",
            ],
            Duration::from_secs(1),
        )
        .expect_err("a hung git must fail, not wait");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "returned only after {:?}",
            started.elapsed()
        );
    }
}
