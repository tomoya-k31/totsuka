//! Shared helpers for totsuka's integration tests (#66).
//!
//! Consolidates the real-git-repo and scratch-dir setup that the worktree,
//! run-loop, and CLI E2E tests all need, so the git-signing workaround and the
//! bare-origin bootstrap live in one place.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a `git` command in `cwd`, asserting success, returning trimmed stdout.
///
/// Commit/tag signing is disabled so a local signing agent (e.g. 1Password)
/// never blocks a background test run.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Inject a synthetic agent-CLI hook signal into a **running** UDS receiver by
/// POSTing `body` to `POST /agent-events` at `socket_path` (minimal HTTP/1.1,
/// `Connection: close`), exactly as `on-stop.sh`'s `curl --unix-socket` does.
/// Returns the HTTP status code the receiver answered.
///
/// This is the E2E injection helper (#141): unlike calling `Engine::on_signal`
/// directly, it exercises the real socket transport, the Bearer check, and the
/// `SignalPort` → run-loop wiring.
#[cfg(unix)]
pub fn post_hook_signal(
    socket_path: &Path,
    token: Option<&str>,
    body: &str,
) -> std::io::Result<u16> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /agent-events HTTP/1.1\r\n\
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
    let text = String::from_utf8_lossy(&response);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no HTTP status line in reply",
            )
        })?;
    Ok(status)
}

/// A fresh, empty scratch directory unique to this process and `name`.
///
/// Removed first if it already exists, so a re-run starts clean.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("totsuka-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Create a bare `origin.git` with one commit on `main` and a working `clone`
/// of it under `base`. Returns the clone path. The clone has a committer
/// identity configured so tests may commit directly.
pub fn bare_origin_and_clone(base: &Path) -> PathBuf {
    let origin = base.join("origin.git");
    git(
        base,
        &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
    );

    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    git(&seed, &["config", "user.email", "t@example.com"]);
    git(&seed, &["config", "user.name", "T"]);
    git(&seed, &["commit", "--allow-empty", "-m", "init"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "origin", "main"]);

    let clone = base.join("clone");
    git(
        base,
        &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
    );
    git(&clone, &["config", "user.email", "t@example.com"]);
    git(&clone, &["config", "user.name", "T"]);
    clone
}

/// Read a recorded NDJSON log file as one JSON value per line (empty when the
/// file was never written).
///
/// **Tolerant of a truncated final line, strict about everything else.** The
/// writer is usually a live mock-plugin process appending with a plain
/// `writeln!` — not atomic — and `run_until`-style polling closures read the
/// file *while* it is being written. A half-written last line means "not
/// flushed yet"; the next poll sees it whole. A malformed line in the middle
/// of the file has no such excuse: that is real corruption and must keep
/// failing the test (#229).
pub fn read_ndjson_log(path: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| match serde_json::from_str(line) {
            Ok(value) => Some(value),
            Err(_) if i + 1 == lines.len() => None,
            Err(e) => panic!("malformed log line {} ({e}): {line}", i + 1),
        })
        .collect()
}
