use chrono::Utc;
use orchestrator::repository::{PgRepository, Repository, Task};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::{SystemClock, TaskId};

#[tokio::test]
async fn upsert_and_get_round_trip() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let repo = PgRepository::new(pool, Arc::new(SystemClock));

    let id = TaskId::new(format!("PVTI_test_{}", uuid::Uuid::new_v4().simple()));
    let t = Task {
        id: id.clone(),
        task_id_short: id.short(),
        repo: "x/y".into(),
        pr_node_id: None,
        current_column: "inbox".into(),
        current_phase: None,
        impl_verify_attempt: 0,
        suppress_writeback_until_human_move: false,
        spawned_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    repo.upsert(&t).await.unwrap();
    let got = repo.get(&id).await.unwrap().expect("present");
    assert_eq!(got.repo, "x/y");
    assert_eq!(got.current_column, "inbox");

    let n = repo.bump_attempt(&id).await.unwrap();
    assert_eq!(n, 1);
    let got2 = repo.get(&id).await.unwrap().unwrap();
    assert_eq!(got2.impl_verify_attempt, 1);
}
