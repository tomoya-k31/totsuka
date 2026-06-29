# Shutdown Bug Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the supervisor from silently calling `shutdown_stack` when its sock_api task dies, surface why sock_api died, and make `totsukactl down` work even when the supervisor pid file is absent.

**Architecture:** Three localized fixes to existing files. `main_loop.rs`'s control-dispatch loop currently treats `ctl_rx.recv() = None` (channel closed because all senders dropped) as "graceful shutdown requested" — it isn't. The fix replaces the closed receiver with one that blocks forever, letting the loop continue serving signals. `sock_api.rs::serve_uds` errors are silently swallowed by `let _ = ... .await;` — the fix logs them. `commands/down.rs` returns `NotRunning` the instant the pidfile is missing — the fix falls back to the supervisor sock.

**Tech Stack:** Rust stable / tokio mpsc / nix signals. No new dependencies.

## Global Constraints

- Rust workspace stable channel, `[profile.release] panic = "abort"`; lib crates expose error enums via `thiserror`, bins return `anyhow::Result<()>`.
- `#![forbid(unsafe_code)]` on every lib.rs.
- `tokio::task::block_in_place` is clippy-denied workspace-wide.
- `SystemTime::now()` / `chrono::Utc::now()` direct calls clippy-denied — `Arc<dyn Clock>` for time.
- `Secret<String>` for tokens/passwords; `.expose()` only at outbound boundaries.
- All Claude-driven commits use `git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "..."` (1Password Touch blocks background-signed commits).
- The bug observed in the 2026-06-29 smoke: `sock_api` task exited shortly after boot → `ctl_tx` clones dropped → `ctl_rx.recv()` returned `None` → control loop hit `None => break (false, false)` → `shutdown_stack` removed `supervisor.pid` while children kept running → `totsukactl down` returned `NotRunning` even though the supervisor process was alive.

---

### Task 1: Stop silent shutdown when `ctl_rx` channel closes

**Files:**
- Modify: `crates/totsukactl/src/supervisor/main_loop.rs:175-190` (the `None => break (false, false)` arm).
- Create: `crates/totsukactl/src/supervisor/ctl_replace.rs` (the new helper, isolated so it's unit-testable).
- Modify: `crates/totsukactl/src/supervisor.rs` (`pub mod ctl_replace;`).
- Create: `crates/totsukactl/tests/ctl_replace.rs`.

**Interfaces:**
- Consumes: `tokio::sync::mpsc::Receiver<ControlMsg>` (the old closed receiver) from `main_loop`.
- Produces: `pub async fn replace_closed_ctl_rx(_old: mpsc::Receiver<ControlMsg>) -> mpsc::Receiver<ControlMsg>` — takes ownership of the closed receiver (drops it), logs an error explaining what happened, returns a fresh receiver whose sender is held by a forever-pending tokio task so the new receiver never yields `None`.

- [ ] **Step 1: Write the failing unit test**

`crates/totsukactl/tests/ctl_replace.rs`:
```rust
use std::time::Duration;
use tokio::sync::mpsc;
use totsukactl::sock_api::ControlMsg;
use totsukactl::supervisor::ctl_replace::replace_closed_ctl_rx;

#[tokio::test]
async fn replaced_rx_blocks_when_old_was_closed() {
    let (tx, rx) = mpsc::channel::<ControlMsg>(1);
    drop(tx); // old channel: receiver would yield None
    let mut new_rx = replace_closed_ctl_rx(rx).await;

    // Old behavior under test: new_rx must NOT yield None within 100 ms.
    let outcome = tokio::time::timeout(Duration::from_millis(100), new_rx.recv()).await;
    assert!(
        outcome.is_err(),
        "new receiver should block; got {outcome:?} instead of timeout"
    );
}

#[tokio::test]
async fn replaced_rx_does_not_panic_on_drop() {
    let (tx, rx) = mpsc::channel::<ControlMsg>(1);
    drop(tx);
    let new_rx = replace_closed_ctl_rx(rx).await;
    drop(new_rx); // exercise the held-sender's task termination path
    // Sleep briefly to let the held-sender task notice the drop (it never will, but verify no panic).
    tokio::time::sleep(Duration::from_millis(50)).await;
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p totsukactl --test ctl_replace
```
Expected: FAIL with `error[E0432]: unresolved import 'totsukactl::supervisor::ctl_replace'`.

- [ ] **Step 3: Implement the helper**

`crates/totsukactl/src/supervisor/ctl_replace.rs`:
```rust
//! Helper for `main_loop`'s control-dispatch loop: when `ctl_rx.recv()` returns
//! `None` (because the sock_api task crashed and dropped all `ControlMsg` senders),
//! the supervisor must NOT interpret that as "shutdown requested". Instead, we
//! log loudly and swap the closed receiver for a fresh one whose sender we
//! intentionally hold forever — the new receiver will never yield `None`, so
//! `tokio::select!` keeps polling the signal arms (SIGTERM/SIGINT).
//!
//! Once the new receiver is in place, the supervisor's CLI IPC is dead until
//! restart, but the children remain healthy under signal-driven shutdown.

use crate::sock_api::ControlMsg;
use tokio::sync::mpsc;

pub async fn replace_closed_ctl_rx(_old: mpsc::Receiver<ControlMsg>) -> mpsc::Receiver<ControlMsg> {
    // Dropping `_old` is intentional: the sender side is already gone, so the
    // receiver is no longer useful.
    tracing::error!(
        "sock_api control channel closed unexpectedly; supervisor continuing on signals only \
         (CLI commands via supervisor.sock will not work until restart)"
    );
    let (sender, new_rx) = mpsc::channel::<ControlMsg>(1);
    tokio::spawn(async move {
        // Holding `sender` for the lifetime of the supervisor keeps `new_rx`
        // open (never yields None). `pending::<()>()` is the standard "never
        // resolves" future.
        let _hold = sender;
        std::future::pending::<()>().await;
    });
    new_rx
}
```

- [ ] **Step 4: Wire the module and run the test**

Modify `crates/totsukactl/src/supervisor.rs` to add the module — insert in the alphabetical mod list (after `control`, before `main_loop`):

```rust
pub mod boot;
pub mod control;
pub mod ctl_replace;
pub mod main_loop;
pub mod restart_tick;
pub mod shutdown;

pub use boot::{await_ready, boot, BootCtx};
pub use ctl_replace::replace_closed_ctl_rx;
pub use main_loop::run_supervisor;
pub use shutdown::{shutdown_stack, ShutdownCfg};
```

Run:
```bash
cargo test -p totsukactl --test ctl_replace
```
Expected: 2/2 PASS.

- [ ] **Step 5: Wire the helper into `main_loop.rs`**

Modify the `None` arm of the control-dispatch loop in `crates/totsukactl/src/supervisor/main_loop.rs` (around line 188-191). Make `ctl_rx` mutable by changing its binding to `let mut ctl_rx = ctl_rx;` (or hoisting the binding before the loop), then replace:

```rust
                    None => break (false, false),
```

with:

```rust
                    None => {
                        ctl_rx = crate::supervisor::ctl_replace::replace_closed_ctl_rx(ctl_rx).await;
                    }
                },
```

The surrounding `let (also_postgres, force): (bool, bool) = loop { tokio::select! { ... } };` must remain a `loop` so the new `None` arm reaches the next iteration. The arm no longer breaks, so the type-checker will complain about non-divergent match arms — fix by removing the `match` arm's terminator inside the `Some(...)` branches and ensuring the match returns `()` for non-shutdown arms (it already does for the Restart/Reload arms). The Shutdown arm continues to `break (postgres, force)`.

- [ ] **Step 6: Verify the change compiles and existing tests still pass**

```bash
cargo check -p totsukactl --tests
cargo test -p totsukactl
cargo clippy -p totsukactl --all-targets --all-features --locked -- -D warnings
```
Expected: clean compile, all tests pass, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/totsukactl/src/supervisor.rs crates/totsukactl/src/supervisor/ctl_replace.rs crates/totsukactl/src/supervisor/main_loop.rs crates/totsukactl/tests/ctl_replace.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "fix(totsukactl): do not shutdown_stack when sock_api control channel closes"
```

---

### Task 2: Log `serve_uds` errors instead of swallowing them

**Files:**
- Modify: `crates/totsukactl/src/supervisor/main_loop.rs` (the `let h_sock = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });` block).

**Interfaces:** none — single-line behavior change.

- [ ] **Step 1: Locate and replace the swallowing wrapper**

In `crates/totsukactl/src/supervisor/main_loop.rs`, find:
```rust
let h_sock = tokio::spawn(async move {
    let _ = serve_uds(listener, r).await;
});
```

Replace with:
```rust
let h_sock = tokio::spawn(async move {
    if let Err(e) = serve_uds(listener, r).await {
        tracing::error!(error=%e, "supervisor.sock serve_uds exited; CLI IPC will be unavailable");
    }
});
```

- [ ] **Step 2: Verify compile + tests**

```bash
cargo check -p totsukactl --tests
cargo test -p totsukactl
cargo clippy -p totsukactl --all-targets --all-features --locked -- -D warnings
```
Expected: clean compile + 300+ tests passing + clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/totsukactl/src/supervisor/main_loop.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "fix(totsukactl): log serve_uds errors instead of silently swallowing them"
```

---

### Task 3: `totsukactl down` falls back to sock when pid file is absent

**Files:**
- Modify: `crates/totsukactl/src/commands/down.rs:9-21` (the pidfile-only gate at the top of `run`).
- Create: `crates/totsukactl/tests/down_sock_fallback.rs` (new integration test using an in-process UDS server).

**Interfaces:**
- Consumes: `SupervisorClient::list()` (Task 17 of the totsukactl plan).
- Produces: `down::run` returns `Ok(())` when the supervisor pidfile is absent BUT the supervisor sock at `paths.supervisor_sock()` responds successfully to `GET /v1/processes`. Returns `NotRunning` only when both checks fail.

- [ ] **Step 1: Write the failing integration test**

`crates/totsukactl/tests/down_sock_fallback.rs`:
```rust
//! Verify that `totsukactl down` falls back to talking to supervisor.sock when
//! the pidfile is absent. Spins an in-process UDS server that accepts a
//! `POST /v1/shutdown`, then calls `down::run`, then asserts the shutdown was
//! delivered.

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use totsukactl::commands::down;
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState};

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
async fn down_falls_back_to_sock_when_pid_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = paths(&tmp);

    // No supervisor.pid is written — simulate the "pidfile vanished while
    // supervisor still runs" race that the smoke test surfaced.
    assert!(!paths.supervisor_pid().exists(), "pidfile must be absent");

    // Stand up a minimal supervisor.sock that accepts shutdown messages.
    let registry = Arc::new(Registry::new());
    let (tx, mut rx) = mpsc::channel::<ControlMsg>(8);
    let state = SockApiState { registry, control_tx: tx };
    let listener = bind_uds(&paths.supervisor_sock()).unwrap();
    let r = router(state);
    let _h_sock = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    // Run down. It must NOT return NotRunning.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        down::run(&paths, /*force=*/ true, /*postgres=*/ false),
    )
    .await
    .expect("down::run should not hang");

    // We DO accept `Err(Timeout(...))` here because there's no actual supervisor
    // process to die — the goal is to confirm the shutdown was *delivered*.
    // What we MUST NOT see is NotRunning.
    match &result {
        Err(totsukactl::error::TotsukactlError::NotRunning) => {
            panic!("down returned NotRunning despite live sock — fallback failed");
        }
        Ok(_) | Err(_) => {} // any other outcome is acceptable for this test
    }

    // The supervisor.sock received the shutdown control msg.
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("shutdown msg should arrive within 2s")
        .expect("shutdown msg present");
    assert!(
        matches!(msg, ControlMsg::Shutdown { postgres: false, force: true }),
        "expected Shutdown(postgres=false, force=true); got {msg:?}"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p totsukactl --test down_sock_fallback
```
Expected: FAIL with `panic`d at `down returned NotRunning despite live sock — fallback failed`.

- [ ] **Step 3: Add sock fallback to `down::run`**

Modify `crates/totsukactl/src/commands/down.rs::run`. Replace the function body's opening block:

```rust
pub async fn run(paths: &Paths, force: bool, postgres: bool) -> Result<(), TotsukactlError> {
    let pid_state = pidfile::check(&paths.supervisor_pid())?;
    let pid = match pid_state {
        pidfile::PidState::Alive(p) => p,
        pidfile::PidState::Stale(_) | pidfile::PidState::Absent => {
            return Err(TotsukactlError::NotRunning);
        }
    };
    // ...rest unchanged
```

…with:

```rust
pub async fn run(paths: &Paths, force: bool, postgres: bool) -> Result<(), TotsukactlError> {
    let pid_state = pidfile::check(&paths.supervisor_pid())?;
    let maybe_pid: Option<i32> = match pid_state {
        pidfile::PidState::Alive(p) => Some(p),
        pidfile::PidState::Stale(_) | pidfile::PidState::Absent => None,
    };

    let client = SupervisorClient::new(paths.supervisor_sock());

    // If pid is absent or stale, the sock is our only remaining evidence of a
    // live supervisor. Try it before giving up.
    if maybe_pid.is_none() {
        match client.shutdown(postgres, force).await {
            Ok(()) => {
                // Shutdown command was delivered. The supervisor will exit on
                // its own; we have no pid to wait on, so we return Ok.
                tracing::info!(
                    "supervisor.pid was absent but supervisor.sock accepted shutdown; \
                     supervisor will exit on its own"
                );
                let _ = pidfile::remove(&paths.supervisor_pid()); // idempotent cleanup
                return Ok(());
            }
            Err(TotsukactlError::SupervisorUnreachable(_)) => {
                return Err(TotsukactlError::NotRunning);
            }
            Err(e) => return Err(e),
        }
    }

    let pid = maybe_pid.expect("guarded above");

    // ... existing sock-shutdown-then-poll-pid flow continues unchanged ...
```

The rest of the function body (the `match client.shutdown(...)` call, the polling loop, the SIGKILL fallback) stays as-is.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p totsukactl --test down_sock_fallback
cargo test -p totsukactl --test down_flow  # the existing NotRunning test must still pass
cargo test -p totsukactl
cargo clippy -p totsukactl --all-targets --all-features --locked -- -D warnings
```
Expected: new test PASSES; existing `down_returns_not_running_without_pidfile` STILL passes (because that test has no live sock); full crate green; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/totsukactl/src/commands/down.rs crates/totsukactl/tests/down_sock_fallback.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "fix(totsukactl): down falls back to supervisor.sock when pidfile is absent"
```

---

### Task 4: Push, PR, and merge

**Files:** none (housekeeping only).

- [ ] **Step 1: Final workspace validation**

```bash
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: all green, 305+ tests passing (303 before this PR + 3 new tests).

- [ ] **Step 2: Push the branch**

```bash
git push -u origin feat/shutdown-bug-fix
```

- [ ] **Step 3: Create the PR**

```bash
gh pr create --title "fix: supervisor shutdown bug surfaced by smoke test" --body "$(cat <<'EOF'
## Summary

Closes the shutdown bug surfaced during the 2026-06-30 totsukactl smoke test, where a successful `totsukactl up` led to:
1. `supervisor.pid` silently disappearing ~12s after boot
2. Children remained alive but `totsukactl down` returned `NotRunning`
3. The only way to shut down was a manual `kill -TERM <pid>` against the supervisor

### Root cause

`supervisor/main_loop.rs`'s control-dispatch loop treated `ctl_rx.recv() = None` (channel closed because all senders dropped) as a graceful-shutdown request. The senders dropped because `sock_api::serve_uds` exited (likely a transient `listener.accept()` error after the first connection), and the silent `let _ = serve_uds(...).await;` wrapper hid the failure.

### Fixes (3 commits, one per defect)

1. **`main_loop.rs`**: when `ctl_rx.recv()` returns `None`, swap the closed receiver for a fresh one whose sender is held forever — the supervisor keeps polling signals instead of shutting down. New unit test `tests/ctl_replace.rs`.
2. **`main_loop.rs`**: `serve_uds(...).await` errors are now logged via `tracing::error!` instead of being swallowed. No new test (mechanical).
3. **`commands/down.rs`**: falls back to `SupervisorClient::shutdown` over the sock when `supervisor.pid` is absent. New integration test `tests/down_sock_fallback.rs` spins an in-process UDS server and confirms the shutdown control msg is delivered.

## Test plan

- [x] `cargo test --workspace --all-features --locked` — 305+ passing (303 before + 3 new)
- [x] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — clean
- [x] `cargo fmt --all -- --check` — clean
- [ ] Manual smoke (post-merge): rerun `totsukactl up` against the real config; observe steady state for >30s; run `totsukactl down`; expect clean shutdown without the SIGTERM-by-pid workaround.

## Known follow-ups (deferred — separate PRs)

- Investigate root cause of `serve_uds` errors (`listener.accept()` returning Err on healthy macOS — was the connection error real or a hyper-side bug?). With Task 2's logging this becomes diagnosable on the next occurrence.
- Log interleaving in `supervisor.stdout` — concurrent tokio tasks writing partial lines to stdout (observed during smoke). Switch to `tracing-subscriber`'s file writer with per-write locking, or add line-buffered stdout.
- `agent-adapter` malformed herdr response parsing (`"" expected u64`) — herdr 0.7.1 wire-protocol skew.
- `github-watcher` stale `catchup_cursor` rejected by GitHub GraphQL — needs cursor invalidation when format changes.
- `agent-adapter` `GET /v1/agents` route still unimplemented (qa-service recovery best-effort).
EOF
)"
```

- [ ] **Step 4: Wait for CI; merge fast-forward**

```bash
# After CI is 5/5 green:
gh pr merge --merge --delete-branch
git checkout main && git pull --ff-only
```

---

## Self-review notes (controller-side)

**Spec coverage:**
- No spec section directly governs the `ctl_rx = None` handling — this is an implementation-correctness fix, not a spec-compliance fix. Task 1 establishes the invariant ("the supervisor's CLI IPC channel closing is NOT a shutdown trigger") in code + test; no spec edit needed.
- Spec §5 shutdown sequence is unchanged — `shutdown_stack` itself still does the right reverse-order SIGTERM dance. The bug was upstream of `shutdown_stack`, in who-decides-to-call-it.

**Type consistency:**
- `replace_closed_ctl_rx` signature `mpsc::Receiver<ControlMsg> -> mpsc::Receiver<ControlMsg>` matches what `main_loop` holds.
- `down::run` signature unchanged — only the function body is rewritten.

**Concurrency:**
- The held-sender task in `replace_closed_ctl_rx` leaks at supervisor lifetime — this is intentional and bounded (one task per `None` event, and `None` should fire at most once per supervisor lifetime). Documented in the helper's doc comment.

**Out of scope (deferred):**
- Restarting / re-binding `serve_uds` after it errors — would require state propagation and additional design. Deferred until the root cause of serve_uds errors is itself diagnosed (Task 2 makes that possible).
- Log interleaving in supervisor.stdout — separate concern, also deferred.
- `ChildSpec::Clone`-related issues if `specs` ever needed to be re-spawned mid-life — not relevant here.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-30-shutdown-bug-fix.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — fresh implementer + reviewer per task; matches the rhythm that delivered PRs #1–#8.

**2. Inline Execution** — batch with checkpoints (`superpowers:executing-plans`).

Which approach?
