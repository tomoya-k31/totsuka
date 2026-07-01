//! Prove that `totsukactl down`'s wait-for-supervisor-exit deadline is the
//! caller-supplied `wait_budget`, not a hardcoded value: (1) a process that
//! outlives a short budget produces a Timeout error reporting that exact
//! budget, and (2) a process that exits quickly returns Ok well before a
//! large budget elapses (a big budget must not slow down the happy path).

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use totsukactl::commands::down;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

fn paths(tmp: &TempDir) -> Paths {
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn down_times_out_at_the_supplied_budget_not_a_hardcoded_value() {
    let tmp = TempDir::new().unwrap();
    let paths = paths(&tmp);

    // Spawn a child that ignores SIGTERM so it survives the fallback
    // SIGTERM `down::run` sends (there is no supervisor.sock listening,
    // so `down::run` falls back to signalling the pid directly).
    let mut child = Command::new("sh")
        .args(["-c", "trap '' TERM; sleep 5"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sh");
    let pid = child.id() as i32;
    let mut f = std::fs::File::create(paths.supervisor_pid()).unwrap();
    writeln!(f, "{pid}").unwrap();
    drop(f);

    let budget = Duration::from_secs(1);
    let start = Instant::now();
    let err = down::run(
        &paths, /*force=*/ false, /*postgres=*/ false, budget,
    )
    .await
    .unwrap_err();
    let elapsed = start.elapsed();

    match err {
        TotsukactlError::Timeout(msg) => {
            assert!(
                msg.contains("did not exit in 1s"),
                "expected message to report the 1s budget, got: {msg}"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
    assert!(
        elapsed >= budget,
        "should not return before the budget elapses, elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "should not wait much longer than the budget, elapsed={elapsed:?}"
    );

    // Cleanup: the child ignores SIGTERM, so force-kill it.
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test(flavor = "multi_thread")]
async fn down_returns_quickly_when_pid_exits_even_with_a_large_budget() {
    let tmp = TempDir::new().unwrap();
    let paths = paths(&tmp);

    // A plain `sleep` has no SIGTERM trap, so the fallback SIGTERM
    // `down::run` sends will terminate it almost immediately.
    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let pid = child.id() as i32;
    let mut f = std::fs::File::create(paths.supervisor_pid()).unwrap();
    writeln!(f, "{pid}").unwrap();
    drop(f);

    // Reap the child in the background as soon as it exits. In production,
    // down::run's target process is never a child of totsukactl (it's the
    // supervisor, reaped by its own real parent), so process_alive()'s
    // `kill(pid, 0)` reliably reports death. Here, the test itself is the
    // OS parent of the spawned `sleep`, so without an explicit wait() the
    // process would linger as a zombie after SIGTERM — and kill(pid, 0)
    // reports zombies as still alive until reaped, which would make
    // process_alive() (and thus this test) wait for the full budget.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    // A large budget (matching what real config values now produce) must
    // not make the happy path slower — down::run returns as soon as the
    // pid disappears, not after the full budget.
    let budget = Duration::from_secs(90);
    let start = Instant::now();
    down::run(
        &paths, /*force=*/ false, /*postgres=*/ false, budget,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "happy path should return quickly regardless of a large budget, elapsed={elapsed:?}"
    );
    assert!(!paths.supervisor_pid().exists());
}
