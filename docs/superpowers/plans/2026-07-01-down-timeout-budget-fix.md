# Down Command Timeout Budget Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `totsukactl down`'s wait-for-supervisor-exit deadline scale with the configured shutdown grace/kill durations instead of a hardcoded 30 seconds, so a real (multi-stage, escalating) shutdown sequence is never mistaken for a hang.

**Architecture:** Add a pure function `shutdown_wait_budget(grace_secs, kill_secs) -> Duration` that computes the worst-case time for the 3-stage reverse-order shutdown (ingestion → orchestrator → agent-adapter), where each stage may need a full `grace` wait plus a `kill` (second SIGTERM) escalation wait, plus a fixed safety margin. Thread the computed `Duration` into `down::run` as an explicit parameter (dependency injection, matching the existing `Arc<dyn Clock>` pattern in this codebase) instead of the current hardcoded `Duration::from_secs(30)`. `cli.rs` computes the budget from `cfg.supervisor.shutdown_grace_secs` / `shutdown_kill_secs` (already loaded before dispatch) and passes it through.

**Tech Stack:** Rust (stable), tokio, existing `totsukactl` crate conventions (`Paths`, `TotsukactlError`, `pidfile`).

## Global Constraints

- Workspace-wide `#![forbid(unsafe_code)]` — do not introduce unsafe code.
- `[profile.release] panic = "abort"` — do not rely on unwinding for control flow.
- No new `SystemTime::now()` / `chrono::Utc::now()` call sites (clippy-denied) — this plan adds none.
- Follow the existing dependency-injection pattern already used for `Arc<dyn Clock>`: pass computed values in as parameters rather than reading global config deep inside a function.
- Every implementer's report contract MUST include running `cargo fmt --all -- --check` before reporting DONE — a prior task in this branch shipped an unformatted commit that broke CI.

---

### Task 1: Pure `shutdown_wait_budget` function with unit tests

**Files:**
- Modify: `crates/totsukactl/src/commands/down.rs` (add function + `#[cfg(test)]` module near the top of the file, above `pub async fn run`)

**Interfaces:**
- Produces: `pub fn shutdown_wait_budget(grace_secs: u64, kill_secs: u64) -> std::time::Duration` — later tasks (Task 2, and `cli.rs`) call this to compute the deadline budget.
- Produces: `pub const SHUTDOWN_WAIT_MARGIN_SECS: u64 = 10;` — the fixed safety margin added on top of the 3-stage worst case.

The formula: the reverse-order shutdown in `crates/totsukactl/src/supervisor/shutdown.rs` runs 3 sequential stages (ingestion: `github-watcher`+`qa-service` in parallel, then `orchestrator`, then `agent-adapter`). Each stage's `wait_or_kill_escalate` sleeps the full `grace` duration unconditionally, and if the child is still alive after that, sends a second SIGTERM and sleeps the full `second_term` (kill) duration before giving up. So the worst case per stage is `grace_secs + kill_secs`, and the worst case for all 3 stages is `3 * (grace_secs + kill_secs)`. Add `SHUTDOWN_WAIT_MARGIN_SECS` on top for socket/scheduling overhead observed during the smoke test (control-channel round trip, process scheduling jitter).

- [ ] **Step 1: Write the failing unit tests**

Add this near the top of `crates/totsukactl/src/commands/down.rs`, after the existing `use` statements and before `pub async fn run` (the function under test, `shutdown_wait_budget`, does not exist yet — that's intentional, added in Step 3):

```rust
#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn budget_matches_default_config_values() {
        // Defaults from totsuka-config schema.rs: grace=15, kill=5.
        assert_eq!(shutdown_wait_budget(15, 5), Duration::from_secs(70));
    }

    #[test]
    fn budget_is_margin_only_when_grace_and_kill_are_zero() {
        assert_eq!(shutdown_wait_budget(0, 0), Duration::from_secs(10));
    }

    #[test]
    fn budget_scales_linearly_with_configured_values() {
        assert_eq!(shutdown_wait_budget(30, 10), Duration::from_secs(130));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package totsukactl budget_tests`
Expected: FAIL to compile — `error[E0425]: cannot find function `shutdown_wait_budget` in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Add this immediately above the `#[cfg(test)] mod budget_tests` block added in Step 1:

```rust
pub const SHUTDOWN_WAIT_MARGIN_SECS: u64 = 10;

/// Worst-case time for the 3-stage reverse-order shutdown (ingestion →
/// orchestrator → agent-adapter) to complete: each stage may need a full
/// `grace_secs` wait plus a `kill_secs` second-SIGTERM escalation wait,
/// plus a fixed safety margin for control-channel and scheduling overhead.
pub fn shutdown_wait_budget(grace_secs: u64, kill_secs: u64) -> Duration {
    Duration::from_secs(3 * (grace_secs + kill_secs) + SHUTDOWN_WAIT_MARGIN_SECS)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package totsukactl budget_tests`
Expected: `test commands::down::budget_tests::budget_matches_default_config_values ... ok`, `test commands::down::budget_tests::budget_is_margin_only_when_grace_and_kill_are_zero ... ok`, `test commands::down::budget_tests::budget_scales_linearly_with_configured_values ... ok` — 3 passed.

- [ ] **Step 5: Format and commit**

Run: `cargo fmt --all -- --check` (fix with `cargo fmt --all` if it reports diffs), then:

```bash
git add crates/totsukactl/src/commands/down.rs
git commit -m "feat(totsukactl): add shutdown_wait_budget pure function"
```

---

### Task 2: Wire the computed budget into `down::run` and its call site

**Files:**
- Modify: `crates/totsukactl/src/commands/down.rs:9` (change `run`'s signature and deadline logic)
- Modify: `crates/totsukactl/src/cli.rs:88` (compute budget from config, pass to `down::run`)
- Modify: `crates/totsukactl/tests/down_flow.rs` (update call site for new signature)
- Modify: `crates/totsukactl/tests/down_sock_fallback.rs` (update call site for new signature)
- Test: `crates/totsukactl/tests/down_budget.rs` (NEW — integration tests proving the budget is honored, not hardcoded)

**Interfaces:**
- Consumes: `shutdown_wait_budget(grace_secs: u64, kill_secs: u64) -> Duration` from Task 1.
- Produces: `pub async fn run(paths: &Paths, force: bool, postgres: bool, wait_budget: Duration) -> Result<(), TotsukactlError>` — the new signature of `down::run`, replacing the current 3-argument version. Any future caller of `down::run` must supply the budget explicitly.

- [ ] **Step 1: Change `down::run`'s signature and deadline computation**

In `crates/totsukactl/src/commands/down.rs`, change the function signature and the two places that reference the old hardcoded `30`:

```rust
pub async fn run(
    paths: &Paths,
    force: bool,
    postgres: bool,
    wait_budget: Duration,
) -> Result<(), TotsukactlError> {
```

And replace the deadline/timeout block (currently `let deadline = Instant::now() + Duration::from_secs(30);` through the closing `Err(TotsukactlError::Timeout(...))`) with:

```rust
    let deadline = Instant::now() + wait_budget;
    while Instant::now() < deadline {
        if !pidfile::process_alive(pid) {
            pidfile::remove(&paths.supervisor_pid())?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if force {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        pidfile::remove(&paths.supervisor_pid())?;
        Ok(())
    } else {
        Err(TotsukactlError::Timeout(format!(
            "supervisor pid {pid} did not exit in {}s; rerun with --force",
            wait_budget.as_secs()
        )))
    }
```

Leave everything else in the function (the `pidfile::check`, the `maybe_pid.is_none()` early-return branch, the `client.shutdown(...)` call, the SIGTERM fallback) unchanged — this task only touches the parameter list and the deadline block.

- [ ] **Step 2: Update the call site in `cli.rs`**

In `crates/totsukactl/src/cli.rs`, change line 88 from:

```rust
        Cmd::Down { force, postgres } => crate::commands::down::run(&paths, force, postgres).await,
```

to:

```rust
        Cmd::Down { force, postgres } => {
            let wait_budget = crate::commands::down::shutdown_wait_budget(
                cfg.supervisor.shutdown_grace_secs,
                cfg.supervisor.shutdown_kill_secs,
            );
            crate::commands::down::run(&paths, force, postgres, wait_budget).await
        }
```

- [ ] **Step 3: Update existing test call sites**

In `crates/totsukactl/tests/down_flow.rs`, change:

```rust
    let err = down::run(&paths, false, false).await.unwrap_err();
```

to:

```rust
    let err = down::run(&paths, false, false, std::time::Duration::from_secs(1))
        .await
        .unwrap_err();
```

(The `NotRunning` branch returns before the deadline is ever used, so any budget value works here; `1` second keeps the test's intent obvious.)

In `crates/totsukactl/tests/down_sock_fallback.rs`, change:

```rust
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        down::run(&paths, /*force=*/ true, /*postgres=*/ false),
    )
    .await
    .expect("down::run should not hang");
```

to:

```rust
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        down::run(
            &paths,
            /*force=*/ true,
            /*postgres=*/ false,
            std::time::Duration::from_secs(1),
        ),
    )
    .await
    .expect("down::run should not hang");
```

(This test's assertions target the pidfile-absent early-return branch, which also returns before the deadline is used.)

- [ ] **Step 4: Run the updated existing tests to verify they still pass**

Run: `cargo test --package totsukactl --test down_flow --test down_sock_fallback`
Expected: both test binaries report all tests passing (`down_returns_not_running_without_pidfile ... ok`, `down_falls_back_to_sock_when_pid_absent ... ok`).

- [ ] **Step 5: Write the new integration tests proving the budget is honored**

Create `crates/totsukactl/tests/down_budget.rs`:

```rust
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
    let err = down::run(&paths, /*force=*/ false, /*postgres=*/ false, budget)
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
    let child = Command::new("sleep")
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

    // A large budget (matching what real config values now produce) must
    // not make the happy path slower — down::run returns as soon as the
    // pid disappears, not after the full budget.
    let budget = Duration::from_secs(90);
    let start = Instant::now();
    down::run(&paths, /*force=*/ false, /*postgres=*/ false, budget)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "happy path should return quickly regardless of a large budget, elapsed={elapsed:?}"
    );
    assert!(!paths.supervisor_pid().exists());
}
```

- [ ] **Step 6: Run the new tests to verify they pass**

Run: `cargo test --package totsukactl --test down_budget`
Expected: `test down_times_out_at_the_supplied_budget_not_a_hardcoded_value ... ok`, `test down_returns_quickly_when_pid_exits_even_with_a_large_budget ... ok` — 2 passed. (This test takes slightly over 1 second of wall-clock time due to the first test's real timeout wait — that is expected.) If either fails, re-check that the deadline block from Step 1 was pasted exactly as written.

- [ ] **Step 7: Run the full totsukactl test suite to catch any other call site**

Run: `cargo test --package totsukactl`
Expected: all tests pass, 0 failures.

- [ ] **Step 8: Format and commit**

Run: `cargo fmt --all -- --check` (fix with `cargo fmt --all` if it reports diffs), then:

```bash
git add crates/totsukactl/src/commands/down.rs \
        crates/totsukactl/src/cli.rs \
        crates/totsukactl/tests/down_flow.rs \
        crates/totsukactl/tests/down_sock_fallback.rs \
        crates/totsukactl/tests/down_budget.rs
git commit -m "fix(totsukactl): down waits for the configured shutdown budget, not a hardcoded 30s"
```

---

## Manual Verification (after both tasks land)

This was surfaced by a real smoke test where `down` errored with "did not exit in 30s" while the supervisor was still correctly mid-shutdown. To confirm the fix on real hardware:

1. Boot the stack: `./target/release/totsukactl up` (backgrounded).
2. Confirm 5/5 healthy: `./target/release/totsukactl status`.
3. Run `./target/release/totsukactl down` and time it: it should now wait up to `shutdown_wait_budget(shutdown_grace_secs, shutdown_kill_secs)` (70s with the default config values of grace=15/kill=5) instead of erroring at 30s, and should return `Ok` as soon as the supervisor process actually exits.
