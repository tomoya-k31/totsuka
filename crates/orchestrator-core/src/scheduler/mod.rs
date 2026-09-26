//! Parallel execution control: slot management, priority queue, and the
//! slot-counting rule (F-40–F-45).
//!
//! Three concurrency tiers gate a dispatch (all must have a free slot):
//! global (F-40), per-repository (F-41), and per-agent-plugin (F-42). Every
//! state from `dispatched` until the task finishes occupies a slot (F-45) —
//! including `waiting_input` and `escalated`, which are blocked on a human.
//! The
//! counters are plain state (not tokio semaphores) so they can be **rebuilt
//! from the state DB** after a restart.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::domain::TaskId;
use crate::domain::state::TaskState;

/// Whether a task in `state` occupies a concurrency slot (F-45).
///
/// Every dispatched, unfinished state counts — **including the ones blocked on
/// a human** (`waiting_input`, `escalated`). Freeing the slot there let
/// the scheduler keep starting new tasks while the old ones sat half-done,
/// since most tasks stop for input sooner or later: the cap then bounded
/// nothing, and the operator ended up answering every task at once. Holding
/// the slot means a full set of waiting tasks stops new work until a human
/// answers (or cancels) one. There is no deadlock in that: a waiting task
/// needs a human, not a slot, and it resumes on the slot it already holds.
/// `pending` (repository confirmation) is before dispatch and holds none.
pub fn counts_toward_slot(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Dispatched
            | TaskState::Running
            | TaskState::WaitingInput
            | TaskState::Escalated
            | TaskState::Verifying
            | TaskState::Publishing
    )
}

/// Concurrency limits. `per_repo`/`per_agent` are caps only for the listed
/// names; absent entries are unlimited for that tier.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Global maximum concurrent tasks (F-40).
    pub global: u32,
    /// Per-repository caps (F-41).
    pub per_repo: HashMap<String, u32>,
    /// Per-agent-plugin caps (F-42).
    pub per_agent: HashMap<String, u32>,
}

impl Limits {
    /// A global-only limit (no per-repo/agent caps).
    pub fn global(global: u32) -> Self {
        Self {
            global,
            per_repo: HashMap::new(),
            per_agent: HashMap::new(),
        }
    }
}

/// Tracks how many slots are used across the three tiers, and **which task**
/// holds each one.
///
/// The per-task ledger (`holders`) is the authority for
/// [`SlotManager::release`] (#758): a slot is released by the task that holds
/// it, never by naming a `(repo, agent)` pair. That makes the three failure
/// shapes the pair-keyed API had to defend against unrepresentable — a task
/// that never acquired (an over-cap resume) releasing another task's slot, a
/// double release, and a cross-pair mismatch (holding `(a,x)` and `(b,y)`,
/// releasing `(a,y)`). The invariant `global_used == Σ repo_used ==
/// Σ agent_used == holders.len()` therefore holds by construction.
#[derive(Debug, Clone)]
pub struct SlotManager {
    limits: Limits,
    global_used: u32,
    repo_used: HashMap<String, u32>,
    agent_used: HashMap<String, u32>,
    /// Task id → the exact `(repo, agent)` pair it holds a slot under.
    holders: HashMap<TaskId, (String, String)>,
}

impl SlotManager {
    /// A manager with everything free.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            global_used: 0,
            repo_used: HashMap::new(),
            agent_used: HashMap::new(),
            holders: HashMap::new(),
        }
    }

    /// Rebuild slot usage and the holder ledger from the currently
    /// slot-occupying tasks (F-45), e.g. after a restart from the state DB.
    /// Pass `(task_id, repo, agent)` for each task whose state
    /// [`counts_toward_slot`].
    ///
    /// Usage and holders come from the **same** list, so the counts and the
    /// ledger cannot disagree after a restart (#758 — they used to be built
    /// from two separate passes). Caps are not consulted: every claim is a
    /// task that is already running.
    pub fn rebuild<I>(&mut self, active: I)
    where
        I: IntoIterator<Item = (TaskId, String, String)>,
    {
        self.global_used = 0;
        self.repo_used.clear();
        self.agent_used.clear();
        self.holders.clear();
        for (task_id, repo, agent) in active {
            self.take(task_id, repo, agent);
        }
    }

    /// Whether a dispatch to `(repo, agent)` fits all three tiers.
    pub fn can_dispatch(&self, repo: &str, agent: &str) -> bool {
        let global_ok = self.global_used < self.limits.global;
        let repo_ok = self
            .limits
            .per_repo
            .get(repo)
            .is_none_or(|&limit| self.repo_used.get(repo).copied().unwrap_or(0) < limit);
        let agent_ok = self
            .limits
            .per_agent
            .get(agent)
            .is_none_or(|&limit| self.agent_used.get(agent).copied().unwrap_or(0) < limit);
        global_ok && repo_ok && agent_ok
    }

    /// Acquire a slot for `task_id` under `(repo, agent)` if all tiers allow
    /// it, recording the task as its holder. A task that already holds a slot
    /// keeps the one it has and is answered `true` — one task, at most one
    /// slot.
    pub fn acquire(&mut self, task_id: TaskId, repo: &str, agent: &str) -> bool {
        if self.holds(task_id) {
            return true;
        }
        if !self.can_dispatch(repo, agent) {
            return false;
        }
        self.take(task_id, repo.to_string(), agent.to_string());
        true
    }

    /// Release the slot `task_id` holds (on `waiting_input`, `cancelled`, or
    /// completion). A task that holds none — never acquired, or already
    /// released — is a safe no-op, so no release can free a slot another task
    /// holds.
    pub fn release(&mut self, task_id: TaskId) {
        let Some((repo, agent)) = self.holders.remove(&task_id) else {
            return;
        };
        self.global_used = self.global_used.saturating_sub(1);
        decrement(&mut self.repo_used, &repo);
        decrement(&mut self.agent_used, &agent);
    }

    /// Whether `task_id` currently holds a slot.
    pub fn holds(&self, task_id: TaskId) -> bool {
        self.holders.contains_key(&task_id)
    }

    /// The tasks currently holding a slot, in no particular order.
    pub fn holders(&self) -> impl Iterator<Item = TaskId> + '_ {
        self.holders.keys().copied()
    }

    /// Total slots in use.
    pub fn global_used(&self) -> u32 {
        self.global_used
    }

    /// Slots in use for a repository.
    pub fn repo_used(&self, repo: &str) -> u32 {
        self.repo_used.get(repo).copied().unwrap_or(0)
    }

    /// Slots in use for an agent plugin.
    pub fn agent_used(&self, agent: &str) -> u32 {
        self.agent_used.get(agent).copied().unwrap_or(0)
    }

    /// Count a slot for `task_id` across every tier and record the holder.
    fn take(&mut self, task_id: TaskId, repo: String, agent: String) {
        self.global_used += 1;
        *self.repo_used.entry(repo.clone()).or_insert(0) += 1;
        *self.agent_used.entry(agent.clone()).or_insert(0) += 1;
        self.holders.insert(task_id, (repo, agent));
    }
}

fn decrement(map: &mut HashMap<String, u32>, key: &str) {
    if let Some(count) = map.get_mut(key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(key);
        }
    }
}

/// A task ready to dispatch (repo/agent resolved), for [`plan_dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTask {
    /// Task id.
    pub task_id: TaskId,
    /// Selected repository.
    pub repo: String,
    /// Agent plugin.
    pub agent: String,
    /// Priority (higher first).
    pub priority: i64,
}

/// Greedily dispatch ready tasks in priority order (higher first, FIFO on
/// ties), acquiring slots as available (F-43). Returns the task ids to
/// dispatch; each one is recorded in `slots` as a holder, so the caller has
/// nothing to mirror. A task blocked by a full tier is skipped so it does not
/// head-of-line-block a different repo/agent.
pub fn plan_dispatch(slots: &mut SlotManager, ready: &[ReadyTask]) -> Vec<TaskId> {
    let mut order: Vec<&ReadyTask> = ready.iter().collect();
    // Stable sort keeps input order (FIFO) among equal priorities.
    order.sort_by_key(|t| std::cmp::Reverse(t.priority));

    let mut dispatched = Vec::new();
    for task in order {
        if slots.acquire(task.task_id, &task.repo, &task.agent) {
            dispatched.push(task.task_id);
        }
    }
    dispatched
}

/// A priority queue of queued task ids: highest priority first, FIFO on ties
/// (F-43). Backed by a binary heap with an insertion sequence number.
#[derive(Debug, Default)]
pub struct PriorityQueue {
    heap: BinaryHeap<QueueEntry>,
    next_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueEntry {
    priority: i64,
    seq: u64,
    task_id: TaskId,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first; then earlier insertion (smaller seq) first.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PriorityQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a task.
    pub fn push(&mut self, task_id: TaskId, priority: i64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.heap.push(QueueEntry {
            priority,
            seq,
            task_id,
        });
    }

    /// Dequeue the highest-priority (then earliest) task id.
    pub fn pop(&mut self) -> Option<TaskId> {
        self.heap.pop().map(|e| e.task_id)
    }

    /// Number of queued tasks.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            global: 3,
            per_repo: HashMap::from([("repoA".to_string(), 2)]),
            per_agent: HashMap::from([("herdr".to_string(), 1)]),
        }
    }

    #[test]
    fn counts_only_active_states() {
        // Waiting on a human still holds the slot.
        for s in [
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Escalated,
            TaskState::Verifying,
            TaskState::Publishing,
        ] {
            assert!(counts_toward_slot(s), "{s} should count");
        }
        for s in [
            TaskState::Queued,
            TaskState::Pending,
            TaskState::Done,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            assert!(!counts_toward_slot(s), "{s} should not count");
        }
    }

    #[test]
    fn all_three_tiers_gate_dispatch() {
        let mut slots = SlotManager::new(limits());
        // per_agent herdr cap = 1.
        assert!(slots.acquire(TaskId(1), "repoA", "herdr"));
        assert!(!slots.can_dispatch("repoB", "herdr"), "agent cap reached");
        // A different agent is fine (repoA cap = 2, global 3).
        assert!(slots.acquire(TaskId(2), "repoA", "orca"));
        assert!(!slots.can_dispatch("repoA", "orca"), "repoA cap reached");
        // global cap = 3.
        assert!(slots.acquire(TaskId(3), "repoB", "orca"));
        assert!(!slots.can_dispatch("repoC", "orca"), "global cap reached");
        assert_eq!(slots.global_used(), 3);
    }

    #[test]
    fn release_frees_a_slot_for_another_task() {
        let mut slots = SlotManager::new(Limits::global(1));
        assert!(slots.acquire(TaskId(1), "r", "a"));
        assert!(!slots.can_dispatch("r", "a"), "global full");
        // Simulate waiting_input releasing the slot (F-45).
        slots.release(TaskId(1));
        assert!(slots.can_dispatch("r", "a"), "slot freed");
        assert!(slots.acquire(TaskId(2), "r", "a"));
    }

    #[test]
    fn waiting_resume_round_trip_preserves_counts() {
        let mut slots = SlotManager::new(limits());
        slots.acquire(TaskId(1), "repoA", "herdr");
        let before = (
            slots.global_used(),
            slots.repo_used("repoA"),
            slots.agent_used("herdr"),
        );
        // waiting_input -> release, resume -> re-acquire.
        slots.release(TaskId(1));
        assert_eq!(slots.global_used(), before.0 - 1);
        assert!(slots.acquire(TaskId(1), "repoA", "herdr"));
        assert_eq!(
            (
                slots.global_used(),
                slots.repo_used("repoA"),
                slots.agent_used("herdr")
            ),
            before,
            "no leak or double-count across the round trip"
        );
    }

    #[test]
    fn stray_release_does_not_corrupt_the_invariant() {
        let mut slots = SlotManager::new(limits());
        slots.acquire(TaskId(1), "repoA", "herdr");
        // Double release: the second is a no-op (slot no longer held).
        slots.release(TaskId(1));
        slots.release(TaskId(1));
        assert_eq!(slots.global_used(), 0);
        assert_eq!(slots.repo_used("repoA"), 0);
        assert_eq!(slots.agent_used("herdr"), 0);

        // A release by a task that never acquired must not drop the count and
        // leave the real slot leaked.
        slots.acquire(TaskId(1), "repoA", "herdr");
        slots.release(TaskId(2)); // task 2 never held -> no-op
        assert_eq!(
            slots.global_used(),
            1,
            "global must stay in sync with tiers"
        );
        assert_eq!(slots.repo_used("repoA"), 1);
        assert_eq!(slots.agent_used("herdr"), 1);
        assert!(slots.holds(TaskId(1)));
    }

    #[test]
    fn cross_pair_release_is_a_no_op() {
        // Holding (repoA, herdr) and (repoB, orca): a release can only name a
        // task, so it frees exactly the pair that task holds — the per-tier
        // breakdown of the other one is untouched.
        let mut slots = SlotManager::new(Limits::global(4));
        slots.acquire(TaskId(1), "repoA", "herdr");
        slots.acquire(TaskId(2), "repoB", "orca");
        slots.release(TaskId(3)); // no such holder -> no-op
        assert_eq!(
            slots.global_used(),
            2,
            "a stranger's release must be a no-op"
        );
        slots.release(TaskId(1));
        assert_eq!(slots.repo_used("repoA"), 0);
        assert_eq!(slots.agent_used("herdr"), 0);
        assert_eq!(slots.repo_used("repoB"), 1);
        assert_eq!(slots.agent_used("orca"), 1);
        slots.release(TaskId(2));
        assert_eq!(slots.global_used(), 0);
    }

    #[test]
    fn rebuild_reconstructs_from_active_tasks() {
        let mut slots = SlotManager::new(limits());
        slots.rebuild([
            (TaskId(1), "repoA".to_string(), "herdr".to_string()),
            (TaskId(2), "repoA".to_string(), "orca".to_string()),
            (TaskId(3), "repoB".to_string(), "orca".to_string()),
        ]);
        assert_eq!(slots.global_used(), 3);
        assert_eq!(slots.repo_used("repoA"), 2);
        assert_eq!(slots.agent_used("orca"), 2);
        // Caps now correctly report full.
        assert!(
            !slots.can_dispatch("repoC", "orca"),
            "global full after rebuild"
        );
    }

    /// #758: usage and holders come from one list, so every rebuilt slot is
    /// releasable by its task and nothing else is.
    #[test]
    fn rebuild_records_holders_alongside_usage() {
        let mut slots = SlotManager::new(Limits::global(4));
        slots.acquire(TaskId(9), "stale", "x"); // wiped by the rebuild
        slots.rebuild([
            (TaskId(1), "repoA".to_string(), "herdr".to_string()),
            (TaskId(2), "repoB".to_string(), "orca".to_string()),
        ]);
        assert!(slots.holds(TaskId(1)) && slots.holds(TaskId(2)));
        assert!(
            !slots.holds(TaskId(9)),
            "rebuild replaces the ledger, not merges"
        );
        slots.release(TaskId(1));
        slots.release(TaskId(2));
        assert_eq!(slots.global_used(), 0);
        assert_eq!(slots.repo_used("repoA") + slots.repo_used("repoB"), 0);
    }

    /// An over-cap resume leaves the task without a slot; its later release
    /// must not free a slot some other task holds.
    #[test]
    fn a_task_that_failed_to_acquire_releases_nothing() {
        let mut slots = SlotManager::new(Limits::global(1));
        assert!(slots.acquire(TaskId(1), "r", "a"));
        assert!(!slots.acquire(TaskId(2), "r", "a"), "global full");
        assert!(!slots.holds(TaskId(2)));
        slots.release(TaskId(2));
        assert_eq!(slots.global_used(), 1);
        assert!(slots.holds(TaskId(1)));
    }

    /// One task, at most one slot: acquiring again keeps the slot it has.
    #[test]
    fn acquiring_twice_counts_once() {
        let mut slots = SlotManager::new(Limits::global(2));
        assert!(slots.acquire(TaskId(1), "r", "a"));
        assert!(slots.acquire(TaskId(1), "r", "a"));
        assert_eq!(slots.global_used(), 1);
        slots.release(TaskId(1));
        assert_eq!(slots.global_used(), 0);
    }

    #[test]
    fn plan_dispatch_respects_priority_fifo_and_slots() {
        let mut slots = SlotManager::new(Limits::global(2));
        let ready = vec![
            ReadyTask {
                task_id: TaskId(1),
                repo: "r".into(),
                agent: "a".into(),
                priority: 0,
            },
            ReadyTask {
                task_id: TaskId(2),
                repo: "r".into(),
                agent: "a".into(),
                priority: 5,
            },
            ReadyTask {
                task_id: TaskId(3),
                repo: "r".into(),
                agent: "a".into(),
                priority: 5,
            },
        ];
        // global=2: highest priority (2) then FIFO tie (3 after 2); id1 blocked.
        let dispatched = plan_dispatch(&mut slots, &ready);
        assert_eq!(dispatched, vec![TaskId(2), TaskId(3)]);
        assert_eq!(slots.global_used(), 2);
        // The plan records its picks as holders; the blocked task holds none.
        assert!(slots.holds(TaskId(2)) && slots.holds(TaskId(3)));
        assert!(!slots.holds(TaskId(1)));
    }

    #[test]
    fn priority_queue_orders_by_priority_then_fifo() {
        let mut q = PriorityQueue::new();
        q.push(TaskId(1), 0);
        q.push(TaskId(2), 5);
        q.push(TaskId(3), 5);
        q.push(TaskId(4), 10);
        assert_eq!(q.len(), 4);
        assert_eq!(q.pop(), Some(TaskId(4))); // highest priority
        assert_eq!(q.pop(), Some(TaskId(2))); // priority 5, earlier
        assert_eq!(q.pop(), Some(TaskId(3))); // priority 5, later
        assert_eq!(q.pop(), Some(TaskId(1))); // lowest
        assert!(q.is_empty());
    }
}
