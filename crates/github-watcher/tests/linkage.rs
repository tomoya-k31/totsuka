use github_watcher::linkage::{resolve_task_id, task_id_from_trailer, task_id_short_from_branch};

#[test]
fn branch_extracts_short() {
    assert_eq!(
        task_id_short_from_branch("totsuka/abc123def456/implv").as_deref(),
        Some("abc123def456"),
    );
}

#[test]
fn branch_rejects_malformed() {
    assert!(task_id_short_from_branch("feature/foo").is_none());
    assert!(task_id_short_from_branch("totsuka//implv").is_none());
    assert!(task_id_short_from_branch("totsuka/abc").is_none());
    assert!(task_id_short_from_branch("totsuka/abc/implv/extra").is_none());
}

#[test]
fn trailer_picks_last() {
    let body = "intro\n\nTotsuka-Task: PVTI_first\n\nmore\nTotsuka-Task: PVTI_last\n";
    assert_eq!(task_id_from_trailer(body).as_deref(), Some("PVTI_last"));
}

#[test]
fn trailer_no_match_returns_none() {
    assert!(task_id_from_trailer("hello world").is_none());
}

#[tokio::test]
async fn resolve_prefers_trailer_on_mismatch() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    // seed two tasks
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_full_xxxxxxxxxxxx").bind("xxxxxxxxxxxx")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_full_yyyyyyyyyyyy").bind("yyyyyyyyyyyy")
        .execute(&pool).await.unwrap();

    let r = resolve_task_id(
        &pool,
        "totsuka/xxxxxxxxxxxx/implv",
        Some("Totsuka-Task: PVTI_full_yyyyyyyyyyyy\n"),
    )
    .await
    .unwrap();
    assert_eq!(r.as_deref(), Some("PVTI_full_yyyyyyyyyyyy"));
}
