# Github-Watcher Flaky Tests and CI Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two `github-watcher` integration tests that race a fixed-duration sleep against async DB work (observed to fail under CI load), and cut the `test (postgres + pgmq)` CI job's wall-clock time by removing its single biggest fixed cost: an unnecessarily broad `sqlx-cli` install that gets rebuilt from source whenever the Rust-toolchain cache is invalidated.

**Architecture:** Both flaky tests spawn an async polling loop, `tokio::time::sleep(Duration::from_millis(200))`, then cancel the loop and await its handle with a 2-second grace period — betting that 200ms of wall-clock time is enough for the loop's first poll cycle (DB reads/writes against a Postgres instance shared by every parallel test binary in the workspace) to finish. Replace the blind sleep in each test with a bounded poll-until-condition wait (check every 20ms, up to a 5s timeout) for an observable signal that the work actually completed — the antenna specific to each test (a cursor value, a DB row) — before cancelling. Also switch both tests from the default single-threaded `#[tokio::test]` runtime to `#[tokio::test(flavor = "multi_thread")]`, removing any contribution from cooperative single-thread scheduling to the race. For CI, replace the `taiki-e/install-action` fallback chain (which has no prebuilt binary for `sqlx-cli@0.8` and ends up compiling it from source with every driver feature enabled, ~3 minutes) with a direct `cargo install` restricted to the exact features the workspace's own `sqlx` dependency already uses (`postgres`, `rustls`) behind a dedicated cache keyed only on the tool name/version/OS — so it survives Rust stable version bumps instead of being invalidated by them.

**Tech Stack:** Rust (stable), tokio, sqlx, GitHub Actions.

## Global Constraints

- Workspace-wide `#![forbid(unsafe_code)]` — no unsafe code (not applicable to test-only or YAML changes here, noted for completeness).
- GitHub Actions third-party actions must be pinned to a full commit SHA with an inline version-tag comment (project rule in `~/.claude/rules/github-action-sha.md` / the dotfiles-managed rules); resolve SHAs with `~/.claude/scripts/resolve-gh-action-sha.sh <owner/repo>`, never hand-guess one.
- No new `SystemTime::now()` / `chrono::Utc::now()` call sites (clippy-denied) — use `tokio::time::Instant::now()` for the new polling deadlines in tests (already the pattern this session used in `totsukactl`'s `down_budget.rs`).
- `sqlx-cli`'s installed feature set must mirror the workspace's own `sqlx` dependency features exactly: `Cargo.toml:47` declares `features = ["runtime-tokio", "tls-rustls", "postgres", ...]` — so `sqlx-cli` must be installed with `--no-default-features --features postgres,rustls` (not the default `postgres,sqlite,mysql,native-tls,completions`).
- Report contract for every task in this plan must include running `cargo fmt --all -- --check` AND `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` before reporting DONE (a `clippy::items_after_test_module` regression shipped past two prior task/whole-branch reviews in this repo's history because clippy wasn't in the required checks — do not repeat that gap).

---

### Task 1: Fix the fixed-sleep race in `e2e_cursor_resume.rs`

**Files:**
- Modify: `crates/github-watcher/tests/e2e_cursor_resume.rs`

**Interfaces:**
- Consumes: `github_watcher::cursor::{get, CursorKey}` (already imported at the top of the file), `github_watcher::polling::issues::{run_issues_loop, IssuesLoopConfig}` (already imported).
- Produces: a modified `run_once` signature — `async fn run_once(pool: sqlx::PgPool, publisher: Arc<Publisher>, mock: Arc<MockGhClient>, tracker: RepoTracker, catchup: chrono::Duration, expect_cursor_prefix: &str)` (adds one new trailing parameter to the existing function). No other file in the workspace calls `run_once` (it is a private test-file helper), so this signature change has no other call sites to update.

The current file (for reference — do not copy this into the diff, it is being replaced):

```rust
async fn run_once(
    pool: sqlx::PgPool,
    publisher: Arc<Publisher>,
    mock: Arc<MockGhClient>,
    tracker: RepoTracker,
    catchup: chrono::Duration,
) {
    let cfg = IssuesLoopConfig {
        poll_interval: Duration::from_millis(50),
        catchup_window: catchup,
    };
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    let h = tokio::spawn(async move {
        run_issues_loop(
            pool,
            publisher,
            mock as Arc<dyn GhClient>,
            tracker,
            Arc::new(SystemClock),
            HealthState::new(),
            cfg,
            s2,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}
```

The bug: `poll_repo` (in `crates/github-watcher/src/polling/issues.rs`) only calls `set()` on the cursor **after** every matching issue for that poll cycle has already been published via `publisher.send(...)`. So "the cursor reached the expected value" is a safe, order-guaranteed proxy for "every publish for this poll cycle already landed in the DB" — waiting on the cursor instead of a fixed sleep removes the race entirely, because we wait exactly as long as the real work takes (with a generous upper bound for a genuinely broken case), not a guessed constant.

- [ ] **Step 1: Replace `run_once` with a version that polls for the cursor instead of sleeping a fixed duration**

Replace the entire `run_once` function (shown above) with:

```rust
async fn run_once(
    pool: sqlx::PgPool,
    publisher: Arc<Publisher>,
    mock: Arc<MockGhClient>,
    tracker: RepoTracker,
    catchup: chrono::Duration,
    expect_cursor_prefix: &str,
) {
    let cfg = IssuesLoopConfig {
        poll_interval: Duration::from_millis(50),
        catchup_window: catchup,
    };
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    let poll_pool = pool.clone();
    let h = tokio::spawn(async move {
        run_issues_loop(
            pool,
            publisher,
            mock as Arc<dyn GhClient>,
            tracker,
            Arc::new(SystemClock),
            HealthState::new(),
            cfg,
            s2,
        )
        .await
    });

    // Wait for the cursor to actually reach the expected value instead of
    // guessing a fixed sleep duration — poll_repo only sets the cursor after
    // every matching issue for this cycle has already published, so this is
    // a safe, order-guaranteed readiness signal.
    let key = CursorKey::issues("acme/cur");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(v) = get(&poll_pool, &key).await.unwrap() {
            if v.starts_with(expect_cursor_prefix) {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "cursor for acme/cur did not reach prefix {expect_cursor_prefix:?} within 5s"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}
```

- [ ] **Step 2: Update both call sites to pass the expected cursor prefix**

The first call site currently reads:

```rust
    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
    )
    .await;
```

and is immediately followed later by `assert!(cur.starts_with("2026-06-29T12:00:00"));`. Change it to:

```rust
    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
        "2026-06-29T12:00:00",
    )
    .await;
```

The second call site currently reads:

```rust
    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
    )
    .await;
```

and is immediately followed later by `assert!(cur2.starts_with("2026-06-29T13:00:00"));`. Change it to:

```rust
    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
        "2026-06-29T13:00:00",
    )
    .await;
```

- [ ] **Step 3: Switch the test to the multi-threaded tokio runtime**

Change:

```rust
#[tokio::test]
async fn issues_cursor_resumes_and_skips_already_seen() {
```

to:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn issues_cursor_resumes_and_skips_already_seen() {
```

- [ ] **Step 4: Run the test against the local Postgres to verify it passes**

This test needs `DATABASE_URL` pointing at a Postgres instance with the `pgmq` extension and this project's migrations applied (the test returns early — a silent skip — if `DATABASE_URL` is unset, so confirm it is set before trusting a pass). If a local Postgres is already running for this project (check with `docker ps` for a container publishing port 5432 — do not start a new one if one is already running; ask if none is running and you cannot tell how to reach it), run:

```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5432/totsuka" cargo test --package github-watcher --test e2e_cursor_resume -- --nocapture
```

(Adjust the URL only if the project's actual local Postgres uses different credentials/db name/port — check `~/.config/totsuka/config.toml`'s `[postgres]` section or ask if unclear; do not guess new credentials.)

Expected: `test issues_cursor_resumes_and_skips_already_seen ... ok`, 1 passed, 0 failed.

- [ ] **Step 5: Run it 5 times in a row to build confidence it is no longer flaky**

```bash
for i in 1 2 3 4 5; do
  DATABASE_URL="postgres://postgres:postgres@localhost:5432/totsuka" cargo test --package github-watcher --test e2e_cursor_resume -- --nocapture || break
done
```

Expected: all 5 runs report `ok`, 0 failures. If any run fails, read the failure output — a panic from Step 1's new deadline loop (`"cursor for acme/cur did not reach prefix ... within 5s"`) is a Critical finding to report (means the 5s bound is genuinely insufficient in this environment); a failure elsewhere is a different, pre-existing bug — do not just increase the timeout without understanding why 5s wasn't enough first.

- [ ] **Step 6: Format, lint, and commit**

Run `cargo fmt --all -- --check` (fix with `cargo fmt --all` if it reports diffs) and `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (must report no issues — this plan's Global Constraints require both before every task is reported DONE). Then:

```bash
git add crates/github-watcher/tests/e2e_cursor_resume.rs
git commit -m "fix(github-watcher): replace fixed-sleep race with cursor-poll wait in e2e_cursor_resume test"
```

---

### Task 2: Fix the fixed-sleep race in `e2e_project_loop.rs`

**Files:**
- Modify: `crates/github-watcher/tests/e2e_project_loop.rs`

**Interfaces:**
- Consumes: nothing from Task 1 (independent file, independent fix, same technique applied to a different observable condition).
- Produces: nothing consumed by later tasks.

This file's test (`project_loop_publishes_status_changed_for_every_diff`) spawns `run_project_loop` (over two mocked project-item pages) into a `pool2`-using task, keeping the original `pool` handle available afterward (already not moved — only `pool2`, a clone, is moved into the spawned task). It then does the same `tokio::time::sleep(Duration::from_millis(200)).await; shutdown.cancel(); ...` pattern before asserting three `gh_item_status` rows and three published queue envelopes. Item `"E2E_C"` is on the second (last) mocked page and ends with `status_display: Some("🏁 完了".into())` mapping to `"released"` — its row appearing with that status is a safe proxy that both pages have been fully processed (whether the loop processes both pages within one poll tick or across two, either way this readiness check waits for the actually-observable end state instead of guessing a duration).

- [ ] **Step 1: Replace the fixed sleep with a poll-until-condition wait**

Change:

```rust
    // Allow one tick
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
```

to:

```rust
    // Wait for the loop to actually finish processing both mocked pages
    // (observable via E2E_C's row reaching "released") instead of guessing
    // a fixed sleep duration.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_C'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        if matches!(&row, Some((Some(s),)) if s == "released") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("gh_item_status for E2E_C did not reach 'released' within 5s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
```

- [ ] **Step 2: Switch the test to the multi-threaded tokio runtime**

Change:

```rust
#[tokio::test]
async fn project_loop_publishes_status_changed_for_every_diff() {
```

to:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn project_loop_publishes_status_changed_for_every_diff() {
```

- [ ] **Step 3: Run the test against the local Postgres to verify it passes**

```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5432/totsuka" cargo test --package github-watcher --test e2e_project_loop -- --nocapture
```

(Use the same `DATABASE_URL` value confirmed working in Task 1 Step 4 — do not guess new credentials if that one was adjusted there.)

Expected: `test project_loop_publishes_status_changed_for_every_diff ... ok`, 1 passed, 0 failed.

- [ ] **Step 4: Run it 5 times in a row to build confidence it is no longer flaky**

```bash
for i in 1 2 3 4 5; do
  DATABASE_URL="postgres://postgres:postgres@localhost:5432/totsuka" cargo test --package github-watcher --test e2e_project_loop -- --nocapture || break
done
```

Expected: all 5 runs report `ok`, 0 failures. Same guidance as Task 1 Step 5 on interpreting a failure.

- [ ] **Step 5: Format, lint, and commit**

Run `cargo fmt --all -- --check` (fix with `cargo fmt --all` if it reports diffs) and `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (must report no issues). Then:

```bash
git add crates/github-watcher/tests/e2e_project_loop.rs
git commit -m "fix(github-watcher): replace fixed-sleep race with row-poll wait in e2e_project_loop test"
```

---

### Task 3: Tune the `test (postgres + pgmq)` CI job's `sqlx-cli` install

**Files:**
- Modify: `.github/workflows/ci.yml:81-84`

**Interfaces:**
- Consumes: nothing from Tasks 1-2 (independent, config-only change).
- Produces: nothing consumed by later tasks.

Measured root cause (from a real CI run, job `test (postgres + pgmq)`, ~6m30s total): the `taiki-e/install-action` step has no prebuilt binary for `sqlx-cli@0.8`, falls back to `cargo-binstall`, which itself fails to find a quickinstall artifact (two 404s) and falls all the way back to `cargo install` from source — compiling `sqlx-cli` with its **default** feature set (`postgres`, `sqlite`, `mysql`, `native-tls`, `completions` — confirmed via `cargo info sqlx-cli@0.8.6`) even though this project only ever uses Postgres via rustls (`Cargo.toml:47`: `features = ["runtime-tokio", "tls-rustls", "postgres", ...]`). This from-source build alone took ~2m54s, the single largest chunk of the job. It is retriggered on **every** cache miss — including the routine case where the pinned `stable` Rust toolchain itself gets a new point release, which changes `Swatinem/rust-cache`'s environment-hash-based cache key and invalidates the `~/.cargo/bin` cache that would otherwise have this binary pre-built (this exact thing was observed happening in this repo's CI history: caches under key prefix `v0-rust-test-Linux-x64-24a7a63d` became stale after a `stable` bump changed the prefix to `v0-rust-test-Linux-x64-497e886f`, forcing full rebuilds of *every* cached tool including `sqlx-cli`, on an unrelated PR).

The fix: (1) install only the features this project needs, and (2) cache the `sqlx-cli` binary under a key that depends only on the tool name/version/OS — not on the Rust toolchain's environment hash — so a `stable` version bump never forces a re-download-and-recompile of a generic, project-independent CLI tool.

- [ ] **Step 1: Resolve the pinned SHA for `actions/cache`**

Run: `~/.claude/scripts/resolve-gh-action-sha.sh actions/cache`
Expected output (may differ if a newer release has shipped since this plan was written — if so, use the SHA and version tag the script actually prints instead of the one below, and use that value everywhere below that this plan writes `caa296126883cff596d87d8935842f9db880ef25 # v5.1.0`):

```
actions/cache@caa296126883cff596d87d8935842f9db880ef25 # v5.1.0
```

- [ ] **Step 2: Replace the `Install sqlx-cli` step**

In `.github/workflows/ci.yml`, the `test` job currently has (lines 81-84):

```yaml
      - name: Install sqlx-cli
        uses: taiki-e/install-action@bffeee26d4db9be238a4ea78d8826604ebcb594d # v2.82.5
        with:
          tool: sqlx-cli@0.8
```

Replace it with (using the SHA/version confirmed in Step 1):

```yaml
      - name: Cache sqlx-cli binary
        id: sqlx-cli-cache
        uses: actions/cache@caa296126883cff596d87d8935842f9db880ef25 # v5.1.0
        with:
          path: ~/.cargo/bin/sqlx
          key: sqlx-cli-0.8.6-postgres-rustls-${{ runner.os }}
      - name: Install sqlx-cli
        if: steps.sqlx-cli-cache.outputs.cache-hit != 'true'
        run: cargo install sqlx-cli --version 0.8.6 --no-default-features --features postgres,rustls --locked
```

This cache key intentionally excludes any Rust-toolchain or `Cargo.lock` hash component — `sqlx-cli` is a generic, project-independent tool, so it should only need to be rebuilt when its own pinned version (`0.8.6`) changes, never when this project's dependencies or the pinned Rust toolchain change.

- [ ] **Step 3: Validate the YAML syntax locally**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo VALID`
Expected: `VALID` (this only checks the file parses as YAML; it cannot validate GitHub Actions semantics — that is confirmed by Step 5's real CI run).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: install sqlx-cli with only the features this project uses, cache it independent of the Rust toolchain hash"
```

- [ ] **Step 5: Push this branch and observe one real CI run of the `test` job to confirm the new steps work**

This cannot be verified locally (the `path:` cache restore/save behavior and the `steps.sqlx-cli-cache.outputs.cache-hit` conditional only execute for real inside GitHub Actions). After this task's commit is pushed as part of this plan's branch, watch the `test (postgres + pgmq)` job's logs for:
- The new `Cache sqlx-cli binary` step running before `Install sqlx-cli`.
- On the **first** run (expected cache miss, since this exact key has never been saved before): `Install sqlx-cli` step runs and succeeds, installing only `postgres` + `rustls` features (confirm in its log output — it should no longer print `Downloaded` lines for `sqlx-mysql`, `sqlx-sqlite`, `native-tls`, or `openssl-sys` crates; if any of those appear, the `--no-default-features` flag did not take effect and this is a Critical finding).
- `Enable pgmq extension` and `Apply migrations` steps still succeed (proves the restricted-feature `sqlx` binary still works for Postgres-only migration commands).
- Record the total `test (postgres + pgmq)` job duration from this run in the task report for comparison against the ~6m30s baseline measured before this plan (the from-source `sqlx-cli` build alone should no longer be the ~2m54s chunk it was — expect it to be smaller due to the trimmed feature set, though this first run still pays a real compile cost since the new cache key has never been populated; the win from the dedicated cache shows up on the *next* run using this key, which this task cannot force locally).

---

## Manual Verification (after all 3 tasks land)

1. Push this branch and let CI run in full. Confirm `test (postgres + pgmq)` passes (no `issues_cursor_resumes_and_skips_already_seen` or `project_loop_publishes_status_changed_for_every_diff` failures).
2. Trigger a second CI run on the same branch (e.g. an empty commit, or re-running the workflow) and confirm the `Cache sqlx-cli binary` step reports a cache hit (`steps.sqlx-cli-cache.outputs.cache-hit` is `true` — visible by the `Install sqlx-cli` step being skipped in the run's step list) and the job's total duration drops meaningfully versus the first run.
