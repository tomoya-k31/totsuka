//! The draft store behind the approval flow (#107): each `result/publish`
//! becomes a [`Draft`] the operator approves or rejects from Slack buttons.
//!
//! In-memory only — a restart loses pending drafts, which is acceptable
//! because the self-DM record keeps the draft text and a stale button press
//! degrades to an "expired" notice. Entries expire after [`DRAFT_TTL`]
//! (swept on the pipeline's hourly tick) and the store is bounded by
//! [`DRAFT_CAP`] with FIFO eviction, mirroring the pending-mention index.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long an unanswered draft stays actionable.
pub const DRAFT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Bound on stored drafts.
const DRAFT_CAP: usize = 1024;

/// Lifecycle of a draft. Only `Pending` drafts can be acted on; anything
/// else answers a button press with a "already handled" notice, which is the
/// double-send guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftStatus {
    /// Presented, awaiting the operator's decision.
    Pending,
    /// Approved and posted to the thread under the operator's name.
    Sent,
    /// Rejected; nothing was posted.
    Rejected,
}

/// One reply draft, keyed by the `draft_id` carried in its button values.
#[derive(Debug, Clone)]
pub struct Draft {
    /// The task this draft answers (`{channel}:{ts}`).
    pub task_id: String,
    /// Channel the mention was posted in (where the approved reply goes).
    pub channel: String,
    /// Thread the approved reply is posted into (`thread_ts ?? ts`).
    pub reply_ts: String,
    /// The mention message itself.
    pub mention_ts: String,
    /// Sender display name (for the draft header).
    pub sender_name: String,
    /// Permalink to the mention, when resolvable.
    pub permalink: Option<String>,
    /// The reply text (agent-generated, prefixed with a mechanical
    /// `<@sender_id>` mention of the asker), sent verbatim on approval.
    pub text: String,
    /// `ts` of the self-DM record, once posted (`chat.update` target).
    pub dm_ts: Option<String>,
    /// Where the draft is in its lifecycle.
    pub status: DraftStatus,
    /// Insertion time, for the TTL sweep.
    pub created_at: Instant,
}

/// The draft map plus its FIFO eviction order and id counter. Callers hold
/// it behind `SharedState`'s lock.
#[derive(Default)]
pub struct DraftStore {
    entries: HashMap<String, Draft>,
    order: VecDeque<String>,
    seq: u64,
}

impl DraftStore {
    /// Store `draft` and return its fresh id. Beyond [`DRAFT_CAP`] the
    /// oldest draft is evicted with a warning (its buttons degrade to the
    /// "expired" notice).
    pub fn insert(&mut self, draft: Draft) -> String {
        let draft_id = self.next_id();
        if self.order.len() >= DRAFT_CAP
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
            tracing::warn!(
                draft_id = %evicted,
                "draft store full; evicted the oldest draft (its buttons are now dead)"
            );
        }
        self.order.push_back(draft_id.clone());
        self.entries.insert(draft_id.clone(), draft);
        draft_id
    }

    /// A unique-per-run id that also never collides with pre-restart button
    /// values (wall-clock component), without pulling in a uuid dependency.
    fn next_id(&mut self) -> String {
        self.seq += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{nanos:x}-{}", self.seq)
    }

    /// The draft behind `draft_id`, if it is still stored.
    pub fn get(&self, draft_id: &str) -> Option<&Draft> {
        self.entries.get(draft_id)
    }

    /// Record the self-DM record's `ts` (the later `chat.update` target).
    pub fn set_dm_ts(&mut self, draft_id: &str, dm_ts: String) {
        if let Some(draft) = self.entries.get_mut(draft_id) {
            draft.dm_ts = Some(dm_ts);
        }
    }

    /// Move `draft_id` to `status`.
    pub fn set_status(&mut self, draft_id: &str, status: DraftStatus) {
        if let Some(draft) = self.entries.get_mut(draft_id) {
            draft.status = status;
        }
    }

    /// Drop drafts older than `ttl`, returning the dropped ids.
    pub fn sweep(&mut self, now: Instant, ttl: Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, draft)| now.duration_since(draft.created_at) >= ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.entries.remove(id);
            self.order.retain(|other| other != id);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(created_at: Instant) -> Draft {
        Draft {
            task_id: "C1:100.2".into(),
            channel: "C1".into(),
            reply_ts: "100.0".into(),
            mention_ts: "100.2".into(),
            sender_name: "alice".into(),
            permalink: None,
            text: "返信案".into(),
            dm_ts: None,
            status: DraftStatus::Pending,
            created_at,
        }
    }

    #[test]
    fn ids_are_unique_and_lookup_round_trips() {
        let mut store = DraftStore::default();
        let start = Instant::now();
        let a = store.insert(draft(start));
        let b = store.insert(draft(start));
        assert_ne!(a, b);
        assert_eq!(store.get(&a).unwrap().task_id, "C1:100.2");

        store.set_dm_ts(&a, "555.1".into());
        store.set_status(&a, DraftStatus::Sent);
        let updated = store.get(&a).unwrap();
        assert_eq!(updated.dm_ts.as_deref(), Some("555.1"));
        assert_eq!(updated.status, DraftStatus::Sent);
        // The other draft is untouched.
        assert_eq!(store.get(&b).unwrap().status, DraftStatus::Pending);
    }

    #[test]
    fn sweep_drops_only_expired_drafts() {
        let mut store = DraftStore::default();
        let start = Instant::now();
        let old = store.insert(draft(start));
        let fresh = store.insert(draft(start + Duration::from_secs(60 * 60)));

        let expired = store.sweep(start + DRAFT_TTL, DRAFT_TTL);
        assert_eq!(expired, vec![old.clone()]);
        assert!(store.get(&old).is_none(), "expired draft is gone");
        assert!(store.get(&fresh).is_some(), "fresh draft survives");
    }

    #[test]
    fn store_is_bounded() {
        let mut store = DraftStore::default();
        let start = Instant::now();
        let first = store.insert(draft(start));
        for _ in 0..DRAFT_CAP {
            store.insert(draft(start));
        }
        assert!(store.get(&first).is_none(), "oldest draft was evicted");
        assert_eq!(store.entries.len(), DRAFT_CAP);
        assert_eq!(store.entries.len(), store.order.len());
    }
}
