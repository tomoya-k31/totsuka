//! Shared helpers for totsuka's integration tests (#66).
//!
//! Consolidates the real-git-repo and scratch-dir setup that the worktree,
//! run-loop, and CLI E2E tests all need, so the git-signing workaround and the
//! bare-origin bootstrap live in one place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

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

/// Locate a **sibling** workspace binary — one that is not a bin target of the
/// crate under test, so `CARGO_BIN_EXE_*` does not exist for it — under the
/// same target profile directory as `anchor` (pass the calling crate's own
/// `CARGO_BIN_EXE_<bin>`).
///
/// Freshness is guaranteed by `cargo build -p <package> --bin <bin>`, run at
/// most **once per test process**. The previous per-call build made 15 nested
/// cargo invocations per `cargo test` run, every one of them contending on the
/// target-directory lock — and under a process-per-test runner it would be one
/// per *test* (#281).
///
/// Set `TEST_SUPPORT_PREBUILT_BINS=1` to skip the build and trust what is
/// already in the target dir. The precondition is "every workspace bin has just
/// been built"; CI satisfies it two different ways (#341):
///
/// - the PR `test` job runs `cargo build --workspace --all-targets` as the step
///   immediately before `cargo test`;
/// - the `coverage` job on main has no such step, but `cargo llvm-cov` finishes
///   its own build phase — which includes every selected package's bin targets —
///   before it runs any test binary.
///
/// Either way a violated precondition surfaces as the `path.exists()` assertion
/// below, never as a silently stale binary.
///
/// The name deliberately avoids the `TOTSUKA_` prefix: `apply_env_overrides`
/// warns to **stderr** for any unrecognised `TOTSUKA_*` variable (ADR-0009),
/// and the E2Es spawn `totsuka` as a child that inherits this env — so a
/// `TOTSUKA_`-prefixed name prepends a warning line to every child's stderr and
/// breaks the tests that parse stderr as a JSON error envelope.
pub fn sibling_bin(anchor: &Path, package: &str, bin: &str) -> PathBuf {
    static BUILT: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut cache = BUILT
        .get_or_init(Mutex::default)
        .lock()
        .expect("sibling_bin cache poisoned");
    if let Some(path) = cache.get(bin) {
        return path.clone();
    }

    // `anchor` lives in the profile dir the tests use; its parent's name is the
    // profile (`debug` / `release`).
    let bin_dir = anchor.parent().expect("target profile dir").to_path_buf();
    let path = bin_dir.join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));

    if std::env::var_os("TEST_SUPPORT_PREBUILT_BINS").is_none() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = Command::new(cargo);
        build.args(["build", "-p", package, "--bin", bin]);
        if bin_dir.file_name().and_then(|n| n.to_str()) == Some("release") {
            build.arg("--release");
        }
        let status = build
            .status()
            .unwrap_or_else(|e| panic!("spawn cargo build for {bin}: {e}"));
        assert!(status.success(), "failed to build {bin}");
    }
    assert!(
        path.exists(),
        "{bin} not found at {} — run `cargo build -p {package} --bin {bin}`, \
         or unset TEST_SUPPORT_PREBUILT_BINS to let the test build it",
        path.display()
    );
    cache.insert(bin.to_string(), path.clone());
    path
}
