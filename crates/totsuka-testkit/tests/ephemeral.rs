//! Behaviour of the ephemeral-DB factory, against a real Postgres
//! (skipped without DATABASE_URL, like every DB test in this workspace).

use totsuka_testkit::create_from;

fn admin_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn creates_isolated_migrated_db_and_never_touches_admin_db() {
    let Some(url) = admin_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let db = create_from(&url).await.unwrap();

    // Connected to a totsuka_test_* database, not the admin one.
    let (current,): (String,) = sqlx::query_as("SELECT current_database()")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        current.starts_with("totsuka_test_"),
        "expected ephemeral db, got {current}"
    );
    assert_eq!(current, db.name());
    assert!(db.url().contains(db.name()));

    // Migrations applied: app tables + pgmq extension available.
    let (tasks,): (i64,) = sqlx::query_as("SELECT count(*) FROM tasks")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(tasks, 0, "fresh db must be empty");
    sqlx::query("SELECT pgmq.create('testkit_probe')")
        .execute(&db.pool)
        .await
        .expect("pgmq extension must be installed by migration 0000");
}

#[tokio::test]
async fn two_ephemeral_dbs_are_independent() {
    let Some(url) = admin_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let a = create_from(&url).await.unwrap();
    let b = create_from(&url).await.unwrap();
    assert_ne!(a.name(), b.name());

    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor) VALUES ('github', 'probe', 'x')",
    )
    .execute(&a.pool)
    .await
    .unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM catchup_cursor")
        .fetch_one(&b.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "fixture in db A must not appear in db B");
}

#[tokio::test]
async fn stale_test_dbs_are_swept_on_next_create() {
    let Some(url) = admin_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    // Plant a "stale" test db (timestamp older than the sweep threshold).
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let stale_name = "totsuka_test_1000000000_deadbeef";
    let _ = sqlx::query(&format!(
        "DROP DATABASE IF EXISTS {stale_name} WITH (FORCE)"
    ))
    .execute(&admin)
    .await;
    sqlx::query(&format!("CREATE DATABASE {stale_name}"))
        .execute(&admin)
        .await
        .unwrap();

    let fresh = create_from(&url).await.unwrap();

    let stale_exists: Option<(String,)> =
        sqlx::query_as("SELECT datname FROM pg_database WHERE datname = $1")
            .bind(stale_name)
            .fetch_optional(&admin)
            .await
            .unwrap();
    assert!(stale_exists.is_none(), "stale test db must be swept");

    // The fresh db (young timestamp) must survive its own sweep.
    let fresh_exists: Option<(String,)> =
        sqlx::query_as("SELECT datname FROM pg_database WHERE datname = $1")
            .bind(fresh.name())
            .fetch_optional(&admin)
            .await
            .unwrap();
    assert!(fresh_exists.is_some(), "fresh db must not be swept");
}

#[tokio::test]
async fn sweep_skips_stale_named_db_with_active_connections() {
    let Some(url) = admin_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    // Plant a stale-named db and HOLD a connection to it — simulating a
    // long-running local test session that crossed the staleness window.
    let held_name = "totsuka_test_1000000001_beefbeef";
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {held_name} WITH (FORCE)"))
        .execute(&admin)
        .await;
    sqlx::query(&format!("CREATE DATABASE {held_name}"))
        .execute(&admin)
        .await
        .unwrap();
    let held_url = url
        .rsplit_once('/')
        .map(|(b, _)| format!("{b}/{held_name}"))
        .unwrap();
    let _held = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect(&held_url)
        .await
        .unwrap();

    let _fresh = create_from(&url).await.unwrap();

    let still_there: Option<(String,)> =
        sqlx::query_as("SELECT datname FROM pg_database WHERE datname = $1")
            .bind(held_name)
            .fetch_optional(&admin)
            .await
            .unwrap();
    assert!(
        still_there.is_some(),
        "a stale-named db with live connections must not be swept"
    );

    drop(_held);
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {held_name} WITH (FORCE)"))
        .execute(&admin)
        .await;
}

#[tokio::test]
async fn non_concurrency_db_errors_are_not_retryable() {
    let Some(url) = admin_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    // A syntax error (42601) must not be classified as the concurrent
    // template-copy condition (55006) that warrants a retry.
    let err = sqlx::query("CREATE DATABASE")
        .execute(&admin)
        .await
        .unwrap_err();
    assert!(!totsuka_testkit::is_concurrent_template_copy(&err));
}
