//! The draft store behind the approval flow (#107): each `result/publish`
//! becomes a [`Draft`] the operator approves or rejects from Slack buttons.
//!
//! Persisted to `drafts.json` since #122: every mutation mirrors the whole
//! store to disk (atomic write, 0600, KB-order data), and `initialize` loads
//! it back, so approval buttons survive a `run --watch` restart. `Sent` /
//! `Rejected` drafts are persisted too — they carry the double-send guard
//! ("already handled") across restarts. Persistence failures degrade to the
//! pre-#122 in-memory behavior (warn + continue), never stop the flow.
//! Entries expire after [`DRAFT_TTL`] (swept on the pipeline's hourly tick,
//! pruned again at load) and the store is bounded by `DRAFT_CAP` with FIFO
//! eviction, mirroring the pending-mention index.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long an unanswered draft stays actionable.
pub const DRAFT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Bound on stored drafts.
const DRAFT_CAP: usize = 1024;

/// On-disk schema version of `drafts.json`. An unknown version is treated
/// like corruption — start empty, never fail startup.
const STORE_VERSION: u32 = 1;

/// Lifecycle of a draft. Only `Pending` drafts can be acted on; anything
/// else answers a button press with a "already handled" notice, which is the
/// double-send guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    /// Presented, awaiting the operator's decision.
    Pending,
    /// Approved and posted to the thread under the operator's name.
    Sent,
    /// Rejected; nothing was posted.
    Rejected,
}

/// One reply draft, keyed by the `draft_id` carried in its button values.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Insertion time, for the TTL sweep. Wall-clock (`SystemTime`, not
    /// `Instant`) so expiry survives a process restart (#122).
    pub created_at: SystemTime,
}

/// The on-disk shape of the store: schema version, the id counter, and the
/// drafts in FIFO (eviction) order, oldest first.
#[derive(Serialize, Deserialize)]
struct PersistedStore {
    version: u32,
    seq: u64,
    drafts: Vec<PersistedDraft>,
}

/// One persisted entry: the draft plus its id (the map key).
#[derive(Serialize, Deserialize)]
struct PersistedDraft {
    id: String,
    #[serde(flatten)]
    draft: Draft,
}

/// The draft map plus its FIFO eviction order and id counter. Callers hold
/// it behind `SharedState`'s lock. With a `path`, every mutation mirrors the
/// store to disk (#122); without one it is in-memory only (tests, or an
/// environment where no state directory could be resolved).
#[derive(Default)]
pub struct DraftStore {
    entries: HashMap<String, Draft>,
    order: VecDeque<String>,
    seq: u64,
    path: Option<PathBuf>,
}

impl DraftStore {
    /// Load the store persisted at `path` (#122). A missing file is a normal
    /// first run; a corrupt, unreadable, or unknown-version file starts an
    /// empty store (warn) rather than failing startup. Entries past
    /// [`DRAFT_TTL`] are pruned on load; `Sent` / `Rejected` survive so the
    /// double-send guard holds across restarts.
    pub fn load(path: PathBuf) -> Self {
        Self::load_at(path, SystemTime::now())
    }

    /// [`Self::load`] with an injected `now`, the unit-test seam.
    fn load_at(path: PathBuf, now: SystemTime) -> Self {
        let mut store = Self {
            path: Some(path),
            ..Self::default()
        };
        let path = store.path.as_ref().unwrap();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "could not read the persisted draft store; starting empty");
                return store;
            }
        };
        let persisted: PersistedStore = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "persisted draft store is corrupt; starting empty");
                return store;
            }
        };
        if persisted.version != STORE_VERSION {
            tracing::warn!(
                path = %path.display(),
                version = persisted.version,
                expected = STORE_VERSION,
                "persisted draft store has an unknown schema version; starting empty"
            );
            return store;
        }

        store.seq = persisted.seq;
        let total = persisted.drafts.len();
        for entry in persisted.drafts {
            if expired(now, entry.draft.created_at, DRAFT_TTL) {
                continue;
            }
            store.order.push_back(entry.id.clone());
            store.entries.insert(entry.id, entry.draft);
        }
        // A tampered/oversized file must not bypass the memory bound.
        while store.order.len() > DRAFT_CAP {
            if let Some(evicted) = store.order.pop_front() {
                store.entries.remove(&evicted);
            }
        }
        tracing::info!(
            path = %path.display(),
            loaded = store.order.len(),
            pruned = total - store.order.len(),
            "draft store loaded; pre-restart approval buttons stay live"
        );
        store
    }

    /// Store `draft` and return its fresh id. Beyond `DRAFT_CAP` the
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
        self.save();
        draft_id
    }

    /// A unique-per-run id that also never collides with pre-restart ids
    /// (wall-clock component), without pulling in a uuid dependency. The
    /// persisted `seq` (#122) keeps the counter monotonic across restarts.
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
            self.save();
        }
    }

    /// Move `draft_id` to `status`.
    pub fn set_status(&mut self, draft_id: &str, status: DraftStatus) {
        if let Some(draft) = self.entries.get_mut(draft_id) {
            draft.status = status;
            self.save();
        }
    }

    /// Drop drafts older than `ttl`, returning the dropped ids.
    pub fn sweep(&mut self, now: SystemTime, ttl: Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, draft)| expired(now, draft.created_at, ttl))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.entries.remove(id);
            self.order.retain(|other| other != id);
        }
        if !expired.is_empty() {
            self.save();
        }
        expired
    }

    /// Mirror the store to its file, when persistence is on. Failures are
    /// logged and tolerated: the in-memory store stays authoritative and the
    /// approval flow keeps working, degraded to the pre-#122 (restart-
    /// losable) behavior.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let persisted = PersistedStore {
            version: STORE_VERSION,
            seq: self.seq,
            drafts: self
                .order
                .iter()
                .filter_map(|id| {
                    self.entries.get(id).map(|draft| PersistedDraft {
                        id: id.clone(),
                        draft: draft.clone(),
                    })
                })
                .collect(),
        };
        let bytes = match serde_json::to_vec(&persisted) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, "could not serialize the draft store; not persisted");
                return;
            }
        };
        if let Err(e) = crate::persist::atomic_write(path, &bytes) {
            tracing::warn!(path = %path.display(), error = %e,
                "could not persist the draft store; continuing in-memory");
        }
    }
}

/// Whether `created_at` is at least `ttl` old at `now`. A clock that moved
/// backwards (`duration_since` error) reads as "not expired" — the sweep
/// catches the entry once the clock recovers.
fn expired(now: SystemTime, created_at: SystemTime, ttl: Duration) -> bool {
    now.duration_since(created_at).is_ok_and(|age| age >= ttl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(created_at: SystemTime) -> Draft {
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

    /// A fresh scratch file path unique to this test.
    fn scratch_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "totsuka-slack-draft-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("drafts.json")
    }

    #[test]
    fn ids_are_unique_and_lookup_round_trips() {
        let mut store = DraftStore::default();
        let start = SystemTime::now();
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
        let start = SystemTime::now();
        let old = store.insert(draft(start));
        let fresh = store.insert(draft(start + Duration::from_secs(60 * 60)));

        let expired = store.sweep(start + DRAFT_TTL, DRAFT_TTL);
        assert_eq!(expired, vec![old.clone()]);
        assert!(store.get(&old).is_none(), "expired draft is gone");
        assert!(store.get(&fresh).is_some(), "fresh draft survives");
    }

    #[test]
    fn a_backwards_clock_never_expires_drafts() {
        let mut store = DraftStore::default();
        let start = SystemTime::now();
        store.insert(draft(start));
        let expired = store.sweep(start - Duration::from_secs(60), DRAFT_TTL);
        assert!(expired.is_empty(), "rollback must not expire anything");
    }

    #[test]
    fn store_is_bounded() {
        let mut store = DraftStore::default();
        let start = SystemTime::now();
        let first = store.insert(draft(start));
        for _ in 0..DRAFT_CAP {
            store.insert(draft(start));
        }
        assert!(store.get(&first).is_none(), "oldest draft was evicted");
        assert_eq!(store.entries.len(), DRAFT_CAP);
        assert_eq!(store.entries.len(), store.order.len());
    }

    #[test]
    fn persisted_store_round_trips_across_a_reload() {
        let path = scratch_path("round-trip");
        let start = SystemTime::now();

        let mut store = DraftStore::load(path.clone());
        let a = store.insert(draft(start));
        let b = store.insert(draft(start));
        store.set_dm_ts(&a, "555.1".into());
        store.set_status(&a, DraftStatus::Sent);

        // A "restarted" store sees the same drafts, statuses, and dm_ts —
        // including the non-Pending one (the double-send guard's memory).
        let reloaded = DraftStore::load(path);
        let restored = reloaded.get(&a).expect("draft a survives the reload");
        assert_eq!(restored.status, DraftStatus::Sent);
        assert_eq!(restored.dm_ts.as_deref(), Some("555.1"));
        assert_eq!(reloaded.get(&b).unwrap().status, DraftStatus::Pending);
        assert_eq!(reloaded.order.len(), 2);
    }

    #[test]
    fn reload_continues_the_id_sequence() {
        let path = scratch_path("seq");
        let mut store = DraftStore::load(path.clone());
        store.insert(draft(SystemTime::now()));
        assert_eq!(store.seq, 1);

        let mut reloaded = DraftStore::load(path);
        assert_eq!(reloaded.seq, 1, "seq is persisted");
        reloaded.insert(draft(SystemTime::now()));
        assert_eq!(reloaded.seq, 2, "the counter keeps counting");
    }

    #[test]
    fn load_prunes_expired_entries() {
        let path = scratch_path("prune");
        let start = SystemTime::now();
        let mut store = DraftStore::load(path.clone());
        let old = store.insert(draft(start - DRAFT_TTL));
        let fresh = store.insert(draft(start));

        let reloaded = DraftStore::load_at(path, start);
        assert!(reloaded.get(&old).is_none(), "expired entry pruned on load");
        assert!(reloaded.get(&fresh).is_some(), "fresh entry survives");
        assert_eq!(reloaded.order.len(), 1);
    }

    #[test]
    fn missing_corrupt_or_future_files_start_empty() {
        // Missing file: a normal first run.
        let path = scratch_path("missing");
        let store = DraftStore::load(path);
        assert!(store.entries.is_empty());

        // Corrupt JSON (e.g. a torn write from a pre-atomic era).
        let path = scratch_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        let store = DraftStore::load(path.clone());
        assert!(store.entries.is_empty(), "corrupt file starts empty");
        // The store still persists: the next mutation overwrites the debris.
        let mut store = store;
        let id = store.insert(draft(SystemTime::now()));
        assert!(DraftStore::load(path).get(&id).is_some());

        // Unknown schema version: same posture as corruption.
        let path = scratch_path("future-version");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{ "version": 999, "seq": 5, "drafts": [] }"#).unwrap();
        let store = DraftStore::load(path);
        assert!(store.entries.is_empty());
        assert_eq!(store.seq, 0, "an unknown version adopts nothing");
    }

    #[test]
    fn in_memory_store_never_touches_disk() {
        // `DraftStore::default()` (no path) is the pre-#122 behavior, used
        // when no state directory can be resolved.
        let mut store = DraftStore::default();
        store.insert(draft(SystemTime::now()));
        assert!(store.path.is_none());
    }
}
