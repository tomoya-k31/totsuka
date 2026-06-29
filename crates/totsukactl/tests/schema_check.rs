//! Requires DATABASE_URL pointing at a migrated pgmq instance (CI provides one).

use sqlx::postgres::PgPoolOptions;
use totsukactl::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn handshake_returns_target_on_migrated_db() {
    let Some(url) = db_url() else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    let got = check_schema_version(&pool).await.expect("handshake ok");
    assert_eq!(got, TARGET_SCHEMA_VERSION);
}
