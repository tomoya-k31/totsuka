//! spec §11.2 + §8.3: catchup_cursor holds per-stream resume points.
//! Updated atomically with snapshot UPSERT + bus publish (use set_in_tx).

use crate::error::WatcherError;
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug, Clone)]
pub struct CursorKey {
    pub source: &'static str,
    pub scope: String,
}

impl CursorKey {
    pub fn project_items() -> Self {
        Self { source: "github", scope: "projectv2_items".into() }
    }
    pub fn issues(repo: &str) -> Self {
        Self { source: "github", scope: format!("issues:{repo}") }
    }
    pub fn prs(repo: &str) -> Self {
        Self { source: "github", scope: format!("prs:{repo}") }
    }
    pub fn releases(repo: &str) -> Self {
        Self { source: "github", scope: format!("releases:{repo}") }
    }
}

pub async fn get(pool: &PgPool, key: &CursorKey) -> Result<Option<String>, WatcherError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT cursor FROM catchup_cursor WHERE source = $1 AND scope = $2",
    )
    .bind(key.source)
    .bind(&key.scope)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn set(pool: &PgPool, key: &CursorKey, cursor: &str) -> Result<(), WatcherError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
            VALUES ($1, $2, $3, now())
            ON CONFLICT (source, scope) DO UPDATE
              SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(key.source)
    .bind(&key.scope)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    key: &CursorKey,
    cursor: &str,
) -> Result<(), WatcherError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
            VALUES ($1, $2, $3, now())
            ON CONFLICT (source, scope) DO UPDATE
              SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(key.source)
    .bind(&key.scope)
    .bind(cursor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
