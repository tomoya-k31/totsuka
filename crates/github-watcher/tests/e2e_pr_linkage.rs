use chrono::{TimeZone, Utc};
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, MockGhClient, PrUpdate, RepoSlug};
use github_watcher::polling::prs::{run_prs_loop, PrsLoopConfig};
use github_watcher::polling::RepoTracker;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, Consumer, Publisher};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::test]
async fn pr_merged_publishes_with_task_id_from_branch() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();

    let task_id = "PVTI_full_aaaaaaaaaaaa";
    let task_short = "aaaaaaaaaaaa";
    sqlx::query("DELETE FROM tasks WHERE id = $1")
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'impl_verify')")
        .bind(task_id).bind(task_short).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='prs:acme/r'")
        .execute(&pool)
        .await
        .unwrap();

    let queue = format!("ghw_pr_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let repo = RepoSlug::parse("acme/r").unwrap();
    let merged_at = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    mock.set_prs(
        &repo,
        vec![PrUpdate {
            node_id: "PR_node_1".into(),
            repo: repo.clone(),
            number: 7,
            head_ref: format!("totsuka/{task_short}/implv"),
            body: None,
            merged: true,
            merged_at: Some(merged_at),
            updated_at: merged_at,
        }],
    );

    let tracker = RepoTracker::new();
    tracker.insert(repo.clone()).await;

    let cfg = PrsLoopConfig {
        poll_interval: Duration::from_millis(50),
        catchup_window: chrono::Duration::hours(48),
    };
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    let pool2 = pool.clone();
    let h = tokio::spawn(async move {
        run_prs_loop(
            pool2,
            publisher,
            mock.clone() as Arc<dyn GhClient>,
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

    let consumer = Consumer::new(queue.clone());
    let (mid, env) = consumer
        .poll_one(&pool, 1)
        .await
        .unwrap()
        .expect("one envelope");
    assert_eq!(env.event_type, "github.pr_merged_ready");
    assert_eq!(env.payload["item_id"], task_id);
    assert_eq!(env.payload["repo"], "acme/r");
    consumer.ack(&pool, mid).await.unwrap();

    let cur = get(&pool, &CursorKey::prs("acme/r")).await.unwrap();
    assert!(cur.is_some());
}
