//! SIGTERM must make `wait_for_signals` return promptly (readyz flip + drain
//! is the server's job, bounded by its own deadline) — NOT sit out a fixed
//! sleep. A fixed sleep >= the supervisor's `shutdown_grace_secs` guarantees
//! a 2nd-SIGTERM escalation on every `totsukactl down`.
//!
//! This file sends SIGTERM to its own test process, so it must stay a
//! separate integration-test binary (own process) and contain only this test.

use std::sync::Arc;
use std::time::Duration;

use agent_adapter::herdr::mock::MockHerdr;
use agent_adapter::lifecycle::wait_for_signals;
use agent_adapter::repo::RepoRegistry;
use agent_adapter::server::AppState;
use agent_adapter::worktree::WorktreeManager;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

fn state() -> AppState {
    AppState {
        herdr: Arc::new(MockHerdr::new()),
        repos: Arc::new(RepoRegistry::new()),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    }
}

#[tokio::test]
async fn sigterm_returns_promptly_without_fixed_drain_sleep() {
    // Pre-register a SIGTERM handler so a signal delivered before
    // `wait_for_signals` installs its own can never kill the test process.
    let _disposition_guard =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();

    let st = state();
    st.health.set_ready(true).await;
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut handle = tokio::spawn(wait_for_signals(
        st,
        "/nonexistent-config".into(),
        shutdown.clone(),
    ));

    // Condition-based: keep re-sending SIGTERM until the task observes it
    // (its handler registration races with the first send).
    let pid = std::process::id().to_string();
    let deadline = tokio::time::Duration::from_secs(3);
    let result = tokio::time::timeout(deadline, async {
        loop {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status();
            tokio::select! {
                r = &mut handle => break r,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    })
    .await;

    let joined = result.expect(
        "wait_for_signals must return well within 3s of SIGTERM, not sleep out a fixed drain",
    );
    joined.expect("join").expect("wait_for_signals result");
    assert!(
        shutdown.is_cancelled(),
        "SIGTERM must cancel the shutdown token so the UDS server drains"
    );
}
