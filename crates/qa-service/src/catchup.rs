//! Slack startup catchup. Reads recent history per channel, advances the
//! per-channel cursor in catchup_cursor (source='slack'), and logs counts so
//! operators can see how much was missed. We deliberately do NOT replay
//! messages into the answer pipeline — that would double-answer questions
//! that were already handled before restart.

use crate::error::QaError;
use crate::slack::SlackClient;
use sqlx::PgPool;

pub async fn run_catchup_once(
    slack: &dyn SlackClient,
    pool: &PgPool,
    channels: &[String],
    default_oldest: Option<String>,
) -> Result<usize, QaError> {
    let mut total = 0usize;
    for channel in channels {
        let scope = format!("channel:{channel}");
        let cursor = get_cursor(pool, &scope).await?;
        let oldest = cursor.or_else(|| default_oldest.clone());
        let msgs = slack.conversation_history(channel, oldest.as_deref(), 100).await?;
        if let Some(max_ts) = msgs.iter().map(|m| m.ts.clone()).max() {
            set_cursor(pool, &scope, &max_ts).await?;
        }
        tracing::info!(channel, observed = msgs.len(), "slack catchup");
        total += msgs.len();
    }
    Ok(total)
}

async fn get_cursor(pool: &PgPool, scope: &str) -> Result<Option<String>, QaError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT cursor FROM catchup_cursor WHERE source = 'slack' AND scope = $1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

async fn set_cursor(pool: &PgPool, scope: &str, cursor: &str) -> Result<(), QaError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
              VALUES ('slack', $1, $2, now())
              ON CONFLICT (source, scope) DO UPDATE
                SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(scope)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}
