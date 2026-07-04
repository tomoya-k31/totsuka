//! Sweeper must drop the thread mapping even when the pane is already gone
//! (stop returns not_found) — otherwise the row leaks forever and later
//! thread continuations target a dead terminal.

use chrono::{Duration as ChronoDuration, Utc};
use qa_service::adapter_client::MockAdapter;
use qa_service::sweeper::run_sweeper;
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;

#[tokio::test]
async fn sweeper_drops_mapping_when_pane_already_gone() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_stop_failure("adapter: 404 /v1/agents/term_gone: agent_not_found");

    // Idle far beyond the TTL.
    let stale_at = Utc::now() - ChronoDuration::hours(2);
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());
    thread_map
        .upsert(&ThreadMapping {
            thread_ts: thread_ts.clone(),
            terminal_id: "term_gone".into(),
            repo: "acme/api".into(),
            last_activity_at: stale_at,
            created_at: stale_at,
        })
        .await
        .unwrap();

    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(run_sweeper(
        thread_map.clone(),
        adapter.clone(),
        clock.clone(),
        ChronoDuration::seconds(60),
        1, // tick every second; first tick fires immediately
        shutdown.clone(),
    ));

    // Poll for the deletion instead of a fixed sleep (CI-load safe).
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        if thread_map.get(&thread_ts).await.unwrap().is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mapping was not dropped within 10s"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    shutdown.cancel();
    let _ = handle.await;

    // The sweeper did try to stop the pane before dropping the row.
    assert!(!adapter.expected_stops().is_empty());
}
