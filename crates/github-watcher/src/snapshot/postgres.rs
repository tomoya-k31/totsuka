use super::{Diff, ItemSnapshot, SnapshotStore};
use crate::cursor::{set_in_tx, CursorKey};
use crate::error::WatcherError;
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use totsuka_bus::Publisher;
use totsuka_core::{ColumnId, DomainEvent};

pub struct PgSnapshotStore {
    pool: PgPool,
    publisher: Arc<Publisher>,
}

impl PgSnapshotStore {
    pub fn new(pool: PgPool, publisher: Arc<Publisher>) -> Self {
        Self { pool, publisher }
    }
}

fn parse_status(s: Option<String>) -> Option<ColumnId> {
    s.and_then(|raw| serde_json::from_value::<ColumnId>(serde_json::Value::String(raw)).ok())
}

#[async_trait]
impl SnapshotStore for PgSnapshotStore {
    async fn diff_page(&self, page: &[ItemSnapshot]) -> Result<Vec<Diff>, WatcherError> {
        if page.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<String> = page.iter().map(|i| i.item_id.clone()).collect();
        let rows = sqlx::query(
            "SELECT item_id, status FROM gh_item_status WHERE item_id = ANY($1)",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        let mut prev = std::collections::HashMap::<String, Option<ColumnId>>::new();
        for r in rows {
            let id: String = r.get("item_id");
            let s: Option<String> = r.get("status");
            prev.insert(id, parse_status(s));
        }
        let mut out = Vec::with_capacity(page.len());
        for snap in page {
            let prior = prev.get(&snap.item_id).cloned().unwrap_or(None);
            if prior != snap.status {
                let repo = snap
                    .content_ref
                    .as_ref()
                    .and_then(|s| s.split('#').next().map(String::from));
                out.push(Diff {
                    item_id: snap.item_id.clone(),
                    from_status: prior,
                    to_status: snap.status,
                    repo,
                });
            }
        }
        Ok(out)
    }

    async fn commit_page(
        &self,
        page: &[ItemSnapshot],
        events: &[(String, DomainEvent)],
        next_cursor: Option<&str>,
    ) -> Result<(), WatcherError> {
        let mut tx = self.pool.begin().await?;
        // 1. Publish every event in the same tx (spec §8.3 atomicity)
        for (_k, ev) in events {
            self.publisher
                .send_in_tx(&mut tx, ev.clone(), None)
                .await?;
        }
        // 2. UPSERT every snapshot row
        for snap in page {
            let status_snake = snap.status.map(|c| c.as_snake().to_string());
            sqlx::query(
                "INSERT INTO gh_item_status (item_id, status, content_ref, closed_at, updated_at)
                    VALUES ($1, $2, $3, $4, now())
                    ON CONFLICT (item_id) DO UPDATE
                      SET status      = EXCLUDED.status,
                          content_ref = EXCLUDED.content_ref,
                          closed_at   = EXCLUDED.closed_at,
                          updated_at  = now()",
            )
            .bind(&snap.item_id)
            .bind(status_snake)
            .bind(snap.content_ref.as_deref())
            .bind(snap.closed_at)
            .execute(&mut *tx)
            .await?;
        }
        // 3. Update the page cursor (if this is the last page, project loop will pass None)
        if let Some(c) = next_cursor {
            set_in_tx(&mut tx, &CursorKey::project_items(), c).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
