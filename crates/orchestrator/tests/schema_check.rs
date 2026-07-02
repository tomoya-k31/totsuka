use orchestrator::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};

#[tokio::test]
async fn returns_target_version_against_migrated_db() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let v = check_schema_version(&pool).await.unwrap();
    assert_eq!(v, TARGET_SCHEMA_VERSION);
}
