use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use totsuka_core::Clock;

use crate::error::OrchestratorError;

pub struct EffectLedger {
    pool: PgPool,
    clock: Arc<dyn Clock>,
    lease_secs: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Skipped { reason: String },
}

impl EffectLedger {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>, lease_secs: i64) -> Self {
        Self {
            pool,
            clock,
            lease_secs,
        }
    }

    pub async fn claim(
        &self,
        key: &str,
        event_key: &str,
        ty: &str,
        owner: &str,
    ) -> Result<ClaimOutcome, OrchestratorError> {
        // The processed_effects PK is (effect_key, created_at) because the
        // table is PARTITIONED BY RANGE (created_at). That PK does NOT prevent
        // duplicate `effect_key` rows across different `created_at` values, so
        // a naive INSERT ... ON CONFLICT (effect_key, created_at) DO NOTHING
        // would silently allow concurrent double-claims. Serialize per-key
        // with a pg_advisory_xact_lock keyed on the effect_key hash, then do
        // a normal SELECT + (INSERT-if-missing | UPDATE-if-expired | SKIP)
        // inside the same transaction.
        let now = self.clock.now();
        let expires = now + chrono::Duration::seconds(self.lease_secs);
        let mut tx = self.pool.begin().await?;

        // Lock other claims for this effect_key until commit.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(key)
            .execute(&mut *tx)
            .await?;

        let existing: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT status, lease_expires_at FROM processed_effects
             WHERE effect_key = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;

        let outcome = match existing {
            None => {
                sqlx::query(
                    "INSERT INTO processed_effects
                        (effect_key, event_key, effect_type, status, lease_owner,
                         lease_expires_at, attempts, created_at)
                     VALUES ($1, $2, $3, 'in_progress', $4, $5, 1, $6)",
                )
                .bind(key)
                .bind(event_key)
                .bind(ty)
                .bind(owner)
                .bind(expires)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                ClaimOutcome::Claimed
            }
            Some((ref s, _)) if s == "done" => ClaimOutcome::Skipped {
                reason: "already done".into(),
            },
            Some((ref s, exp)) if s == "in_progress" => {
                if let Some(e) = exp {
                    if e > now {
                        tx.commit().await?;
                        return Ok(ClaimOutcome::Skipped {
                            reason: format!("leased until {e}"),
                        });
                    }
                }
                // Expired — take over the most recent row.
                let upd = sqlx::query(
                    "UPDATE processed_effects SET lease_owner = $2, lease_expires_at = $3,
                     attempts = attempts + 1, updated_at = $4
                     WHERE effect_key = $1 AND created_at = (
                         SELECT max(created_at) FROM processed_effects WHERE effect_key = $1
                     )",
                )
                .bind(key)
                .bind(owner)
                .bind(expires)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                if upd.rows_affected() == 1 {
                    ClaimOutcome::Claimed
                } else {
                    ClaimOutcome::Skipped {
                        reason: "race lost".into(),
                    }
                }
            }
            Some((s, _)) => ClaimOutcome::Skipped {
                reason: format!("status={s}"),
            },
        };
        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn complete(&self, key: &str, result: Value) -> Result<(), OrchestratorError> {
        sqlx::query(
            "UPDATE processed_effects SET status='done', result=$2, lease_owner=NULL,
             lease_expires_at=NULL, updated_at=$3 WHERE effect_key=$1",
        )
        .bind(key)
        .bind(result)
        .bind(self.clock.now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail(&self, key: &str, err: &str) -> Result<(), OrchestratorError> {
        sqlx::query(
            "UPDATE processed_effects SET status='failed', result=$2, lease_owner=NULL,
             lease_expires_at=NULL, updated_at=$3 WHERE effect_key=$1",
        )
        .bind(key)
        .bind(serde_json::json!({"error": err}))
        .bind(self.clock.now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn result_for(
        &self,
        effect_key: &str,
    ) -> Result<Option<serde_json::Value>, OrchestratorError> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT result FROM processed_effects WHERE effect_key = $1 AND status = 'done'
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(effect_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Latest completed result whose key starts with `prefix`. Spawn keys
    /// carry a `:g<seq>` generation suffix (column re-entry), so lookups
    /// that only know task+phase+attempt match by prefix.
    pub async fn latest_result_with_prefix(
        &self,
        prefix: &str,
    ) -> Result<Option<serde_json::Value>, OrchestratorError> {
        // Escape LIKE metacharacters in the key prefix, then anchor it.
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT result FROM processed_effects WHERE effect_key LIKE $1 AND status = 'done'
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(pattern)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }
}
