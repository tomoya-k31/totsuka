//! Handing the launch env to the agent through a named pipe (FIFO) instead of
//! typing it into the terminal (#744).
//!
//! orca's `terminal create --command` types its text into the operator's
//! shell (see [`crate::launch`]), so an `env K=V …` prefix left every value —
//! the hook token, the whole prompt context, any secret an `env_file` adds —
//! on the screen and in the scrollback. Instead:
//!
//! 1. [`EnvHandoff::start`] makes a `0600` FIFO in this process's `0700`
//!    directory under totsuka's runtime dir and starts writing the env to it as
//!    `export K='v'` lines — the write completes once a reader opens it.
//! 2. The terminal gets only a command that reads the FIFO, deletes it, and
//!    `exec`s the agent ([`crate::launch::launch_command`]).
//!
//! The values pass through the kernel's pipe buffer only — nothing is written
//! to disk — and the FIFO is gone by the time the agent runs. A reader that
//! never shows up (the terminal failed, an rc file stopped, the tab was closed
//! first) is waited for a bounded time; then the writer gives up and the FIFO
//! is removed, so the secret is left nowhere.
//!
//! The writer never sits in a blocking `open`: a FIFO opened for writing
//! blocks until a reader appears, and one that is unlinked meanwhile can never
//! get one — the thread would hang for good (it did, in the tests). It polls a
//! non-blocking open against a real-time deadline and a cancel flag instead,
//! so its thread always ends.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::error::OrcaError;
use crate::launch::shell_quote;

/// The FIFO directory under totsuka's runtime dir.
const DIR_NAME: &str = "orca-env";

/// How often the writer looks for its reader (and for room in the pipe).
const POLL: Duration = Duration::from_millis(20);

/// Where the FIFOs live: `$XDG_RUNTIME_DIR/totsuka/orca-env`, or under the
/// state dir when `XDG_RUNTIME_DIR` is unset — the Orchestrator's own
/// `runtime_dir` rule (`paths.rs`), next to the hook socket. Relative XDG
/// values are ignored, as the XDG spec says. `None` when neither an absolute
/// XDG base nor `HOME` is available.
pub fn default_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let absolute = |key: &str| {
        env(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = absolute("XDG_RUNTIME_DIR")
        .or_else(|| absolute("XDG_STATE_HOME"))
        .or_else(|| absolute("HOME").map(|home| home.join(".local").join("state")))?;
    Some(base.join("totsuka").join(DIR_NAME))
}

/// This process's FIFO directory: `<base>/<pid>`, both `0700`.
///
/// **Per process, not shared.** `totsuka doctor` and `config validate` launch
/// and initialize an orca plugin of their own while `totsuka run`'s may be
/// mid-dispatch, so a directory every plugin process sweeps would let a probe
/// unlink the run's live FIFO (Copilot review, #745). Each process removes
/// only its own directory, on open (a leftover under a reused pid) and on
/// drop. A crashed process leaves its directory behind, but a FIFO holds no
/// data on disk, so what is left carries no secret.
#[derive(Debug)]
pub struct EnvHandoff {
    dir: PathBuf,
    /// How long each FIFO waits for its reader.
    wait: Duration,
}

impl EnvHandoff {
    /// Create `<base>/<pid>` (`0700`), emptied of anything a dead process
    /// with the same pid left there. Each FIFO then waits up to `wait` for
    /// the terminal's shell to read it.
    pub fn open(base: PathBuf, wait: Duration) -> std::io::Result<Self> {
        std::fs::create_dir_all(&base)?;
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
        let dir = base.join(std::process::id().to_string());
        // Our pid, so whoever made it is gone.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { dir, wait })
    }

    /// Make a FIFO for `env` and start writing it on a thread: the lines go
    /// through once the shell opens the FIFO. Called *before* `terminal
    /// create`, since the shell's read blocks until a writer exists.
    pub async fn start(&self, env: &BTreeMap<String, String>) -> Result<Delivery, OrcaError> {
        let mut content = String::new();
        for (key, value) in env {
            // The key lands unquoted in `export KEY=…`: anything but a shell
            // identifier would be code, not a name.
            if !is_env_name(key) {
                return Err(OrcaError::EnvHandoff(format!(
                    "`{key}` is not a valid environment variable name"
                )));
            }
            content.push_str(&format!("export {key}={}\n", shell_quote(value)));
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = self
            .dir
            .join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
        // `mkfifo(1)` rather than `mkfifo(3)`: POSIX, no `unsafe`, no new
        // dependency, and `-m` sets the mode regardless of the umask.
        let status = tokio::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&path)
            .status()
            .await
            .map_err(|e| OrcaError::EnvHandoff(format!("cannot run mkfifo: {e}")))?;
        if !status.success() {
            return Err(OrcaError::EnvHandoff(format!(
                "mkfifo {} failed ({status})",
                path.display()
            )));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (target, stop, wait) = (path.clone(), cancel.clone(), self.wait);
        let task = tokio::task::spawn_blocking(move || write_fifo(&target, &content, wait, &stop));
        Ok(Delivery { path, task, cancel })
    }
}

impl Drop for EnvHandoff {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether `name` is a POSIX shell identifier (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Write `content` to the FIFO at `path` once a reader opens it, giving up
/// (`TimedOut`) after `wait` or as soon as `cancel` is set.
fn write_fifo(
    path: &Path,
    content: &str,
    wait: Duration,
    cancel: &AtomicBool,
) -> std::io::Result<()> {
    let deadline = Instant::now() + wait;
    let give_up = || cancel.load(Ordering::Relaxed) || Instant::now() >= deadline;
    // Non-blocking, a writer's open fails with ENXIO until a reader exists.
    let mut file = loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                if give_up() {
                    return Err(ErrorKind::TimedOut.into());
                }
                std::thread::sleep(POLL);
            }
            Err(e) => return Err(e),
        }
    };
    // The prompt context can outgrow the pipe buffer: wait for the reader to
    // drain it, under the same deadline.
    let mut rest = content.as_bytes();
    while !rest.is_empty() {
        match file.write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if give_up() {
                    return Err(ErrorKind::TimedOut.into());
                }
                std::thread::sleep(POLL);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A write in progress. Dropping it — delivered or not — stops the writer and
/// removes the FIFO.
#[derive(Debug)]
pub struct Delivery {
    path: PathBuf,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    cancel: Arc<AtomicBool>,
}

impl Delivery {
    /// The FIFO's path, for the launch command.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait for the shell to take the env (bounded by the handoff's wait).
    pub async fn finish(mut self) -> Result<(), OrcaError> {
        match (&mut self.task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) if e.kind() == ErrorKind::TimedOut => Err(OrcaError::EnvHandoff(
                "the terminal did not read its launch env in time (the shell never ran the \
                 command — a failed terminal, an rc file that stopped, or a closed tab)"
                    .into(),
            )),
            Ok(Err(e)) => Err(OrcaError::EnvHandoff(format!(
                "writing the launch env failed: {e}"
            ))),
            Err(e) => Err(OrcaError::EnvHandoff(format!(
                "the launch env writer died: {e}"
            ))),
        }
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::launch_command;

    const WAIT: Duration = Duration::from_secs(10);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "totsuka-orca-handoff-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_dir_follows_the_orchestrators_runtime_rule() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            default_dir(env(&[("XDG_RUNTIME_DIR", "/run/u"), ("HOME", "/h")])),
            Some(PathBuf::from("/run/u/totsuka/orca-env"))
        );
        assert_eq!(
            default_dir(env(&[("XDG_RUNTIME_DIR", "rel"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/state/totsuka/orca-env"))
        );
        assert_eq!(
            default_dir(env(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")])),
            Some(PathBuf::from("/s/totsuka/orca-env"))
        );
        assert_eq!(default_dir(env(&[])), None);
    }

    #[test]
    fn env_names_must_be_shell_identifiers() {
        assert!(is_env_name("TOTSUKA_JOB_ID"));
        assert!(is_env_name("_x1"));
        assert!(!is_env_name(""));
        assert!(!is_env_name("1A"));
        assert!(!is_env_name("A;rm -rf ~"));
        assert!(!is_env_name("A-B"));
    }

    /// Another plugin process — `doctor`'s probe next to a live run — has a
    /// directory of its own, and nothing this process does touches it.
    #[tokio::test]
    async fn each_process_owns_a_private_dir_and_removes_only_that() {
        let base = scratch("owned");
        let sibling = base.join("1"); // another process's directory
        std::fs::create_dir_all(&sibling).unwrap();
        let theirs = sibling.join("0");
        let made = std::process::Command::new("mkfifo")
            .arg(&theirs)
            .status()
            .unwrap();
        assert!(made.success());
        // A dead process that had our pid left something behind.
        let own = base.join(std::process::id().to_string());
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("stale"), "").unwrap();

        let handoff = EnvHandoff::open(base.clone(), WAIT).unwrap();
        assert!(!own.join("stale").exists(), "our pid's leftover is cleared");
        for dir in [&base, &own] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }
        drop(handoff);
        assert!(!own.exists(), "our directory goes on drop");
        assert!(theirs.exists(), "another process's FIFO is never touched");
    }

    #[tokio::test]
    async fn a_bad_name_is_refused_before_any_fifo_exists() {
        let dir = scratch("badname");
        let handoff = EnvHandoff::open(dir.clone(), WAIT).unwrap();
        let dir = dir.join(std::process::id().to_string());
        let err = handoff
            .start(&BTreeMap::from([("A B".to_string(), "1".to_string())]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("`A B`"), "{err}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    /// The whole path, minus orca: the command orca would type, run by a
    /// real `/bin/sh`, reading a real FIFO.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_launch_command_receives_the_env_through_the_fifo_and_removes_it() {
        let dir = scratch("roundtrip");
        let handoff = EnvHandoff::open(dir.clone(), WAIT).unwrap();
        let env = BTreeMap::from([
            ("PLAIN".to_string(), "x y".to_string()),
            ("QUOTED".to_string(), "it's \"q\" $HOME `id`".to_string()),
            ("MULTI".to_string(), "line1\nline2".to_string()),
        ]);
        let delivery = handoff.start(&env).await.unwrap();
        let path = delivery.path().to_path_buf();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let command = launch_command(
            "sh",
            &[
                "-c".to_string(),
                r#"printf '%s|%s|%s' "$PLAIN" "$QUOTED" "$MULTI""#.to_string(),
            ],
            Some(&path),
        );
        assert!(!command.contains("x y"), "no value is typed: {command}");

        let shell = tokio::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output();
        let (out, delivered) = tokio::join!(shell, delivery.finish());
        delivered.unwrap();
        let out = out.unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "x y|it's \"q\" $HOME `id`|line1\nline2"
        );
        assert!(!path.exists(), "the FIFO is gone once the agent runs");
    }

    #[tokio::test]
    async fn no_reader_times_out_releases_the_writer_and_removes_the_fifo() {
        let dir = scratch("timeout");
        let handoff = EnvHandoff::open(dir.clone(), Duration::from_millis(200)).unwrap();
        let delivery = handoff
            .start(&BTreeMap::from([("SECRET".to_string(), "s".to_string())]))
            .await
            .unwrap();
        let path = delivery.path().to_path_buf();
        let err = delivery.finish().await.unwrap_err();
        assert!(err.to_string().contains("did not read"), "{err}");
        assert!(!path.exists(), "the FIFO is removed");
    }

    /// A shell that runs after the writer gave up must not start the agent
    /// with no env.
    #[tokio::test]
    async fn a_missing_fifo_stops_the_launch() {
        let command = launch_command(
            "echo",
            &["launched".to_string()],
            Some(Path::new("/nonexistent/fifo")),
        );
        let out = tokio::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .await
            .unwrap();
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("launched"));
    }
}
