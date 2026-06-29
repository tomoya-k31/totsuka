use crate::error::TotsukactlError;
use sqlx::postgres::PgPoolOptions;

pub fn build_db_url(cfg: &totsuka_config::Config) -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            cfg.postgres.user,
            cfg.postgres.password.expose(),
            cfg.postgres.host,
            cfg.postgres.port,
            cfg.postgres.database,
        )
    })
}

pub async fn run(cfg: &totsuka_config::Config) -> Result<(), TotsukactlError> {
    let url = build_db_url(cfg);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .map_err(|e| TotsukactlError::Migrate(format!("connect: {e}")))?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .map_err(|e| TotsukactlError::Migrate(format!("{e}")))?;
    println!("migrations applied");
    Ok(())
}
