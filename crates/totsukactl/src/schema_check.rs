//! spec §11.1 bin↔DB handshake. Reads the highest version from schema_meta
//! and validates it against the bin's compiled range.

use crate::error::TotsukactlError;
use sqlx::PgPool;

pub const MIN_SCHEMA_VERSION: i32 = 6;
pub const TARGET_SCHEMA_VERSION: i32 = 9;

pub async fn check_schema_version(pool: &PgPool) -> Result<i32, TotsukactlError> {
    let row: (Option<i32>,) = sqlx::query_as("SELECT max(version) FROM schema_meta")
        .fetch_one(pool)
        .await?;
    let got = row.0.ok_or_else(|| {
        TotsukactlError::Internal("schema_meta is empty; run totsukactl migrate".into())
    })?;
    if !(MIN_SCHEMA_VERSION..=TARGET_SCHEMA_VERSION).contains(&got) {
        return Err(TotsukactlError::SchemaOutOfRange {
            got,
            min: MIN_SCHEMA_VERSION,
            target: TARGET_SCHEMA_VERSION,
        });
    }
    Ok(got)
}
