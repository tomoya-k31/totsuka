//! `run` main loop (#63, §5.1): match → ingest → repo select → worktree →
//! dispatch → monitor → finalize.
//!
//! The [`Engine`] integrates the pieces built by earlier tasks — plugin host
//! (#51), worktree lifecycle (#53), workflow matching (#54), scheduler (#55),
//! repo selection (#56), and restart recovery (#57) — into one event-driven
//! loop:
//!
//! 1. `task/submit` (push, ADR-0008): every task_source plugin pushes its own
//!    tasks; ingest idempotently (F-73).
//! 2. Repository selection (F-10–F-14); ambiguity → `pending` + Notifier.
//! 3. Slot-gated dispatch (F-40–F-43): worktree create → `task/dispatch` →
//!    `state/subscribe`.
//! 4. `state/notification` events drive the task state machine; terminal
//!    handling runs the output policy (stubbed until #65), the
//!    `on_success`/`on_failure` status write-back (F-84), and worktree cleanup
//!    (F-23/F-85). `waiting_input`/`pending`/`done`/`failed` are delivered to
//!    Notifier plugins (F-35/F-90).
//!
//! **One-shot** (default): an initial recovery cycle, then the loop drains
//! until every dispatched task reaches a terminal or waiting state and no
//! push has arrived for its quiet-period floor — every source is push-only since
//! protocol 0.2.0, so a task submitted moments after launch needs a real
//! chance to arrive before the run gives up. **`--watch`**: stays up
//! indefinitely, dispatching every push as it arrives, until shutdown.
//! **Dry run**: [`Engine::dry_run`] is a no-op with zero side effects — push
//! sources have nothing to preview ahead of time.

pub mod hooks;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plugin_protocol::method;
use plugin_protocol::methods::{
    AgentState, ExecutionMode, NotReleased, NotifierEvent, NotifyParams, ResultPublishParams,
    SessionReleaseParams, SessionReleaseResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult, TaskLookupParams, TaskLookupResult, TaskSubmitParams, TaskSubmitResult,
    TaskSubmitStatus, TaskUpdateStatusParams,
};
use plugin_protocol::{Notification, Task, jsonrpc};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};

use crate::adapters::clock::SystemClock;
use crate::adapters::plugin_host::{HostError, IncomingRequest, Plugin};
use crate::adapters::state_db::{
    NewTask, StateDb, StateError, TaskMessage, TaskMessageInsert, TaskMessageOutcome, TaskRecord,
};
use crate::adapters::{EngineSignalSink, hook_uds};
use crate::config::{
    CleanupPolicyConfig, CleanupPolicyName, DEFAULT_GLOBAL_CONCURRENCY, OutputPolicy, PluginKind,
    Profile, RootConfig, WorkflowMode, resolve::ResolveError,
};
use crate::domain::signal::{AgentSignal, JobId};
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::workflow::Workflow;
use crate::paths::Paths;
use crate::ports::agent_session::AttachOutcome;
use crate::ports::clock::Clock;
use crate::ports::git::GitRunner;
use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};
use crate::ports::secret::SecretString;
use crate::ports::signal_ingress::FocusOutcome;
use crate::recovery::{self, RecoveryReport, RetryPlan};
use crate::repo_select::{ReadmeCache, RepoCandidate, RepoDecision, SelectConfig, select_repo};
use crate::scheduler::{Limits, ReadyTask, SlotManager, counts_toward_slot, plan_dispatch};
use crate::tool::{LaunchInputs, ToolProfile};
use crate::worktree::{
    CleanupDecision, CleanupOutcome, CleanupPolicy, CreateRequest, DEFAULT_WORKTREE_NAME_TEMPLATE,
    WorktreeError, WorktreeManager, default_location_template,
};

mod dispatch;
mod events;
mod finalize;
use finalize::{PaneRelease, ReleaseMode};
mod ingest;
mod report;
mod settings;
mod supervise;
mod support;
mod sweep;

use support::*;

pub use report::{DryRunEntry, MethodReport, PluginReport, RunStats, RunSummary};
pub use settings::{
    EngineError, EngineSettings, HookRuntime, PluginSet, RepoSettings, RestartPolicy,
    settings_from_config,
};

/// Lines of a repository README shown to the LLM as selection context (F-11).
const README_HEAD_LINES: usize = 30;

/// How long the run loop sleeps between periodic maintenance ticks (settle
/// checks in one-shot, timeout/retention sweeps in both modes).
const SETTLE_TICK: Duration = Duration::from_millis(200);

/// Minimum interval between worktree-retention sweeps (#210). The sweep runs
/// `git status --porcelain` per retained worktree, so re-running it every
/// [`SETTLE_TICK`] would cost a process spawn 5×/s per `Retained`/
/// `DirtySkipped` worktree — and the `keep_7d`/`keep_28d` presets *retain by
/// design* for days. 60s granularity is meaningless against day-scale
/// retention; the done-time cleanup (`finalize_success`) stays immediate.
const WORKTREE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// One-shot's quiet-period floor before an empty `settled()` is trusted
/// (0.2.0): every source is push-only, so a task submitted moments after
/// launch (plugin spawn → `initialize` → `task/submit`) has not necessarily
/// arrived yet when the loop's very first iteration starts. One-shot keeps
/// the loop alive until this much time has passed since the last event
/// (submit, hook signal, …), so a source that is still mid-handshake gets a
/// real chance before the run gives up and reports nothing to do.
const ONE_SHOT_GRACE: Duration = Duration::from_secs(2);

/// An event observed by the run loop.
///
/// `pub(crate)` so the signal-ingress driving adapter
/// ([`EngineSignalSink`](crate::adapters::EngineSignalSink)) can enqueue a
/// [`HookSignal`](PluginEvent::HookSignal); the variant is never exposed across
/// the crate boundary.
pub(crate) enum PluginEvent {
    /// A `state/notification` from an agent plugin.
    State(String, StateNotification),
    /// A plugin process exited without being asked to (§5.3, #495). Emitted
    /// for **every** kind, from the child's own exit rather than from any one
    /// stream.
    Closed(String),
    /// A scheduled relaunch of a crashed plugin has come due (#495). The
    /// backoff is slept in a spawned task so the engine loop keeps serving
    /// events while a plugin is down.
    RestartDue(String),
    /// A relaunch attempt finished (#495). Boxed because a live [`Plugin`] is
    /// much larger than the other variants, and every event on the channel
    /// would otherwise pay for it.
    Restarted {
        /// The plugin instance that was relaunched.
        name: String,
        /// The new process, or why it could not be started.
        outcome: Box<Result<Plugin, HostError>>,
    },
    /// A normalized Claude Code hook signal from the UDS receiver (#136).
    /// Engine interpretation (state transitions, verification) lands in #138.
    HookSignal(AgentSignal),
    /// A `POST /focus` control request (F-94): focus the task's pane and
    /// answer the outcome over `respond` (request-response, unlike a signal).
    Focus {
        /// The task whose pane should come to the foreground.
        task_id: i64,
        /// Where the adapter awaits the outcome.
        respond: tokio::sync::oneshot::Sender<FocusOutcome>,
    },
    /// A `task/submit` from a task source (P→O, 0.1.6): persist the task and
    /// answer the ack over `respond` **after** the durable write committed
    /// (persist-before-ack). A JSON-RPC error answer means "retry with
    /// backoff"; a `TaskSubmitResult` is final.
    TaskSubmit {
        /// The submitting plugin's instance name (overwrites `task.source`).
        source: String,
        /// The workflow the plugin says this task belongs to (0.6.0, #554).
        workflow: String,
        /// The task in the common schema.
        task: Task,
        /// Where the forwarder awaits the ack.
        respond: SubmitRespond,
    },
    /// A `task/lookup` from a task source (P→O, 0.2.4, #242): answer whether
    /// the conversation is already known, and which repository it settled on.
    /// Read-only — nothing about the task changes.
    TaskLookup {
        /// The asking plugin's instance name (authoritative over the params').
        source: String,
        /// The conversation identity to look up.
        task_id: String,
        /// Where the forwarder awaits the answer.
        respond: LookupRespond,
    },
}

/// The answer channel for one [`PluginEvent::TaskSubmit`].
type SubmitRespond = tokio::sync::oneshot::Sender<Result<TaskSubmitResult, jsonrpc::Error>>;

/// The answer channel for one [`PluginEvent::TaskLookup`].
type LookupRespond = tokio::sync::oneshot::Sender<Result<TaskLookupResult, jsonrpc::Error>>;

/// Per-plugin cap on in-flight `task/submit` requests (backpressure; an
/// exhausted budget answers `SUBMIT_OVERLOADED`, which the plugin retries
/// with backoff). Persisting is one SQLite upsert, so this rarely binds.
const SUBMIT_IN_FLIGHT_BUDGET: usize = 64;

/// The same cap for `task/lookup`, kept **separate** from the submit budget
/// so a burst of one can never starve the other. A lookup is one indexed
/// `SELECT` answered from the engine loop, so this rarely binds either.
const LOOKUP_IN_FLIGHT_BUDGET: usize = 64;

/// What one ingest did to the conversation it belongs to (#242).
///
/// Only [`Duplicate`](Self::Duplicate) is a no-op; the other three all
/// accepted new work, and the difference between them is what the
/// conversation looked like beforehand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngestOutcome {
    /// The conversation did not exist; it was created with this as its first
    /// message.
    Created,
    /// Appended to a finished conversation, which was requeued
    /// ([`TaskEvent::Reopen`]).
    Reopened,
    /// Appended to a conversation still in flight; its state is unchanged and
    /// the running dispatch collects the message when it finishes.
    Appended,
    /// The ledger already held this `message_key` — a re-delivery. Nothing
    /// changed.
    Duplicate,
}

impl IngestOutcome {
    /// The ack the source gets back.
    ///
    /// Everything that accepted the message answers `Accepted`, including a
    /// reopen: from the source's side "we took it" is the same statement
    /// regardless of what the conversation was doing. Only a re-delivery is
    /// `Duplicate`, and both are final — the plugin must not re-submit either.
    fn ack(self) -> TaskSubmitStatus {
        match self {
            Self::Created | Self::Reopened | Self::Appended => TaskSubmitStatus::Accepted,
            Self::Duplicate => TaskSubmitStatus::Duplicate,
        }
    }
}

/// A router used when no `[llm]` is configured: repo selection that would need
/// the LLM deterministically falls back to `pending` (F-14) with an actionable
/// reason instead of failing the task.
pub(crate) struct NoLlmRouter;

impl LlmRouter for NoLlmRouter {
    async fn chat_json(&self, _request: &ChatRequest) -> Result<Value, LlmError> {
        Err(LlmError::InvalidResponse(
            "no [llm] configured → set [llm] in config.toml, add a repo hint to the task, \
             or register a single repository"
                .to_string(),
        ))
    }
}

/// The run-loop engine. Owns the state DB, the launched plugins, and the slot
/// accounting for one `run` invocation.
pub struct Engine<G: GitRunner, L: LlmRouter> {
    db: StateDb,
    settings: EngineSettings,
    plugins: PluginSet,
    worktrees: WorktreeManager<G>,
    llm: Option<L>,
    slots: SlotManager,
    /// Per-task slot ledger: task id → the exact `(repo, agent)` pair it holds
    /// a slot under. Releases go through this so a task that never acquired
    /// (e.g. an over-cap resume) can never release another task's slot.
    slot_holders: HashMap<i64, (String, String)>,
    /// `(agent plugin, session_id)` → task id, for routing notifications.
    sessions: HashMap<(String, String), i64>,
    /// Availability answers for the external tools a profile needs (#399),
    /// cached so the 200 ms dispatch loop does not re-stat every tick.
    agent_tools: crate::agent_tools::ToolCache,
    /// Tasks already reported as blocked on a missing tool, so the operator is
    /// told once rather than every cycle.
    ///
    /// In-process, not persisted: a restart re-notifies once, which is the
    /// right amount — the situation is still true and the previous message is
    /// gone from the operator's notification centre anyway. Persisting it would
    /// mean a schema change for a message.
    blocked_on_tools: std::collections::HashSet<i64>,
    /// Tasks already reported as waiting for a downed agent plugin (#499), on
    /// the same once-per-task contract as `blocked_on_tools`. Cleared when the
    /// task finally gets past the gate, so a second outage is reported again.
    blocked_on_agent: std::collections::HashSet<i64>,
    /// Plugins the supervisor has stopped trying to relaunch (#495/#499).
    /// A task waiting on one of these is waiting forever, so dispatch fails it
    /// with a reason instead of parking it.
    abandoned_plugins: std::collections::HashSet<String>,
    /// Relaunch attempts per plugin inside the policy window (#495).
    restarts: HashMap<String, supervise::RestartLedger>,
    /// Call stats harvested from plugin instances that have been replaced
    /// (#497). A restart (#495) creates a **new** `Plugin`, so its counters
    /// start at zero; without carrying the old ones forward, the plugin that
    /// crashed most would report the fewest calls — the opposite of the truth.
    retired_stats: HashMap<String, crate::adapters::plugin_host::CallStats>,
    /// Per-plugin crash and restart tallies (#497), so the summary can name
    /// *which* plugin is flapping rather than only how many times something
    /// did.
    plugin_events: HashMap<String, (usize, usize)>,
    events: mpsc::UnboundedReceiver<PluginEvent>,
    /// Kept so `events.recv()` never observes a closed channel, and cloned
    /// whenever a consumer task has to be re-spawned — which a plugin restart
    /// does for every stream the dead process owned (#495).
    events_tx: mpsc::UnboundedSender<PluginEvent>,
    readme_cache: Option<ReadmeCache>,
    /// Accumulated agent output (streamed `log_chunk`s) per task, used as the
    /// `output = source` publish artifact (F-07).
    agent_output: HashMap<i64, String>,
    /// Session rows whose pane has been released — or is known to be
    /// unreleasable — so a repeated cleanup never re-sends `session/release`
    /// for the same dispatch (#210).
    ///
    /// Keyed by `sessions.id`, **not** by task (#486). A task can hold several
    /// panes over its life (retry, a follow-up message), and a task-keyed memo
    /// had to be cleared by hand wherever a new pane appeared — a rule whose
    /// failure was silent: forget it and the new pane simply never gets
    /// released, which is the leak #210 was filed for. A dispatch creates a new
    /// session row, so this key invalidates itself.
    released_panes: HashSet<i64>,
    /// When the last worktree-retention sweep ran (#210); `None` at startup so
    /// the first `cycle()` always sweeps (startup recovery stays immediate).
    last_worktree_sweep: Option<tokio::time::Instant>,
    /// Wall-clock source for retention decisions and timeout sweeps (#174);
    /// a seam so time-dependent behavior is testable deterministically.
    clock: Arc<dyn Clock>,
    stats: RunStats,
}

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Where a new project item goes, per repository (#542).
    ///
    /// Rebuilt on each call from the live plugin set rather than cached at
    /// startup: a plugin restart (#495) replaces the `Plugin` object, so a
    /// snapshot taken once would go on describing the dead process's claims —
    /// and a plugin whose config changed across the restart would route to the
    /// old board with nothing saying so.
    ///
    /// Sources are visited in **name order**. `PluginSet::sources` is a
    /// `HashMap`, and iteration order would otherwise vary between runs of the
    /// same config. Since #554 that no longer decides anything — a repository
    /// names one `[[projects]]` entry and the entry names one source, so two
    /// plugins cannot claim it — but the registry is also what the prose is
    /// read out of, and unordered iteration would still shuffle *that*.
    fn claim_registry(&self) -> crate::plugins::claims::ClaimRegistry {
        let mut names: Vec<&String> = self.plugins.sources.keys().collect();
        names.sort();
        crate::plugins::claims::ClaimRegistry::from_sources(
            names
                .into_iter()
                .map(|name| (name.as_str(), self.plugins.sources[name].claimed_repos())),
        )
    }

    /// Build an engine over launched plugins. Spawns a forwarder per agent
    /// plugin so `state/notification` streams (F-38) are consumed from the
    /// moment of construction — dispatch must happen after this, never before,
    /// or early notifications would be dropped.
    pub async fn new(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
    ) -> Self {
        Self::build(db, settings, plugins, git, llm, Arc::new(SystemClock)).await
    }

    /// Build an engine with an explicit [`Clock`] (#174) — the seam
    /// deterministic tests use to control retention and timeout decisions.
    ///
    /// Callers that also injected a clock into [`StateDb`] must pass the
    /// **same `Arc`** here, or DB timestamps and engine decisions would run
    /// on two different timelines.
    pub async fn with_clock(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::build(db, settings, plugins, git, llm, clock).await
    }

    /// Shared constructor body behind [`new`](Self::new) and the seam
    /// variants.
    async fn build(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        for (name, plugin) in &plugins.agents {
            supervise::wire_agent(name, plugin, &tx).await;
        }
        // 0.1.6: consume plugin-initiated requests (`task/submit`) from every
        // task source. Parsing and backpressure happen here; persistence and
        // the ack decision happen on the engine loop (persist-before-ack).
        // Ordering per source is preserved: the event-channel send is inline,
        // only the ack await is spawned off.
        for (name, plugin) in &plugins.sources {
            supervise::wire_source(name, plugin, &tx).await;
        }
        // Liveness is watched for every kind, notifiers included (#495): before
        // this, a dead `task_source` produced no event at all and the run kept
        // going as a process that would never receive another task.
        for (name, plugin) in plugins
            .sources
            .iter()
            .chain(plugins.agents.iter())
            .chain(plugins.notifiers.iter())
        {
            supervise::wire_liveness(name, plugin, &tx);
        }
        let slots = SlotManager::new(settings.limits.clone());
        let readme_cache = settings.readme_cache_dir.clone().map(ReadmeCache::new);
        Self {
            agent_tools: crate::agent_tools::ToolCache::default(),
            blocked_on_tools: std::collections::HashSet::new(),
            blocked_on_agent: std::collections::HashSet::new(),
            abandoned_plugins: std::collections::HashSet::new(),
            db,
            settings,
            plugins,
            worktrees: WorktreeManager::new(git),
            llm,
            slots,
            restarts: HashMap::new(),
            retired_stats: HashMap::new(),
            plugin_events: HashMap::new(),
            slot_holders: HashMap::new(),
            sessions: HashMap::new(),
            events: rx,
            events_tx: tx,
            readme_cache,
            agent_output: HashMap::new(),
            released_panes: HashSet::new(),
            last_worktree_sweep: None,
            clock,
            stats: RunStats::default(),
        }
    }

    /// Borrow the state DB (status queries, tests).
    pub fn db(&self) -> &StateDb {
        &self.db
    }

    /// Startup recovery (§5.3): re-attach in-flight sessions, rebuild slot
    /// usage (F-45), restore the session→task routing table, and finish tasks
    /// whose agent already completed while the orchestrator was down.
    pub async fn recover(&mut self) -> Result<RecoveryReport, EngineError> {
        let report = {
            let attacher = crate::adapters::PluginAgentSession::new(&self.plugins.agents);
            recovery::recover(&self.db, &attacher).await?
        };
        self.slots
            .rebuild(recovery::active_slot_claims(&self.db, &report)?);
        for outcome in report.resumed() {
            if let (Some(plugin), Some(session_id)) =
                (outcome.plugin.clone(), outcome.session_id.clone())
            {
                self.sessions.insert((plugin, session_id), outcome.task_id);
            }
            let Some(record) = self.db.get_task(outcome.task_id)? else {
                continue;
            };
            // Mirror the rebuilt slot usage into the per-task ledger.
            if counts_toward_slot(record.state)
                && let (Some(repo), Some(plugin)) = (record.repo.clone(), outcome.plugin.clone())
            {
                self.slot_holders.insert(outcome.task_id, (repo, plugin));
            }
            match record.state {
                // The agent finished while we were down. Re-subscribing does
                // not replay a terminal state (plugins only stream *future*
                // changes), so finalize now instead of waiting forever.
                TaskState::Publishing => {
                    // Restore the published artifact captured on the
                    // BeginPublish transition, so a crash *during* the previous
                    // finalize does not publish a placeholder.
                    if let Some(artifact) = self.persisted_artifact(outcome.task_id)? {
                        self.agent_output.insert(outcome.task_id, artifact);
                    }
                    self.finalize_success(&record).await?;
                }
                // Surface the open question again (F-35); the agent will not
                // re-announce it.
                TaskState::WaitingInput => {
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::WaitingInput,
                        &record,
                        None,
                    );
                }
                _ => {}
            }
        }
        for outcome in report.needs_confirmation() {
            tracing::warn!(
                task_id = outcome.task_id,
                "task could not be resumed automatically → {} ({:?})",
                recovery::NEXT_ACTIONS.join(" / "),
                outcome.result
            );
        }
        // Replay any hook signals that were spooled while the orchestrator was
        // down (E-07): the idempotency key makes a read-all-then-delete safe.
        self.replay_spool().await?;
        Ok(report)
    }

    /// Detect worktrees git knows about that no task owns (F-24). Warn-only:
    /// `doctor` (#64) proposes the actual cleanup.
    pub fn warn_orphan_worktrees(&self) -> Result<Vec<PathBuf>, EngineError> {
        let known: HashSet<PathBuf> = self
            .db
            .list_tasks()?
            .into_iter()
            .filter_map(|t| t.worktree_path.map(PathBuf::from))
            .collect();
        let mut orphans = Vec::new();
        for repo in &self.settings.repos {
            match self.worktrees.detect_orphans(&repo.path, &known) {
                Ok(found) => {
                    for path in found {
                        tracing::warn!(
                            repo = %repo.name,
                            worktree = %path.display(),
                            "orphan worktree (no owning task) → run `totsuka doctor` to clean up"
                        );
                        orphans.push(path);
                    }
                }
                Err(e) => {
                    tracing::warn!(repo = %repo.name, "orphan detection failed: {e}");
                }
            }
        }
        Ok(orphans)
    }

    /// Run the loop: an initial cycle, then event-driven monitoring. One-shot
    /// (`watch = false`) exits once every dispatched task reaches a terminal or
    /// waiting state AND its quiet-period floor has passed (§5.1, 0.2.0:
    /// every source pushes, so a just-launched source's first submission may
    /// not have landed on the loop's first iteration — the grace period gives
    /// it a real chance instead of exiting on an empty snapshot); `--watch`
    /// keeps the loop alive, receiving `task/submit` pushes as they arrive,
    /// until `shutdown` resolves (SIGINT → graceful: in-flight tasks stay in
    /// the state DB for next-start recovery). There is no Orchestrator-side
    /// polling to schedule tasks with, but a short heartbeat tick still
    /// re-runs [`cycle`](Self::cycle) periodically in both modes so signal
    /// timeouts (D-03) and worktree retention (F-23) are re-checked even when
    /// no push event happens to arrive.
    pub async fn run<F>(&mut self, watch: bool, shutdown: F) -> Result<RunSummary, EngineError>
    where
        F: std::future::Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut interrupted = false;

        // Start the UDS hook receiver (#136), if a hook runtime is configured.
        // It runs as a detached task; a `watch` channel signals graceful
        // shutdown so it can unlink the socket. The runtime stays in
        // `settings.hook` (dispatch reads it all run long); only the socket
        // path + token are cloned here for the server.
        let hook_handle = match self.settings.hook.as_ref() {
            Some(hs) => match hook_uds::bind(&hs.socket_path) {
                Ok(listener) => {
                    let socket_path = hs.socket_path.clone();
                    tracing::info!(socket = %socket_path.display(), "hook receiver listening");
                    let sink = EngineSignalSink::new(self.events_tx.clone());
                    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                    // The sink doubles as the focus port (F-94): both feed the
                    // same event channel, focus with a response oneshot.
                    let handle = tokio::spawn(hook_uds::serve(
                        listener,
                        socket_path,
                        sink.clone(),
                        sink,
                        hs.auth_token.clone(),
                        stop_rx,
                    ));
                    Some((stop_tx, handle))
                }
                Err(e) => {
                    tracing::warn!(
                        socket = %hs.socket_path.display(),
                        "hook receiver failed to bind: {e} → hook-driven completion is unavailable this run"
                    );
                    None
                }
            },
            None => None,
        };

        self.cycle().await?;

        // One-shot's quiet-period floor: every source is push-only, so a
        // task submitted right after launch may not have arrived by the
        // loop's very first iteration. Reset on every event so the loop
        // keeps giving a still-arriving submission a chance instead of
        // exiting the instant nothing happens to be monitored yet.
        let mut last_activity = tokio::time::Instant::now();

        loop {
            if !watch && self.settled()? && last_activity.elapsed() >= self.settings.one_shot_grace
            {
                break;
            }
            tokio::select! {
                _ = &mut shutdown => {
                    interrupted = true;
                    break;
                }
                event = self.events.recv() => {
                    if let Some(event) = event {
                        last_activity = tokio::time::Instant::now();
                        self.on_event(event).await?;
                        self.dispatch_ready().await?;
                    }
                }
                _ = tokio::time::sleep(SETTLE_TICK) => {
                    // Periodic maintenance tick (both modes, D-03/F-23): the
                    // old poll-driven `cycle()` call used to double as this
                    // heartbeat before 0.2.0 removed Orchestrator-side
                    // polling — without it, a long-running `--watch` process
                    // would never re-check signal timeouts or worktree
                    // retention unless a push event happened to arrive.
                    self.cycle().await?;
                }
            }
        }

        // Stop the hook receiver and wait for it to unlink its socket.
        if let Some((stop_tx, handle)) = hook_handle {
            let _ = stop_tx.send(true);
            let _ = handle.await;
        }

        // Count deaths that were reported but not yet read (#512).
        //
        // The exit check sits at the top of the loop, before `select!`, so a
        // `Closed` already sitting in the channel is never seen: the run ends
        // reporting `plugin_crashes: 0` while the same summary shows the
        // dispatch that killed the plugin with a `crashed` outcome. Two
        // observers write those numbers — the RPC call site records the
        // transport error, `on_plugin_closed` records the death — and only the
        // second one can be behind.
        //
        // **This closes the reported-but-unread window only.** A child whose
        // exit the watcher has not observed yet is still uncounted; that race
        // is not fixable here, which is why the crash count is pinned by
        // `orchestrator-core/tests/plugin_supervision.rs` (it waits for the
        // condition) rather than by a wall-clock e2e.
        //
        // **Counting only** — deliberately not `on_plugin_closed`. That runs
        // the teardown too: `fail_sessions_of` flips in-flight tasks to
        // `Failed` (contradicting the graceful-shutdown contract on the SIGINT
        // path) and awaits a `task/update_status` write-back, which would turn
        // four lines of bookkeeping into a 120s hang after the run already
        // decided to exit and stopped the hook receiver; `schedule_restart`
        // would book a relaunch no loop is left to consume. Everything else in
        // the channel is dropped exactly as it is today.
        //
        // Safe on the interrupted path for the same reason: a death that
        // happened is counted, and nothing else moves.
        while let Ok(event) = self.events.try_recv() {
            if let PluginEvent::Closed(plugin) = event {
                self.count_plugin_crash(&plugin);
            }
        }

        let mut summary = RunSummary {
            stats: self.stats.clone(),
            interrupted,
            ..RunSummary::default()
        };
        for task in self.db.tasks_in_state(TaskState::WaitingInput)? {
            summary.waiting.push(task.id);
        }
        for task in self.db.tasks_in_state(TaskState::Pending)? {
            summary.pending.push(task.id);
        }
        for task in self.db.tasks_in_state(TaskState::Queued)? {
            summary.queued.push(task.id);
        }
        summary.plugins = self.plugin_reports();
        Ok(summary)
    }

    /// Gracefully shut down every launched plugin.
    pub async fn shutdown(self, grace: Duration) {
        for plugin in self
            .plugins
            .sources
            .values()
            .chain(self.plugins.agents.values())
            .chain(self.plugins.notifiers.values())
        {
            if let Err(e) = plugin.shutdown(grace).await {
                tracing::warn!(plugin = %plugin.name(), "shutdown failed: {e}");
            }
        }
    }

    /// Whether the one-shot loop can exit: no task **this run is monitoring**
    /// is actively executing. `waiting_input`/`pending` tasks remain by design
    /// (§5.1); `queued` leftovers were warned about at dispatch time; a
    /// leftover active-state row with no live session (recovery left it for
    /// human confirmation, §5.3) can never progress, so it must not wedge the
    /// exit.
    fn settled(&self) -> Result<bool, EngineError> {
        let monitored: HashSet<i64> = self.sessions.values().copied().collect();
        for task_id in monitored {
            if let Some(record) = self.db.get_task(task_id)?
                && counts_toward_slot(record.state)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// One full cycle: repo selection for already-ingested tasks, dispatch,
    /// and the timeout/cleanup sweeps. All ingestion since 0.2.0 arrives
    /// asynchronously via `task/submit`, so this is the startup/recovery
    /// sweep, not a fetch pass.
    pub async fn cycle(&mut self) -> Result<(), EngineError> {
        // Drain any hook signals a failed POST spooled (E-07) before acting on
        // state, so a completion that only reached the spool is applied this
        // cycle rather than a cycle late.
        self.replay_spool().await?;
        // Before dispatching: a conversation that finished with messages still
        // queued goes back to `Queued`, so `dispatch_ready` picks it up in the
        // same cycle rather than the next one (#242).
        self.requeue_conversations_with_unsent_messages().await?;
        self.select_repos().await?;
        self.dispatch_ready().await?;
        // Escalate hook-dispatched tasks that have gone silent past their
        // workflow timeout (D-03).
        self.sweep_signal_timeouts().await?;
        // The worktree sweep spawns `git status` per retained worktree, so it
        // runs on its own (longer) interval, not every 200ms tick (#210). The
        // startup cycle (`last_worktree_sweep == None`) always sweeps.
        let sweep_due = self
            .last_worktree_sweep
            .is_none_or(|last| last.elapsed() >= self.settings.worktree_sweep_interval);
        if sweep_due {
            self.sync_branches_of_active_tasks().await?;
            self.sweep_finished_worktrees().await?;
            self.last_worktree_sweep = Some(tokio::time::Instant::now());
        }
        Ok(())
    }
}

/// Deliver an event to every notifier plugin (F-90). Fire-and-forget:
/// delivery failures never affect task execution (F-93).
fn notify_all(
    notifiers: &HashMap<String, Plugin>,
    event: NotifierEvent,
    record: &TaskRecord,
    body: Option<String>,
) {
    let params = NotifyParams {
        event,
        task_id: Some(record.id.to_string()),
        workflow: Some(record.workflow.clone()),
        title: record.title.clone(),
        body,
    };
    deliver_notification(notifiers, &params);
}

/// Deliver one already-built [`NotifyParams`] to every notifier (F-90).
/// Fire-and-forget: delivery failures never affect task execution (F-93).
fn deliver_notification(notifiers: &HashMap<String, Plugin>, params: &NotifyParams) {
    for plugin in notifiers.values() {
        if let Err(e) = plugin.notify(method::NOTIFY, params) {
            tracing::warn!(plugin = %plugin.name(), "notify delivery failed (ignored, F-93): {e}");
        }
    }
}

/// Index workflows by name.
fn workflows_by_name(workflows: &[Workflow]) -> HashMap<&str, &Workflow> {
    workflows.iter().map(|w| (w.name.as_str(), w)).collect()
}

/// Convert a raw plugin notification into a run-loop event, if relevant.
fn state_event(plugin: &str, note: Notification) -> Option<PluginEvent> {
    if note.method != method::STATE_NOTIFICATION {
        return None;
    }
    let params = note.params?;
    match serde_json::from_value::<StateNotification>(params) {
        Ok(state) => Some(PluginEvent::State(plugin.to_string(), state)),
        Err(e) => {
            tracing::warn!(plugin, "malformed state/notification: {e}");
            None
        }
    }
}

/// A ready-to-drive engine for the module tests.
///
/// Lives here rather than in one module's test block because two of them
/// need it, and a `#[cfg(test)]` item in a sibling is not reachable.
/// Renamed from `sweep_test_engine` when it stopped belonging to the sweep
/// tests alone (#464).
#[cfg(test)]
/// A minimal engine (no plugins, no repos) whose only observable behavior
/// is the sweep-throttle bookkeeping.
pub(crate) async fn test_engine(
    interval: Duration,
) -> Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter> {
    let settings = EngineSettings {
        workflows: Vec::new(),
        repos: Vec::new(),
        limits: Limits::global(1),
        worktree_name_template: DEFAULT_WORKTREE_NAME_TEMPLATE.to_string(),
        location_template: "/tmp/totsuka-sweep/{repo_name}/{worktree_name}".to_string(),
        cleanup_implement: CleanupPolicy::Manual,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        readme_cache_dir: None,
        worktree_sweep_interval: interval,
        one_shot_grace: ONE_SHOT_GRACE,
        tools: crate::tool::builtin_registry(),
        default_tool: "claude".to_string(),
        prompts: Default::default(),
        plugin_restart: Default::default(),
        restart_disabled: Default::default(),
        hook: None,
    };
    Engine::new(
        StateDb::open_in_memory().unwrap(),
        settings,
        PluginSet::default(),
        crate::adapters::git::SystemGitRunner,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #512: a death reported while the loop was deciding to stop is still
    /// counted.
    ///
    /// The exit check sits at the top of the loop, before `select!`, so with a
    /// zero grace and nothing to settle the first iteration breaks without
    /// ever polling the channel. That is exactly the window the summary used
    /// to lose — it reported `plugin_crashes: 0` while the same document
    /// showed the dispatch that killed the plugin with a `crashed` outcome.
    ///
    /// Deterministic because the event is enqueued *before* `run` is called:
    /// no wall clock, no child process, no race.
    #[tokio::test]
    async fn a_death_queued_when_the_loop_exits_is_still_counted() {
        let mut engine = test_engine(Duration::from_secs(3600)).await;
        engine.settings.one_shot_grace = Duration::ZERO;
        engine
            .events_tx
            .send(PluginEvent::Closed("mock_agent".to_string()))
            .expect("the receiver is alive");

        let summary = engine
            .run(false, std::future::pending())
            .await
            .expect("run loop error");

        assert_eq!(
            summary.stats.plugin_crashes, 1,
            "the queued death must reach the summary: {summary:?}"
        );
        let report = summary
            .plugins
            .get("mock_agent")
            .expect("the plugin must appear in the per-plugin report");
        assert_eq!(report.crashes, 1, "{report:?}");
    }

    /// #409/#410: a read-only profile that ended up on a branch is failed
    /// rather than published. Gated on the **profile** — a plain
    /// `mode = "plan"` workflow never promised anything about branches, so
    /// failing it would make an existing config start losing tasks on upgrade.
    #[test]
    fn a_read_only_profile_on_a_branch_is_a_publish_failure() {
        let check = |profile, branch| read_only_side_effect("wf", profile, branch, 42, "/wt/t42");

        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            let reason = check(Some(profile), Some("feat/x"))
                .unwrap_or_else(|| panic!("{profile:?} on a branch must not publish"));
            assert!(reason.contains("feat/x"), "{reason}");
            assert!(reason.contains(profile.as_str()), "{reason}");
            // The operator has to be told the part that cannot be undone…
            assert!(reason.contains("pushed"), "{reason}");
            // …and how to get out, because a plain retry hits this again.
            assert!(reason.contains("switch --detach"), "{reason}");
            assert!(reason.contains("/wt/t42"), "{reason}");
            assert!(reason.contains("cancel 42"), "{reason}");
        }
        // Detached: nothing to report. This is the state an operator reaches
        // by following the remedy, so it must clear the gate.
        assert!(check(Some(Profile::Design), None).is_none());
        // `implement` is what branches are for.
        assert!(check(Some(Profile::Implement), Some("feat/x")).is_none());
        // No profile: the legacy `mode = "plan"` shape keeps its warning only.
        assert!(check(None, Some("feat/x")).is_none());
    }
}
