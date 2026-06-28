use chrono::{TimeZone, Utc};
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, IssueUpdate, MockGhClient, RepoSlug};
use github_watcher::polling::issues::{run_issues_loop, IssuesLoopConfig};
use github_watcher::polling::RepoTracker;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{Consumer, Publisher, create_queue};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

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
        run_issues_loop(pool, publisher, mock as Arc<dyn GhClient>, tracker, Arc::new(SystemClock), HealthState::new(), cfg, s2).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}

#[tokio::test]
async fn issues_cursor_resumes_and_skips_already_seen() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();

    let repo = RepoSlug::parse("acme/cur").unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='issues:acme/cur'")
        .execute(&pool).await.unwrap();

    let queue = format!("ghw_cur_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let t0 = Utc.with_ymd_and_hms(2026, 6, 29, 10, 0, 0).unwrap();
    let t1 = Utc.with_ymd_and_hms(2026, 6, 29, 11, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let t3 = Utc.with_ymd_and_hms(2026, 6, 29, 13, 0, 0).unwrap();
    mock.set_issues(&repo, vec![
        IssueUpdate { node_id: "I1".into(), repo: repo.clone(), number: 1, updated_at: t0, state: "open".into() },
        IssueUpdate { node_id: "I2".into(), repo: repo.clone(), number: 2, updated_at: t1, state: "open".into() },
        IssueUpdate { node_id: "I3".into(), repo: repo.clone(), number: 3, updated_at: t2, state: "open".into() },
    ]);

    let tracker = RepoTracker::new();
    tracker.insert(repo.clone()).await;

    run_once(pool.clone(), publisher.clone(), mock.clone(), tracker.clone(), chrono::Duration::hours(48)).await;

    // Drain queue — should have 3
    let consumer = Consumer::new(queue.clone());
    let mut drained = 0;
    loop {
        if let Some((mid, _)) = consumer.poll_one(&pool, 1).await.unwrap() {
            consumer.ack(&pool, mid).await.unwrap();
            drained += 1;
        } else {
            break;
        }
    }
    assert_eq!(drained, 3);
    let cur = get(&pool, &CursorKey::issues("acme/cur")).await.unwrap().unwrap();
    assert!(cur.starts_with("2026-06-29T12:00:00"));

    // Add a new later issue + same 3 old ones. Only I4 should publish.
    mock.set_issues(&repo, vec![
        IssueUpdate { node_id: "I1".into(), repo: repo.clone(), number: 1, updated_at: t0, state: "open".into() },
        IssueUpdate { node_id: "I2".into(), repo: repo.clone(), number: 2, updated_at: t1, state: "open".into() },
        IssueUpdate { node_id: "I3".into(), repo: repo.clone(), number: 3, updated_at: t2, state: "open".into() },
        IssueUpdate { node_id: "I4".into(), repo: repo.clone(), number: 4, updated_at: t3, state: "open".into() },
    ]);
    run_once(pool.clone(), publisher.clone(), mock.clone(), tracker.clone(), chrono::Duration::hours(48)).await;

    let mut drained = 0;
    loop {
        if let Some((mid, env)) = consumer.poll_one(&pool, 1).await.unwrap() {
            assert_eq!(env.payload["issue_node_id"], "I4");
            consumer.ack(&pool, mid).await.unwrap();
            drained += 1;
        } else {
            break;
        }
    }
    assert_eq!(drained, 1);
    let cur2 = get(&pool, &CursorKey::issues("acme/cur")).await.unwrap().unwrap();
    assert!(cur2.starts_with("2026-06-29T13:00:00"));
}
