use github_watcher::cursor::{get, set, set_in_tx, CursorKey};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn round_trip_project_cursor() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let k = CursorKey::project_items();
    set(&pool, &k, "abc").await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("abc".into()));
    set(&pool, &k, "def").await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("def".into()));
}

#[tokio::test]
async fn issues_cursor_is_repo_scoped() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let a = CursorKey::issues("acme/a");
    let b = CursorKey::issues("acme/b");
    set(&pool, &a, "2026-06-01T00:00:00Z").await.unwrap();
    set(&pool, &b, "2026-06-02T00:00:00Z").await.unwrap();
    assert_eq!(
        get(&pool, &a).await.unwrap(),
        Some("2026-06-01T00:00:00Z".into())
    );
    assert_eq!(
        get(&pool, &b).await.unwrap(),
        Some("2026-06-02T00:00:00Z".into())
    );
}

#[tokio::test]
async fn set_in_tx_is_atomic_with_rollback() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let k = CursorKey::prs("acme/tx");
    set(&pool, &k, "baseline").await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    set_in_tx(&mut tx, &k, "should-roll-back").await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("baseline".into()));
}
