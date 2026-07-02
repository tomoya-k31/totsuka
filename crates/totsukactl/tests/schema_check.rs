//! Needs DATABASE_URL pointing at a Postgres server; the test runs against
//! its own ephemeral, migrated database (see totsuka-testkit).

use totsukactl::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};

#[tokio::test]
async fn handshake_returns_target_on_migrated_db() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let got = check_schema_version(&pool).await.expect("handshake ok");
    assert_eq!(got, TARGET_SCHEMA_VERSION);
}
