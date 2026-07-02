use chrono::{TimeZone, Utc};
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, IssueUpdate, MockGhClient, RepoSlug};
use github_watcher::polling::issues::{run_issues_loop, IssuesLoopConfig};
use github_watcher::polling::RepoTracker;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, Consumer, Publisher};
use totsuka_core::{MockClock, SystemClock};
use totsuka_telemetry::HealthState;

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
    // Fixed instant safely after all fixture timestamps (t0..t3 are on
    // 2026-06-29, latest at 13:00) so poll_repo's `since = now - catchup`
    // window deterministically covers the fixtures regardless of the real
    // wall-clock date. Using SystemClock here (as before) made this test a
    // time bomb: it silently broke once real time drifted more than
    // catchup_window past the fixtures' hardcoded dates.
    let fixed_now = Utc.with_ymd_and_hms(2026, 6, 29, 14, 0, 0).unwrap();
    let h = tokio::spawn(async move {
        run_issues_loop(
            pool,
            publisher,
            mock as Arc<dyn GhClient>,
            tracker,
            Arc::new(MockClock::new(fixed_now)),
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
            panic!("cursor for acme/cur did not reach prefix {expect_cursor_prefix:?} within 5s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn issues_cursor_resumes_and_skips_already_seen() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();

    let repo = RepoSlug::parse("acme/cur").unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='issues:acme/cur'")
        .execute(&pool)
        .await
        .unwrap();

    let queue = format!("ghw_cur_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let t0 = Utc.with_ymd_and_hms(2026, 6, 29, 10, 0, 0).unwrap();
    let t1 = Utc.with_ymd_and_hms(2026, 6, 29, 11, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let t3 = Utc.with_ymd_and_hms(2026, 6, 29, 13, 0, 0).unwrap();
    mock.set_issues(
        &repo,
        vec![
            IssueUpdate {
                node_id: "I1".into(),
                repo: repo.clone(),
                number: 1,
                updated_at: t0,
                state: "open".into(),
            },
            IssueUpdate {
                node_id: "I2".into(),
                repo: repo.clone(),
                number: 2,
                updated_at: t1,
                state: "open".into(),
            },
            IssueUpdate {
                node_id: "I3".into(),
                repo: repo.clone(),
                number: 3,
                updated_at: t2,
                state: "open".into(),
            },
        ],
    );

    let tracker = RepoTracker::new();
    tracker.insert(repo.clone()).await;

    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
        "2026-06-29T12:00:00",
    )
    .await;

    // Drain queue — should have 3
    let consumer = Consumer::new(queue.clone());
    let mut drained = 0;
    while let Some((mid, _)) = consumer.poll_one(&pool, 1).await.unwrap() {
        consumer.ack(&pool, mid).await.unwrap();
        drained += 1;
    }
    assert_eq!(drained, 3);
    let cur = get(&pool, &CursorKey::issues("acme/cur"))
        .await
        .unwrap()
        .unwrap();
    assert!(cur.starts_with("2026-06-29T12:00:00"));

    // Add a new later issue + same 3 old ones. Only I4 should publish.
    mock.set_issues(
        &repo,
        vec![
            IssueUpdate {
                node_id: "I1".into(),
                repo: repo.clone(),
                number: 1,
                updated_at: t0,
                state: "open".into(),
            },
            IssueUpdate {
                node_id: "I2".into(),
                repo: repo.clone(),
                number: 2,
                updated_at: t1,
                state: "open".into(),
            },
            IssueUpdate {
                node_id: "I3".into(),
                repo: repo.clone(),
                number: 3,
                updated_at: t2,
                state: "open".into(),
            },
            IssueUpdate {
                node_id: "I4".into(),
                repo: repo.clone(),
                number: 4,
                updated_at: t3,
                state: "open".into(),
            },
        ],
    );
    run_once(
        pool.clone(),
        publisher.clone(),
        mock.clone(),
        tracker.clone(),
        chrono::Duration::hours(48),
        "2026-06-29T13:00:00",
    )
    .await;

    let mut drained = 0;
    while let Some((mid, env)) = consumer.poll_one(&pool, 1).await.unwrap() {
        assert_eq!(env.payload["issue_node_id"], "I4");
        consumer.ack(&pool, mid).await.unwrap();
        drained += 1;
    }
    assert_eq!(drained, 1);
    let cur2 = get(&pool, &CursorKey::issues("acme/cur"))
        .await
        .unwrap()
        .unwrap();
    assert!(cur2.starts_with("2026-06-29T13:00:00"));
}
