//! spec §8.3: ProjectsV2 snapshot diff. The repository owns the same-tx
//! publish path: every diff row's bus event + every UPSERT + the page's
//! end-cursor update are committed in one transaction (via
//! Publisher::send_in_tx). This is the atomicity guarantee the orchestrator
//! relies on — if any step fails, the next poll re-derives the same diff and
//! the deterministic event_key makes the orchestrator absorb the duplicate.

use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use totsuka_core::{ColumnId, DomainEvent};

pub mod postgres;
pub use postgres::PgSnapshotStore;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemSnapshot {
    pub item_id: String,
    pub status: Option<ColumnId>,
    pub content_ref: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub item_id: String,
    pub from_status: Option<ColumnId>,
    pub to_status: Option<ColumnId>,
    pub repo: Option<String>,
}

#[async_trait]
pub trait SnapshotStore: Send + Sync + 'static {
    async fn diff_page(&self, page: &[ItemSnapshot]) -> Result<Vec<Diff>, WatcherError>;

    async fn commit_page(
        &self,
        page: &[ItemSnapshot],
        events: &[(String /* event_key, currently unused but kept for trace */, DomainEvent)],
        next_cursor: Option<&str>,
    ) -> Result<(), WatcherError>;
}
