use qa_service::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> { std::env::var("DATABASE_URL").ok() }

#[tokio::test]
async fn returns_target_version_against_migrated_db() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    assert_eq!(check_schema_version(&pool).await.unwrap(), TARGET_SCHEMA_VERSION);
}
