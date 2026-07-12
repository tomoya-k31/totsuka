//! Parallel execution control: slot management, priority queue, and the
//! slot-counting rule (F-40–F-45).
//!
//! Three concurrency tiers gate a dispatch (all must have a free slot):
//! global (F-40), per-repository (F-41), and per-agent-plugin (F-42). Only the
//! `dispatched → running → publishing` states occupy a slot (F-45); entering
//! `waiting_input`/`pending` releases it and resuming re-acquires it. The
//! counters are plain state (not tokio semaphores) so they can be **rebuilt
//! from the state DB** after a restart.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::domain::state::TaskState;

/// Whether a task in `state` occupies a concurrency slot (F-45).
///
/// Only actively-executing states count; `waiting_input`/`pending` free the
/// slot to prevent wait-induced deadlock.
pub fn counts_toward_slot(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Dispatched | TaskState::Running | TaskState::Publishing
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

/// Tracks how many slots are used across the three tiers.
#[derive(Debug, Clone)]
pub struct SlotManager {
    limits: Limits,
    global_used: u32,
    repo_used: HashMap<String, u32>,
    agent_used: HashMap<String, u32>,
}

impl SlotManager {
    /// A manager with everything free.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            global_used: 0,
            repo_used: HashMap::new(),
            agent_used: HashMap::new(),
        }
    }

    /// Rebuild slot usage from the currently slot-occupying tasks (F-45), e.g.
    /// after a restart from the state DB. Pass `(repo, agent)` for each task
    /// whose state [`counts_toward_slot`].
    pub fn rebuild<I>(&mut self, active: I)
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.global_used = 0;
        self.repo_used.clear();
        self.agent_used.clear();
        for (repo, agent) in active {
            self.global_used += 1;
            *self.repo_used.entry(repo).or_insert(0) += 1;
            *self.agent_used.entry(agent).or_insert(0) += 1;
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

    /// Acquire a slot for `(repo, agent)` if all tiers allow it.
    pub fn acquire(&mut self, repo: &str, agent: &str) -> bool {
        if !self.can_dispatch(repo, agent) {
            return false;
        }
        self.global_used += 1;
        *self.repo_used.entry(repo.to_string()).or_insert(0) += 1;
        *self.agent_used.entry(agent.to_string()).or_insert(0) += 1;
        true
    }

    /// Release a slot for `(repo, agent)` (on `waiting_input`, `cancelled`, or
    /// completion). Saturating; zeroed entries are dropped.
    pub fn release(&mut self, repo: &str, agent: &str) {
        self.global_used = self.global_used.saturating_sub(1);
        decrement(&mut self.repo_used, repo);
        decrement(&mut self.agent_used, agent);
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
    pub task_id: i64,
    /// Selected repository.
    pub repo: String,
    /// Agent plugin.
    pub agent: String,
    /// Priority (higher first).
    pub priority: i64,
}

/// Greedily dispatch ready tasks in priority order (higher first, FIFO on
/// ties), acquiring slots as available (F-43). Returns the task ids to
/// dispatch; `slots` is advanced for each. A task blocked by a full tier is
/// skipped so it does not head-of-line-block a different repo/agent.
pub fn plan_dispatch(slots: &mut SlotManager, ready: &[ReadyTask]) -> Vec<i64> {
    let mut order: Vec<&ReadyTask> = ready.iter().collect();
    // Stable sort keeps input order (FIFO) among equal priorities.
    order.sort_by_key(|t| std::cmp::Reverse(t.priority));

    let mut dispatched = Vec::new();
    for task in order {
        if slots.acquire(&task.repo, &task.agent) {
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
    task_id: i64,
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
    pub fn push(&mut self, task_id: i64, priority: i64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.heap.push(QueueEntry {
            priority,
            seq,
            task_id,
        });
    }

    /// Dequeue the highest-priority (then earliest) task id.
    pub fn pop(&mut self) -> Option<i64> {
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
        for s in [
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::Publishing,
        ] {
            assert!(counts_toward_slot(s), "{s} should count");
        }
        for s in [
            TaskState::Queued,
            TaskState::Pending,
            TaskState::WaitingInput,
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
        assert!(slots.acquire("repoA", "herdr"));
        assert!(!slots.can_dispatch("repoB", "herdr"), "agent cap reached");
        // A different agent is fine (repoA cap = 2, global 3).
        assert!(slots.acquire("repoA", "orca"));
        assert!(!slots.can_dispatch("repoA", "orca"), "repoA cap reached");
        // global cap = 3.
        assert!(slots.acquire("repoB", "orca"));
        assert!(!slots.can_dispatch("repoC", "orca"), "global cap reached");
        assert_eq!(slots.global_used(), 3);
    }

    #[test]
    fn release_frees_a_slot_for_another_task() {
        let mut slots = SlotManager::new(Limits::global(1));
        assert!(slots.acquire("r", "a"));
        assert!(!slots.can_dispatch("r", "a"), "global full");
        // Simulate waiting_input releasing the slot (F-45).
        slots.release("r", "a");
        assert!(slots.can_dispatch("r", "a"), "slot freed");
        assert!(slots.acquire("r", "a"));
    }

    #[test]
    fn waiting_resume_round_trip_preserves_counts() {
        let mut slots = SlotManager::new(limits());
        slots.acquire("repoA", "herdr");
        let before = (
            slots.global_used(),
            slots.repo_used("repoA"),
            slots.agent_used("herdr"),
        );
        // waiting_input -> release, resume -> re-acquire.
        slots.release("repoA", "herdr");
        assert_eq!(slots.global_used(), before.0 - 1);
        assert!(slots.acquire("repoA", "herdr"));
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
    fn rebuild_reconstructs_from_active_tasks() {
        let mut slots = SlotManager::new(limits());
        slots.rebuild([
            ("repoA".to_string(), "herdr".to_string()),
            ("repoA".to_string(), "orca".to_string()),
            ("repoB".to_string(), "orca".to_string()),
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

    #[test]
    fn plan_dispatch_respects_priority_fifo_and_slots() {
        let mut slots = SlotManager::new(Limits::global(2));
        let ready = vec![
            ReadyTask {
                task_id: 1,
                repo: "r".into(),
                agent: "a".into(),
                priority: 0,
            },
            ReadyTask {
                task_id: 2,
                repo: "r".into(),
                agent: "a".into(),
                priority: 5,
            },
            ReadyTask {
                task_id: 3,
                repo: "r".into(),
                agent: "a".into(),
                priority: 5,
            },
        ];
        // global=2: highest priority (2) then FIFO tie (3 after 2); id1 blocked.
        let dispatched = plan_dispatch(&mut slots, &ready);
        assert_eq!(dispatched, vec![2, 3]);
        assert_eq!(slots.global_used(), 2);
    }

    #[test]
    fn priority_queue_orders_by_priority_then_fifo() {
        let mut q = PriorityQueue::new();
        q.push(1, 0);
        q.push(2, 5);
        q.push(3, 5);
        q.push(4, 10);
        assert_eq!(q.len(), 4);
        assert_eq!(q.pop(), Some(4)); // highest priority
        assert_eq!(q.pop(), Some(2)); // priority 5, earlier
        assert_eq!(q.pop(), Some(3)); // priority 5, later
        assert_eq!(q.pop(), Some(1)); // lowest
        assert!(q.is_empty());
    }
}
