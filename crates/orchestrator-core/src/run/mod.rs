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
pub mod output;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use plugin_protocol::method;
use plugin_protocol::methods::{
    AgentState, ExecutionMode, HookLaunchSpec, NotifierEvent, NotifyParams, ResultPublishParams,
    SessionReleaseParams, SessionReleaseResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult, TaskSubmitParams, TaskSubmitResult, TaskSubmitStatus,
    TaskUpdateStatusParams,
};
use plugin_protocol::{Notification, Task, jsonrpc};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};

use crate::adapters::clock::SystemClock;
use crate::adapters::plugin_host::{IncomingRequest, Plugin};
use crate::adapters::state_db::{
    NewTask, StateDb, StateError, TaskMessageInsert, TaskMessageOutcome, TaskRecord,
};
use crate::adapters::{EngineSignalSink, hook_uds};
use crate::config::{
    CleanupPolicyConfig, CleanupPolicyName, DEFAULT_GLOBAL_CONCURRENCY, OutputPolicy, PluginKind,
    RootConfig, WorkflowMode, resolve::ResolveError,
};
use crate::domain::signal::{AgentSignal, JobId};
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::workflow::{Workflow, match_workflow};
use crate::paths::Paths;
use crate::ports::agent_session::AttachOutcome;
use crate::ports::clock::Clock;
use crate::ports::git::GitRunner;
use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};
use crate::ports::secret::SecretString;
use crate::ports::signal_ingress::FocusOutcome;
use crate::recovery::{self, RecoveryReport, RetryPlan};
use crate::repo_select::{ReadmeCache, RepoCandidate, RepoDecision, SelectConfig, select_repo};
use crate::run::output::{
    DEFAULT_PR_BODY_TEMPLATE, DEFAULT_PR_TITLE_TEMPLATE, GhPrCreator, PrContext, PrCreator,
    PrRequest, render_template,
};
use crate::scheduler::{Limits, ReadyTask, SlotManager, counts_toward_slot, plan_dispatch};
use crate::tool::{LaunchInputs, ToolProfile};
use crate::worktree::{
    CleanupDecision, CleanupOutcome, CleanupPolicy, CreateRequest, DEFAULT_BRANCH_TEMPLATE,
    WorktreeError, WorktreeManager, default_location_template,
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

/// Errors that abort the run loop (per-task failures are handled in-loop).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// State DB failure — the loop cannot proceed without persistence.
    #[error(transparent)]
    Db(#[from] StateError),
}

/// A repository the engine can target (paths already expanded).
#[derive(Debug, Clone)]
pub struct RepoSettings {
    /// Repository name (config `[[repositories]].name`).
    pub name: String,
    /// Absolute local clone path.
    pub path: PathBuf,
    /// Free-text summary for LLM selection (F-61).
    pub summary: Option<String>,
    /// Per-repo worktree location template override (F-22).
    pub worktree_location: Option<String>,
    /// Per-repo default AI tool (#196); resolved at dispatch time
    /// (workflow pin > this > global default), same carry-unresolved pattern
    /// as `worktree_location`.
    pub tool: Option<String>,
}

/// Interpreted engine configuration, assembled from [`RootConfig`] by
/// [`settings_from_config`] (or built directly in tests).
#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Workflows in definition order (F-81).
    pub workflows: Vec<Workflow>,
    /// Target repositories.
    pub repos: Vec<RepoSettings>,
    /// Concurrency limits (F-40–F-42).
    pub limits: Limits,
    /// Branch-name template (F-21).
    pub branch_template: String,
    /// Global worktree location template (F-22).
    pub location_template: String,
    /// Cleanup policy for implement-mode worktrees (F-23).
    pub cleanup_implement: CleanupPolicy,
    /// Cleanup policy for plan-mode worktrees (F-85).
    pub cleanup_plan: CleanupPolicy,
    /// Environment for `${ENV}` expansion in worktree templates.
    pub env: HashMap<String, String>,
    /// Repo-selection tuning (F-14).
    pub select: SelectConfig,
    /// README head cache directory (`$XDG_CACHE_HOME/totsuka`), if any.
    pub readme_cache_dir: Option<PathBuf>,
    /// Pull-request title template (F-86).
    pub pr_title_template: String,
    /// Pull-request body template (F-86).
    pub pr_body_template: String,
    /// Minimum interval between worktree-retention sweeps (#210). Not exposed
    /// in config (no user knob); tests set [`Duration::ZERO`] to sweep every
    /// cycle.
    pub worktree_sweep_interval: Duration,
    /// Resolved AI-tool registry (#196): built-ins overlaid with `[tools]`
    /// entries, keyed by tool name. Dispatch resolves each task's tool here
    /// and sends the assembled [`ToolLaunchSpec`](plugin_protocol::methods::ToolLaunchSpec) to the agent plugin.
    pub tools: std::collections::HashMap<String, ToolProfile>,
    /// Global default tool name (#196) when neither the workflow nor the
    /// selected repository picks one. `"claude"` unless `default_tool` is set.
    pub default_tool: String,
    /// Claude Code hook runtime (#131/#138): receiver endpoint, auth token,
    /// spool dir, per-workflow `--settings` paths, and the escalation
    /// threshold. A normal `totsuka run` always sets this (the CLI builds it
    /// even when `[hooks]` is unset — a default socket path is used, so a config
    /// with no hook-capable agent simply never receives a POST). `None` only for
    /// `--dry-run` (read-only: no receiver, no dispatch) and hook-disabled
    /// tests; when `None` the receiver never starts and dispatch never sets a
    /// [`HookLaunchSpec`].
    pub hook: Option<HookRuntime>,
}

/// Everything the engine needs to drive hook-based agents for one run
/// (#131/#138). Assembled by the CLI (`run_cmd`): it resolves the Bearer token
/// via the platform secret store, expands the socket/spool paths, and looks up
/// each workflow's rendered settings file. `None` in tests and in configs with
/// no hook-capable agent.
#[derive(Debug, Clone)]
pub struct HookRuntime {
    /// UDS path the receiver binds and hooks POST to (also injected as
    /// `TOTSUKA_HOOK_ENDPOINT`). Created `0600`; stale sockets are unlinked.
    pub socket_path: PathBuf,
    /// Bearer token every POST must present (`Authorization: Bearer <token>`),
    /// also injected as `TOTSUKA_HOOK_TOKEN`. `None` disables the check (0600
    /// socket only); the CLI logs a warning in that case.
    pub auth_token: Option<SecretString>,
    /// Directory the hooks spool NDJSON to when a POST fails (E-07), also
    /// injected as `TOTSUKA_HOOK_SPOOL_DIR`. The engine drains it after
    /// `recover()` and on every cycle. `None` disables at-least-once recovery.
    pub spool_dir: Option<PathBuf>,
    /// Per-workflow rendered `orchestrator-<workflow>.json` path
    /// (`HookLaunchSpec.settings_path`), keyed by workflow name (H-01/H-03).
    pub settings_paths: HashMap<String, PathBuf>,
    /// Consecutive UNKNOWN stops before a task escalates (D-02).
    pub block_retry_limit: u32,
}

/// Interpret a parsed [`RootConfig`] into [`EngineSettings`].
///
/// `env` supplies `${ENV}`/`~` expansion for repository paths and worktree
/// templates (injectable for tests). `paths` supplies the XDG-resolved bases
/// for defaults the operator did not configure; it is passed in rather than
/// re-resolved here so the engine and the CLI (state DB, logs, hook spool)
/// always agree on one set of directories.
pub fn settings_from_config(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    paths: &Paths,
) -> Result<EngineSettings, ResolveError> {
    let env_fn = |k: &str| env.get(k).cloned();

    let mut repos = Vec::with_capacity(cfg.repositories.len());
    for repo in &cfg.repositories {
        repos.push(RepoSettings {
            name: repo.name.clone(),
            path: crate::config::expand_path(&repo.path.to_string_lossy(), &env_fn)?,
            summary: repo.summary.clone(),
            worktree_location: repo.worktree_location.clone(),
            tool: repo.tool.clone(),
        });
    }

    let limits = Limits {
        global: cfg.max_concurrency.unwrap_or(DEFAULT_GLOBAL_CONCURRENCY),
        per_repo: cfg
            .repositories
            .iter()
            .filter_map(|r| r.max_concurrency.map(|n| (r.name.clone(), n)))
            .collect(),
        per_agent: cfg
            .plugins
            .iter()
            .filter(|(_, p)| p.kind == PluginKind::AgentIde)
            .filter_map(|(name, p)| p.max_concurrency.map(|n| (name.clone(), n)))
            .collect(),
    };

    Ok(EngineSettings {
        workflows: Workflow::from_configs(&cfg.workflows),
        repos,
        limits,
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        location_template: cfg
            .worktree
            .location
            .clone()
            .unwrap_or_else(|| default_location_template(paths)),
        // Implement-mode default is `manual`: a worktree may hold committed but
        // unpushed work until the output policy (#65) publishes it.
        cleanup_implement: cleanup_policy(cfg.worktree.cleanup, CleanupPolicy::Manual),
        // Plan-mode default is `immediate` (F-85): design output is published
        // to the source, the worktree carries nothing unique.
        cleanup_plan: cleanup_policy(cfg.worktree.plan_cleanup, CleanupPolicy::Immediate),
        env: env.clone(),
        select: SelectConfig {
            max_tokens: cfg.llm.as_ref().and_then(|l| l.max_tokens),
            ..SelectConfig::default()
        },
        readme_cache_dir: None,
        pr_title_template: cfg
            .output
            .pr_title_template
            .clone()
            .unwrap_or_else(|| DEFAULT_PR_TITLE_TEMPLATE.to_string()),
        pr_body_template: cfg
            .output
            .pr_body_template
            .clone()
            .unwrap_or_else(|| DEFAULT_PR_BODY_TEMPLATE.to_string()),
        worktree_sweep_interval: WORKTREE_SWEEP_INTERVAL,
        tools: crate::tool::registry_from_config(&cfg.tools),
        default_tool: cfg
            .default_tool
            .clone()
            .unwrap_or_else(|| "claude".to_string()),
        // The hook runtime needs the resolved token, expanded paths, and the
        // per-workflow settings files — all CLI-level (secret store, `Paths`,
        // the `hooks` module). `run_cmd` fills this in before building the
        // engine; interpreting config alone leaves it unset.
        hook: None,
    })
}

/// Map a config cleanup policy to the worktree policy, with a default. The
/// `keep_*` presets (#210) desugar to `RetentionDays` here — [`CleanupPolicy`]
/// never learns about them.
fn cleanup_policy(config: Option<CleanupPolicyConfig>, default: CleanupPolicy) -> CleanupPolicy {
    match config {
        None => default,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Immediate)) => CleanupPolicy::Immediate,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Manual)) => CleanupPolicy::Manual,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep7d)) => {
            CleanupPolicy::RetentionDays(7)
        }
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep28d)) => {
            CleanupPolicy::RetentionDays(28)
        }
        Some(CleanupPolicyConfig::Retention { retention_days }) => {
            CleanupPolicy::RetentionDays(retention_days)
        }
    }
}

/// The launched plugins, split by kind (enabled entries only, F-58).
#[derive(Debug, Default)]
pub struct PluginSet {
    /// task_source plugins by instance name.
    pub sources: HashMap<String, Plugin>,
    /// agent_ide plugins by instance name.
    pub agents: HashMap<String, Plugin>,
    /// notifier plugins by instance name.
    pub notifiers: HashMap<String, Plugin>,
}

/// An event observed by the run loop.
///
/// `pub(crate)` so the signal-ingress driving adapter
/// ([`EngineSignalSink`](crate::adapters::EngineSignalSink)) can enqueue a
/// [`HookSignal`](PluginEvent::HookSignal); the variant is never exposed across
/// the crate boundary.
pub(crate) enum PluginEvent {
    /// A `state/notification` from an agent plugin.
    State(String, StateNotification),
    /// An agent plugin's notification stream closed (process exit, §5.3).
    Closed(String),
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
        /// The task in the common schema.
        task: Task,
        /// Where the forwarder awaits the ack.
        respond: SubmitRespond,
    },
}

/// The answer channel for one [`PluginEvent::TaskSubmit`].
type SubmitRespond = tokio::sync::oneshot::Sender<Result<TaskSubmitResult, jsonrpc::Error>>;

/// Per-plugin cap on in-flight `task/submit` requests (backpressure; an
/// exhausted budget answers `SUBMIT_OVERLOADED`, which the plugin retries
/// with backoff). Persisting is one SQLite upsert, so this rarely binds.
const SUBMIT_IN_FLIGHT_BUDGET: usize = 64;

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

/// Counters accumulated over one `run` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunStats {
    /// Newly ingested tasks that arrived via `task/submit` (0.1.6;
    /// duplicates do not count, F-73).
    pub submitted: usize,
    /// Dispatches performed.
    pub dispatched: usize,
    /// Tasks that reached `done` this run.
    pub done: usize,
    /// Tasks that reached `failed` this run.
    pub failed: usize,
}

/// The summary printed when `run` exits (§5.1 one-shot contract).
#[derive(Debug, Default)]
pub struct RunSummary {
    /// Counters for this run.
    pub stats: RunStats,
    /// Tasks left in `waiting_input` (resume via answer + next run).
    pub waiting: Vec<i64>,
    /// Tasks left in `pending` (repo confirmation, F-14).
    pub pending: Vec<i64>,
    /// Tasks left in `queued` (e.g. unknown workflow after a config change).
    pub queued: Vec<i64>,
    /// Whether the loop exited due to a shutdown signal.
    pub interrupted: bool,
}

/// One line of a `--dry-run` report (§5.1: what would run where, and why).
#[derive(Debug, Clone)]
pub struct DryRunEntry {
    /// Source plugin instance.
    pub source: String,
    /// Source task id.
    pub task_id: String,
    /// Task title.
    pub title: String,
    /// Matched workflow name.
    pub workflow: String,
    /// Execution mode (`plan`/`implement`).
    pub mode: &'static str,
    /// Agent plugin that would receive the dispatch.
    pub agent: String,
    /// Repository decision rationale.
    pub repo: String,
    /// Present when the task is already in the state DB (state name).
    pub already_ingested: Option<String>,
}

/// A router used when no `[llm]` is configured: repo selection that would need
/// the LLM deterministically falls back to `pending` (F-14) with an actionable
/// reason instead of failing the task.
struct NoLlmRouter;

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
    events: mpsc::UnboundedReceiver<PluginEvent>,
    /// Kept so `events.recv()` never observes a closed channel.
    _events_tx: mpsc::UnboundedSender<PluginEvent>,
    readme_cache: Option<ReadmeCache>,
    /// Accumulated agent output (streamed `log_chunk`s) per task, used as the
    /// `output = source` publish artifact (F-07).
    agent_output: HashMap<i64, String>,
    /// Opens pull requests for `output = pull_request` (F-86); a seam so the
    /// push flow is testable without hitting GitHub.
    pr_creator: Box<dyn PrCreator>,
    /// Tasks whose pane release has already been settled this run (#210):
    /// the `session/release` RPC answered, or release is impossible (no
    /// pane-controlling plugin). Without it, a worktree whose removal keeps
    /// failing would be re-released on every sweep.
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
    /// Build an engine over launched plugins. Spawns a forwarder per agent
    /// plugin so `state/notification` streams (F-38) are consumed from the
    /// moment of construction — dispatch must happen after this, never before,
    /// or early notifications would be dropped.
    /// Build an engine with the production [`GhPrCreator`] (opens PRs via
    /// `gh`). Tests use [`Engine::with_pr_creator`] to inject a fake.
    pub async fn new(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
    ) -> Self {
        Self::build(
            db,
            settings,
            plugins,
            git,
            llm,
            Box::new(GhPrCreator),
            Arc::new(SystemClock),
        )
        .await
    }

    /// Build an engine with an explicit pull-request creator (the seam tests
    /// use to exercise the push/PR flow without hitting GitHub).
    pub async fn with_pr_creator(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
        pr_creator: Box<dyn PrCreator>,
    ) -> Self {
        Self::build(
            db,
            settings,
            plugins,
            git,
            llm,
            pr_creator,
            Arc::new(SystemClock),
        )
        .await
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
        Self::build(
            db,
            settings,
            plugins,
            git,
            llm,
            Box::new(GhPrCreator),
            clock,
        )
        .await
    }

    /// Shared constructor body behind [`new`](Self::new) and the seam
    /// variants.
    async fn build(
        db: StateDb,
        settings: EngineSettings,
        plugins: PluginSet,
        git: G,
        llm: Option<L>,
        pr_creator: Box<dyn PrCreator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        for (name, plugin) in &plugins.agents {
            if let Some(mut notifications) = plugin.take_notifications().await {
                let name = name.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    while let Some(note) = notifications.recv().await {
                        if let Some(event) = state_event(&name, note)
                            && tx.send(event).is_err()
                        {
                            return;
                        }
                    }
                    let _ = tx.send(PluginEvent::Closed(name));
                });
            }
        }
        // 0.1.6: consume plugin-initiated requests (`task/submit`) from every
        // task source. Parsing and backpressure happen here; persistence and
        // the ack decision happen on the engine loop (persist-before-ack).
        // Ordering per source is preserved: the event-channel send is inline,
        // only the ack await is spawned off.
        for (name, plugin) in &plugins.sources {
            if let Some(mut incoming) = plugin.take_incoming_requests().await {
                let name = name.clone();
                let tx = tx.clone();
                let budget = Arc::new(Semaphore::new(SUBMIT_IN_FLIGHT_BUDGET));
                tokio::spawn(async move {
                    while let Some(request) = incoming.recv().await {
                        forward_submit(&name, request, &tx, &budget);
                    }
                });
            }
        }
        let slots = SlotManager::new(settings.limits.clone());
        let readme_cache = settings.readme_cache_dir.clone().map(ReadmeCache::new);
        Self {
            db,
            settings,
            plugins,
            worktrees: WorktreeManager::new(git),
            llm,
            slots,
            slot_holders: HashMap::new(),
            sessions: HashMap::new(),
            events: rx,
            _events_tx: tx,
            readme_cache,
            agent_output: HashMap::new(),
            pr_creator,
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
                    let sink = EngineSignalSink::new(self._events_tx.clone());
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
            if !watch && self.settled()? && last_activity.elapsed() >= ONE_SHOT_GRACE {
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
            self.sweep_finished_worktrees().await?;
            self.last_worktree_sweep = Some(tokio::time::Instant::now());
        }
        Ok(())
    }

    /// Re-apply the cleanup policy to finished tasks whose worktree still
    /// exists (F-23: a `retention_days` policy elapses long after the
    /// finishing run's immediate cleanup attempt retained the worktree).
    async fn sweep_finished_worktrees(&mut self) -> Result<(), EngineError> {
        let mut candidates = Vec::new();
        for state in [TaskState::Done, TaskState::Cancelled] {
            for record in self.db.tasks_in_state(state)? {
                if record
                    .worktree_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).exists())
                {
                    candidates.push(record.id);
                }
            }
        }
        for task_id in candidates {
            self.cleanup_worktree(task_id).await?;
        }
        Ok(())
    }

    /// Persist one normalized task under `wf`, idempotently (F-73), appending
    /// the delivery to the conversation's message ledger. Returns
    /// `(row id, outcome)`. Every ingest since 0.2.0 arrives via `task/submit`
    /// (see [`Self::on_task_submit`]).
    ///
    /// Since #242 a task is a **conversation**, so `source_task_id` no longer
    /// distinguishes deliveries — every message in a Slack thread carries the
    /// same one. Dedup therefore moves to the ledger's `message_key`, and the
    /// task row's `ON CONFLICT DO NOTHING` goes back to meaning only "this
    /// conversation already exists" rather than "drop this message".
    fn ingest_task(
        &mut self,
        wf: &Workflow,
        task: &Task,
    ) -> Result<(i64, IngestOutcome), EngineError> {
        let existing = self.db.find_by_source(&task.source, &task.id)?;
        let new_task = NewTask {
            source: task.source.clone(),
            source_task_id: task.id.clone(),
            workflow: wf.name.clone(),
            mode: mode_str(wf.mode).to_string(),
            repo: None,
            priority: task.priority,
            title: task.title.clone(),
            url: task.url.clone(),
            // The full normalized task, so dispatch can reconstruct it.
            source_payload: serde_json::to_value(task).ok(),
            // Carry the source's conversation-continuation key (E-09).
            thread_key: task.thread_key.clone(),
            last_signal_at: None,
        };
        // Existing conversations are left untouched by this (`DO NOTHING`):
        // the title stays the one the conversation opened with, which is what
        // a thread's subject should be.
        let id = self.db.upsert_submitted_task(&new_task)?;

        // A source that sends one message per task omits `message_key`;
        // falling back to the conversation id makes its second delivery
        // collide exactly as before, so GitHub and Notion keep their old
        // at-most-once behaviour without knowing this field exists.
        let message_key = task.message_key.clone().unwrap_or_else(|| task.id.clone());
        let insert = TaskMessageInsert {
            task_id: id,
            message_key: message_key.clone(),
            // The normalized schema has no author field; whoever the source
            // named is preserved in `payload`. Left NULL rather than filled
            // with `assignee`, which is a different person in every source.
            author: None,
            body: task.body.clone().unwrap_or_default(),
            url: task.url.clone(),
            // Not `unwrap_or_default()`: an empty payload would silently
            // destroy the audit record (N-01) and anything reading the message
            // back. A failure here is a persistence failure like any other.
            payload: serde_json::to_string(task).map_err(StateError::from)?,
        };
        // Appending and reopening are one transaction: see
        // `append_task_message_reopening` for why splitting them can strand a
        // message forever.
        let (appended, reopened) = self.db.append_task_message_reopening(
            &insert,
            Some(serde_json::json!({ "kind": "reopen", "message_key": message_key })),
        )?;

        let outcome = match (existing, appended, reopened) {
            (None, ..) => IngestOutcome::Created,
            // The ledger already had this delivery: a Socket Mode re-send, or
            // a restart replaying an unacked message. This is the last line of
            // defence — plugin-side dedup lives in memory and dies with the
            // process — so it must change nothing at all.
            (Some(_), TaskMessageOutcome::Duplicate, _) => IngestOutcome::Duplicate,
            (Some(_), TaskMessageOutcome::New, Some(_)) => IngestOutcome::Reopened,
            // Still in flight: the message waits in the ledger and the
            // in-progress dispatch picks it up when it finishes.
            (Some(_), TaskMessageOutcome::New, None) => IngestOutcome::Appended,
        };
        Ok((id, outcome))
    }

    /// Select a repository for every queued task that has none (F-10–F-14).
    async fn select_repos(&mut self) -> Result<(), EngineError> {
        let queued = self.db.tasks_in_state(TaskState::Queued)?;
        for record in queued.iter().filter(|t| t.repo.is_none()) {
            let task = task_from_record(record);
            let decision = self.decide_repo(&task).await;
            match decision {
                RepoDecision::Selected { repo, reason } => {
                    tracing::info!(task_id = record.id, repo = %repo, "repository selected: {reason}");
                    self.db.set_repo(record.id, &repo)?;
                }
                RepoDecision::Pending { reason } => {
                    tracing::warn!(
                        task_id = record.id,
                        "repository pending confirmation: {reason}"
                    );
                    self.db.apply_event(
                        record.id,
                        TaskEvent::NeedRepoConfirmation,
                        Some(serde_json::json!({ "kind": "repo_select", "reason": reason })),
                    )?;
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::Pending,
                        record,
                        Some(reason),
                    );
                }
                RepoDecision::Failed { reason } => {
                    tracing::error!(task_id = record.id, "repository selection failed: {reason}");
                    self.db.apply_event(
                        record.id,
                        TaskEvent::Fail,
                        Some(serde_json::json!({ "kind": "repo_select", "reason": reason })),
                    )?;
                    self.stats.failed += 1;
                    self.write_back_status(record, false).await;
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::Failed,
                        record,
                        Some(reason),
                    );
                }
            }
        }
        Ok(())
    }

    /// Decide the repository for one task (rules first, LLM fallback).
    async fn decide_repo(&self, task: &Task) -> RepoDecision {
        let candidates: Vec<RepoCandidate> = self
            .settings
            .repos
            .iter()
            .map(|r| RepoCandidate {
                name: r.name.clone(),
                summary: r.summary.clone(),
                readme_head: self
                    .readme_cache
                    .as_ref()
                    .and_then(|c| c.head(&r.path, README_HEAD_LINES)),
            })
            .collect();
        match &self.llm {
            Some(llm) => select_repo(task, &candidates, llm, &self.settings.select).await,
            None => select_repo(task, &candidates, &NoLlmRouter, &self.settings.select).await,
        }
    }

    /// Dispatch queued tasks with a selected repository, gated by slots
    /// (F-40–F-43).
    async fn dispatch_ready(&mut self) -> Result<(), EngineError> {
        let workflows = workflows_by_name(&self.settings.workflows);
        let queued = self.db.tasks_in_state(TaskState::Queued)?;
        let mut ready = Vec::new();
        for record in &queued {
            let Some(repo) = record.repo.clone() else {
                continue; // repo selection pending/failed this cycle
            };
            let Some(wf) = workflows.get(record.workflow.as_str()) else {
                tracing::warn!(
                    task_id = record.id,
                    workflow = %record.workflow,
                    "workflow no longer configured; task stays queued → restore the workflow or cancel the task"
                );
                continue;
            };
            ready.push(ReadyTask {
                task_id: record.id,
                repo,
                agent: wf.agent.clone(),
                priority: record.priority,
            });
        }
        let pair_by_id: HashMap<i64, (String, String)> = ready
            .iter()
            .map(|t| (t.task_id, (t.repo.clone(), t.agent.clone())))
            .collect();
        for task_id in plan_dispatch(&mut self.slots, &ready) {
            if let Some(pair) = pair_by_id.get(&task_id) {
                self.slot_holders.insert(task_id, pair.clone());
            }
            self.dispatch_one(task_id).await?;
        }
        Ok(())
    }

    /// Dispatch a single task: worktree (create or reuse), `task/dispatch` (or
    /// `session/attach` on retry reuse, F-44), `state/subscribe`. The slot has
    /// already been acquired; failure paths release it and fail the task.
    async fn dispatch_one(&mut self, task_id: i64) -> Result<(), EngineError> {
        let record = self
            .db
            .get_task(task_id)?
            .ok_or(StateError::NotFound(task_id))?;
        let repo_name = record.repo.clone().unwrap_or_default();
        let workflows = workflows_by_name(&self.settings.workflows);
        let Some(wf) = workflows.get(record.workflow.as_str()).copied() else {
            self.release_slot(task_id);
            return Ok(()); // warned in dispatch_ready
        };
        let agent_name = wf.agent.clone();

        let Some(repo) = self
            .settings
            .repos
            .iter()
            .find(|r| r.name == repo_name)
            .cloned()
        else {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "selected repository `{repo_name}` is not configured → re-add it to [[repositories]]"
                    ),
                )
                .await;
        };
        match self.plugins.agents.get(&agent_name) {
            None => {
                return self
                    .fail_dispatch(
                        &record,
                        format!(
                            "agent plugin `{agent_name}` is not launched → install and enable it"
                        ),
                    )
                    .await;
            }
            // Without a state stream the task could never progress and its
            // slot would be held for the life of the process — refuse upfront.
            Some(agent) if !agent.capabilities().state_stream => {
                return self
                    .fail_dispatch(
                        &record,
                        format!(
                            "agent plugin `{agent_name}` does not declare the `state_stream` capability → totsuka cannot track its progress; use a state-streaming agent plugin"
                        ),
                    )
                    .await;
            }
            Some(_) => {}
        }

        // AI-tool resolution (#196): workflow pin > repo default > global
        // default. The registry always contains the built-ins, so an unknown
        // name here is a config-drift error (validation catches it upfront);
        // a kind without an adapter could never signal completion, so both
        // are refused before any side effect (no worktree, no session row).
        let tool_name = crate::tool::resolve_tool_name(
            wf.tool.as_deref(),
            repo.tool.as_deref(),
            &self.settings.default_tool,
        );
        let Some(tool_profile) = self.settings.tools.get(&tool_name).cloned() else {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "resolved tool `{tool_name}` is not configured → add `[tools.{tool_name}]` or fix the `tool`/`default_tool` reference"
                    ),
                )
                .await;
        };
        if !tool_profile.kind.has_adapter() {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "tool `{tool_name}` (kind `{}`) has no completion-detection adapter yet → use a kind with an adapter",
                        tool_profile.kind.as_str()
                    ),
                )
                .await;
        }

        // Slack thread conversation continuity (#140, D-10): a follow-up
        // mention in the same thread is a *new* task, but resumes the prior
        // task's session via the tool's resume mechanism so context carries
        // over.
        // Decided here, before the retry-reuse block (which only fires for a
        // retry of *this* task and early-returns); the value threads into
        // `task/dispatch` below. Best-effort — any unmet precondition yields
        // `None` and falls back to a normal fresh dispatch, with no warning.
        // The worktree is created by the normal flow below (recreated fresh if
        // the prior one was discarded); only the session is reused. Gated on
        // the tool's capabilities (#196): a tool that cannot resume, or whose
        // native session id is never captured, always dispatches fresh.
        let tool_caps = tool_profile.capabilities();
        let resume_session_id = if tool_caps.resume && tool_caps.session_id_capture {
            self.thread_resume_session_id(&record, &agent_name)?
        } else {
            None
        };

        // Retry reuse (F-44): a surviving worktree + session resumes the
        // existing conversation instead of dispatching anew.
        let latest = self.db.latest_session(record.id)?;
        if let RetryPlan::ReuseSession {
            plugin, session_id, ..
        } = recovery::retry_plan(&record, latest.as_ref())
            && plugin == agent_name
            && let Some(state) = self.try_reattach(&plugin, &session_id).await
        {
            self.db.apply_event(
                record.id,
                TaskEvent::Dispatch,
                Some(serde_json::json!({
                    "kind": "dispatch", "reused_session": session_id, "plugin": plugin,
                })),
            )?;
            self.sessions
                .insert((plugin.clone(), session_id), record.id);
            self.stats.dispatched += 1;
            self.apply_agent_state(record.id, &plugin, state, None)
                .await?;
            return Ok(());
        }

        // Worktree: reuse a recorded one (retry without a live session), else
        // create fresh (F-20–F-22).
        //
        // The recorded path must still be **on disk** (#254). Cleanup removes
        // it at completion under `plan_cleanup = "immediate"`, and an operator
        // may remove it by hand, so a recorded path is not evidence of a usable
        // worktree; handing a missing directory to the agent fails the dispatch
        // for a reason the operator cannot act on. Re-creating renders the same
        // branch and path (both are pure functions of source + task id), and
        // the agent session survives it: Claude Code keys sessions by working
        // directory, storing them outside the worktree.
        let worktree_path = match (&record.worktree_path, &record.branch) {
            (Some(path), Some(_)) if Path::new(path).is_dir() => PathBuf::from(path),
            _ => {
                let location_template = repo
                    .worktree_location
                    .clone()
                    .unwrap_or_else(|| self.settings.location_template.clone());
                let request = CreateRequest {
                    repo_path: &repo.path,
                    repo_name: &repo.name,
                    source: &record.source,
                    task_id: &record.source_task_id,
                    branch_template: &self.settings.branch_template,
                    location_template: &location_template,
                    base_branch: None,
                    env: &self.settings.env,
                };
                match self.worktrees.create(&request) {
                    Ok(worktree) => {
                        let path = worktree.path.display().to_string();
                        self.db.set_worktree(record.id, &path, &worktree.branch)?;
                        worktree.path
                    }
                    // `AlreadyExists` means the rendered path is claimed by a
                    // worktree this task does not own — a leftover from an
                    // interrupted run, or an operator's own checkout. Say so
                    // and name the remedy instead of surfacing raw git stderr:
                    // re-creation (#254) already absorbs every case totsuka
                    // caused itself, so reaching here needs a human.
                    Err(WorktreeError::AlreadyExists { branch, path }) => {
                        return self
                            .fail_dispatch(
                                &record,
                                format!(
                                    "`{}` is already occupied (branch `{branch}`) but is not \
                                     recorded for this task; remove it — `git worktree remove {}`, \
                                     or the cleanup `totsuka doctor` offers, or plain `rm -rf` if \
                                     it is not a worktree at all — and retry",
                                    path.display(),
                                    path.display(),
                                ),
                            )
                            .await;
                    }
                    Err(e) => {
                        return self.fail_dispatch(&record, e.to_string()).await;
                    }
                }
            }
        };

        // Hook-capable agents (herdr 0.1.3: `resume_session` / `diagnostics_snapshot`)
        // receive a correlation `job_id` + a [`HookLaunchSpec`] so their Claude
        // Code hooks POST completion signals back (#131/#138). The job id's
        // `session_row` must exist *before* launch — it is injected into the
        // process and echoed by every hook — so the session row is reserved up
        // front and its native id filled in after `task/dispatch` returns.
        // Non-hook agents (orca / mock) take the unchanged path below.
        let hook_capable = self
            .plugins
            .agents
            .get(&agent_name)
            .map(|a| a.capabilities().hook_capable())
            .unwrap_or(false);
        let task = task_from_record(&record);
        let (job_id, hook_spec, reserved_row, visible_hook_context) = match hook_capable
            .then(|| self.hook_launch(&record.workflow))
            .flatten()
        {
            Some((settings_path, mut env)) => {
                let session_row = self.db.reserve_session(record.id, &agent_name)?;
                // Thread continuity (#140): tentatively stamp the resumed
                // Claude session id onto the fresh row so a later follow-up can
                // resume it even before this dispatch's SessionStart hook lands
                // (best-effort resilience). The hook's SessionStart reconciles
                // it against the real id (#138: a `--resume` may legitimately
                // change the id → warn + keep the newest).
                if let Some(sid) = &resume_session_id {
                    self.db.set_tool_session_id(session_row, sid)?;
                }
                let job_id = JobId::new(record.id, session_row);
                env.insert("TOTSUKA_JOB_ID".to_string(), job_id.to_string());
                // Invisible prompt context: the task-source's `instructions`
                // (0.1.5) plus the marker self-report convention ride the
                // `UserPromptSubmit` hook's `additionalContext` via this env
                // var — the model sees them, the pane shows only the task
                // body. Hook knowledge stays in core (H-01): source plugins
                // never compose marker instructions.
                let mut prompt_context = String::new();
                if let Some(instructions) = &task.instructions {
                    prompt_context.push_str(instructions);
                    prompt_context.push_str("\n\n");
                }
                prompt_context.push_str(&hooks::MARKER_SELF_REPORT_INSTRUCTION);
                // Context routing per tool capability (#196 Phase 3): a tool
                // without invisible injection (opencode — no UserPromptSubmit
                // additionalContext channel) gets the same instructions +
                // marker convention as *visible* extra_context instead, so
                // the completion contract still reaches the model up front.
                let visible_hook_context = if tool_profile.capabilities().invisible_injection {
                    env.insert("TOTSUKA_PROMPT_CONTEXT".to_string(), prompt_context);
                    None
                } else {
                    Some(prompt_context)
                };
                (
                    Some(job_id.to_string()),
                    Some(HookLaunchSpec { settings_path, env }),
                    Some(session_row),
                    visible_hook_context,
                )
            }
            None => (None, None, None, None),
        };

        // task/dispatch (F-31) → session id → persist (F-37) → subscribe (F-38).
        let agent = self.plugins.agents.get(&agent_name).expect("checked above");
        // Context routing: hook dispatches deliver everything invisibly via
        // `TOTSUKA_PROMPT_CONTEXT` above when the tool supports it; a tool
        // without invisible injection got the same content as
        // `visible_hook_context` instead. Non-hook dispatches (orca / mock)
        // have no invisible channel — fall back to the task's instructions as
        // visible string extra_context (no marker convention: non-hook agents
        // don't report completion through hooks).
        let extra_context = match (&hook_spec, visible_hook_context) {
            // Hook dispatch, tool without invisible injection: the context is
            // delivered visibly (see above).
            (Some(_), Some(ctx)) => Some(serde_json::Value::String(ctx)),
            (Some(_), None) => None,
            (None, _) => task.instructions.clone().map(serde_json::Value::String),
        };
        let mode = execution_mode(&record.mode);
        // Fully-resolved tool launch (#196): the argv (base command, mode
        // flags, hook settings, resume id) is assembled in core from the
        // resolved profile; the plugin launches it verbatim. The deprecated
        // `hook` spec rides along for plugins predating protocol 0.2.3.
        let tool_launch = tool_profile.launch_spec(&LaunchInputs {
            plan: mode == plugin_protocol::methods::ExecutionMode::Plan,
            settings_path: hook_spec.as_ref().map(|h| h.settings_path.as_str()),
            resume_session_id: resume_session_id.as_deref(),
            env: hook_spec
                .as_ref()
                .map(|h| h.env.clone())
                .unwrap_or_default(),
        });
        let params = TaskDispatchParams {
            task,
            worktree_path: worktree_path.display().to_string(),
            mode,
            extra_context,
            job_id,
            resume_session_id,
            hook: hook_spec,
            tool_launch,
        };
        let dispatched: TaskDispatchResult = match agent.call(method::TASK_DISPATCH, &params).await
        {
            Ok(result) => result,
            Err(e) => {
                // Roll back the pre-dispatch session reservation (hook path) so
                // a failed dispatch never leaves an empty-id row for retry /
                // recovery to re-attach to.
                if let Some(row) = reserved_row
                    && let Err(err) = self.db.delete_session(row)
                {
                    tracing::warn!(
                        task_id = record.id,
                        "failed to roll back reserved session row: {err}"
                    );
                }
                return self.fail_dispatch(&record, e.to_string()).await;
            }
        };
        // Persist the native session id (F-37): fill the reserved hook row, or
        // append a fresh row on the non-hook path.
        match reserved_row {
            Some(row) => self.db.set_session_native_id(row, &dispatched.session_id)?,
            None => {
                self.db
                    .record_session(record.id, &agent_name, &dispatched.session_id)?;
            }
        }
        self.db.apply_event(
            record.id,
            TaskEvent::Dispatch,
            Some(serde_json::json!({
                "kind": "dispatch", "plugin": agent_name, "session_id": dispatched.session_id,
            })),
        )?;
        self.sessions.insert(
            (agent_name.clone(), dispatched.session_id.clone()),
            record.id,
        );
        self.stats.dispatched += 1;
        tracing::info!(
            task_id = record.id,
            agent = %agent_name,
            session_id = %dispatched.session_id,
            worktree = %worktree_path.display(),
            "dispatched"
        );

        let subscribe = plugin_protocol::methods::StateSubscribeParams {
            session_id: dispatched.session_id.clone(),
        };
        let subscribe_error = match agent
            .call::<_, Value>(method::STATE_SUBSCRIBE, &subscribe)
            .await
        {
            Ok(_) => None,
            Err(e) => {
                // No stream means the task could never progress (the loop
                // would hold its slot forever). Best-effort cancel, then fail.
                let cancel = plugin_protocol::methods::TaskCancelParams {
                    session_id: dispatched.session_id.clone(),
                };
                let _ = agent.call::<_, Value>(method::TASK_CANCEL, &cancel).await;
                Some(e.to_string())
            }
        };
        if let Some(e) = subscribe_error {
            self.drop_task_sessions(record.id);
            return self
                .fail_dispatch(
                    &record,
                    format!("state/subscribe failed: {e} → dispatch cancelled; fix the agent plugin and `task retry`"),
                )
                .await;
        }
        Ok(())
    }

    /// The Claude session id a follow-up task should resume (Slack thread
    /// conversation continuity, #140), or `None` to dispatch fresh.
    ///
    /// Returns `Some(claude_sid)` only when **all** hold: the agent declares the
    /// `resume_session` capability, `record` carries a `thread_key`, a *prior*
    /// task in the same workflow+thread exists (E-09: matched on `workflow`, and
    /// `record` excluded so it never resolves to itself), and that prior task's
    /// latest session has a non-empty Claude session id (established by the
    /// SessionStart hook, #138). Any miss yields `None` — conversation
    /// continuity is best-effort and never hard-fails a dispatch.
    ///
    /// E-09: the reply destination is always the *new* task's own
    /// `source_task_id` (task_id-origin routing via `job_id`); nothing here — or
    /// anywhere — derives a destination from a Claude session id, so a resumed
    /// session can never mis-route a reply into the prior task's thread.
    fn thread_resume_session_id(
        &self,
        record: &TaskRecord,
        agent_name: &str,
    ) -> Result<Option<String>, EngineError> {
        let Some(agent) = self.plugins.agents.get(agent_name) else {
            return Ok(None);
        };
        let Some(thread_key) = record.thread_key.as_deref() else {
            return Ok(None);
        };
        if !agent.capabilities().resume_session {
            return Ok(None);
        }
        let Some(prior) = self
            .db
            .find_by_thread_key(&record.workflow, thread_key, record.id)?
        else {
            return Ok(None);
        };
        let resume = self
            .db
            .latest_session(prior.id)?
            .and_then(|s| s.tool_session_id)
            .filter(|sid| !sid.is_empty());
        if let Some(sid) = &resume {
            tracing::info!(
                task_id = record.id,
                prior_task_id = prior.id,
                tool_session_id = %sid,
                "resuming the prior task's tool session for thread continuity (#140)"
            );
        }
        Ok(resume)
    }

    /// Try to re-attach to a session for retry reuse; `None` means dispatch
    /// fresh instead (lost session / attach failure).
    async fn try_reattach(&self, plugin: &str, session_id: &str) -> Option<AgentState> {
        use crate::ports::agent_session::AgentSession;
        let attacher = crate::adapters::PluginAgentSession::new(&self.plugins.agents);
        match attacher.attach(plugin, session_id).await {
            Ok(AttachOutcome::Attached(state)) => Some(state),
            Ok(AttachOutcome::Lost) => None,
            Err(e) => {
                tracing::warn!(plugin, session_id, "retry re-attach failed: {e}");
                None
            }
        }
    }

    /// Release the slot a task holds, if it holds one (per-task ledger).
    fn release_slot(&mut self, task_id: i64) {
        if let Some((repo, agent)) = self.slot_holders.remove(&task_id) {
            self.slots.release(&repo, &agent);
        }
    }

    /// Drop a finished task's session routes so long-running `--watch` does
    /// not accumulate stale `(plugin, session_id)` entries.
    fn drop_task_sessions(&mut self, task_id: i64) {
        self.sessions.retain(|_, &mut id| id != task_id);
    }

    /// Fail a task during dispatch: release its slot, record the reason,
    /// notify (F-90).
    async fn fail_dispatch(
        &mut self,
        record: &TaskRecord,
        reason: String,
    ) -> Result<(), EngineError> {
        tracing::error!(task_id = record.id, "dispatch failed: {reason}");
        self.release_slot(record.id);
        self.agent_output.remove(&record.id);
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": "dispatch", "reason": reason })),
        )?;
        self.stats.failed += 1;
        self.write_back_status(record, false).await;
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Failed,
            record,
            Some(reason),
        );
        Ok(())
    }

    /// Handle one plugin event.
    async fn on_event(&mut self, event: PluginEvent) -> Result<(), EngineError> {
        match event {
            PluginEvent::State(plugin, note) => {
                let Some(&task_id) = self
                    .sessions
                    .get(&(plugin.clone(), note.session_id.clone()))
                else {
                    tracing::debug!(plugin, session_id = %note.session_id, "notification for unknown session");
                    return Ok(());
                };
                if let Some(chunk) = &note.log_chunk {
                    tracing::debug!(task_id, "agent log: {chunk}");
                    // Accumulate the streamed output; it is the `output = source`
                    // publish artifact (F-07) and the PR body `{summary}`.
                    let buf = self.agent_output.entry(task_id).or_default();
                    buf.push_str(chunk);
                    if !chunk.ends_with('\n') {
                        buf.push('\n');
                    }
                }
                self.apply_agent_state(task_id, &plugin, note.state, note.log_chunk)
                    .await
            }
            PluginEvent::Closed(plugin) => self.on_plugin_closed(&plugin).await,
            // A normalized Claude Code hook signal from the UDS receiver (#136):
            // idempotent record → task resolution → state transition →
            // verification → output (#138, `run::hooks`).
            PluginEvent::HookSignal(signal) => self.on_signal(signal).await,
            // A control-UDS focus request (F-94): resolve, ask the agent
            // plugin, and answer the waiting adapter. A dropped receiver just
            // means the caller gave up (timeout) — not an engine error.
            PluginEvent::Focus { task_id, respond } => {
                let outcome = self.focus_task(task_id).await;
                let _ = respond.send(outcome);
                Ok(())
            }
            // A pushed task (`task/submit`, 0.1.6): persist, ack only after
            // the commit, then run repo selection so the loop's dispatch pass
            // can pick the task up. A persistence error answers the plugin
            // with the retryable INTERNAL_ERROR and still propagates — DB
            // failures are run-fatal, and this path must not be forgiving.
            PluginEvent::TaskSubmit {
                source,
                task,
                respond,
            } => match self.on_task_submit(source, task) {
                Ok(result) => {
                    let _ = respond.send(Ok(result));
                    self.select_repos().await
                }
                Err(e) => {
                    let _ = respond.send(Err(jsonrpc::Error::new(
                        plugin_protocol::error_code::INTERNAL_ERROR,
                        format!("failed to persist task: {e} → retry with backoff"),
                    )));
                    Err(e)
                }
            },
        }
    }

    /// Ingest one pushed task (`task/submit`, 0.1.6): normalize and
    /// defensively re-match the workflow. Returns the final ack; `Err` is a
    /// persistence failure (retryable for the plugin, fatal for the run).
    fn on_task_submit(
        &mut self,
        source: String,
        mut task: Task,
    ) -> Result<TaskSubmitResult, EngineError> {
        // Workflow matching and the ingest key use the `[plugins.<name>]`
        // key, not the plugin's own notion of its source name.
        task.source = source;
        let workflows = self.settings.workflows.clone();
        let Some(wf) = match_workflow(&workflows, &task) else {
            return Ok(TaskSubmitResult {
                status: TaskSubmitStatus::Rejected,
                reason: Some(format!(
                    "no workflow matches source `{}` (status: {:?}, labels: {:?}) → \
                     add a [[workflows]] entry or fix its trigger",
                    task.source, task.status, task.labels
                )),
            });
        };
        let (id, outcome) = self.ingest_task(wf, &task)?;
        match outcome {
            IngestOutcome::Created => {
                self.stats.submitted += 1;
                tracing::info!(task_id = id, workflow = %wf.name, title = %task.title, "task submitted");
            }
            IngestOutcome::Reopened => {
                tracing::info!(
                    task_id = id, workflow = %wf.name,
                    "conversation reopened by a new message"
                );
            }
            IngestOutcome::Appended => {
                tracing::info!(
                    task_id = id, workflow = %wf.name,
                    "message appended to a conversation still in progress"
                );
            }
            IngestOutcome::Duplicate => {
                tracing::debug!(task_id = id, "message already ingested; dropped");
            }
        }
        Ok(TaskSubmitResult {
            status: outcome.ack(),
            reason: None,
        })
    }

    /// Advance a task's state machine to match the agent's reported state
    /// (F-32), handling slots (F-45), notifier delivery (F-35/F-90), and
    /// terminal processing.
    async fn apply_agent_state(
        &mut self,
        task_id: i64,
        agent_plugin: &str,
        state: AgentState,
        log_chunk: Option<String>,
    ) -> Result<(), EngineError> {
        let record = self
            .db
            .get_task(task_id)?
            .ok_or(StateError::NotFound(task_id))?;
        // Only tasks in the agent-driven pipeline react to agent states; a
        // stray/late notification for a queued, pending, or finished task is
        // ignored rather than corrupting the state machine.
        if !matches!(
            record.state,
            TaskState::Dispatched
                | TaskState::Running
                | TaskState::WaitingInput
                | TaskState::Publishing
        ) {
            return Ok(());
        }
        let repo = record.repo.clone().unwrap_or_default();
        let detail = serde_json::json!({ "kind": "agent_state", "state": state });

        match state {
            AgentState::Idle => {}
            AgentState::Running => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start],
                    TaskState::WaitingInput => &[TaskEvent::ResumeInput],
                    _ => &[],
                };
                let resuming = record.state == TaskState::WaitingInput;
                for &event in events {
                    self.db.apply_event(task_id, event, Some(detail.clone()))?;
                }
                if resuming {
                    // F-45: resuming re-acquires a slot. Reality wins over the
                    // cap: the agent *is* running, so a full tier only logs —
                    // but the ledger stays empty, so this task's completion
                    // will not release a slot another task holds.
                    if self.slots.acquire(&repo, agent_plugin) {
                        self.slot_holders
                            .insert(task_id, (repo.clone(), agent_plugin.to_string()));
                    } else {
                        tracing::warn!(
                            task_id,
                            "resumed task exceeds a concurrency cap temporarily"
                        );
                    }
                }
            }
            AgentState::WaitingInput => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start, TaskEvent::WaitInput],
                    TaskState::Running => &[TaskEvent::WaitInput],
                    _ => &[],
                };
                if !events.is_empty() {
                    for &event in events {
                        self.db.apply_event(task_id, event, Some(detail.clone()))?;
                    }
                    // F-45: a waiting task frees its slot.
                    self.release_slot(task_id);
                    tracing::info!(task_id, "agent is waiting for input (F-35)");
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::WaitingInput,
                        &record,
                        log_chunk,
                    );
                }
            }
            AgentState::Done => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start, TaskEvent::BeginPublish],
                    TaskState::Running => &[TaskEvent::BeginPublish],
                    TaskState::WaitingInput => &[TaskEvent::ResumeInput, TaskEvent::BeginPublish],
                    _ => &[],
                };
                for &event in events {
                    // Persist the accumulated agent output on the BeginPublish
                    // transition, so a crash before finalize can recover the
                    // artifact from the audit log (source publish / PR summary).
                    let event_detail = if event == TaskEvent::BeginPublish {
                        serde_json::json!({
                            "kind": "agent_state", "state": state,
                            "publish_artifact": self.agent_output.get(&task_id),
                        })
                    } else {
                        detail.clone()
                    };
                    self.db.apply_event(task_id, event, Some(event_detail))?;
                }
                self.finalize_success(&record).await?;
            }
            AgentState::Failed => {
                self.db.apply_event(
                    task_id,
                    TaskEvent::Fail,
                    Some(serde_json::json!({ "kind": "agent_state", "state": "failed" })),
                )?;
                self.release_slot(task_id);
                self.drop_task_sessions(task_id);
                self.agent_output.remove(&task_id);
                self.stats.failed += 1;
                self.write_back_status(&record, false).await;
                // The worktree is kept for `task retry` (F-44).
                notify_all(
                    &self.plugins.notifiers,
                    NotifierEvent::Failed,
                    &record,
                    log_chunk,
                );
                tracing::warn!(task_id, "task failed");
            }
        }
        Ok(())
    }

    /// Terminal processing for a task whose agent finished: run the workflow's
    /// output policy (#65), then either complete or fail.
    ///
    /// `record` is in a pre-`Complete` pipeline state (usually `Publishing`).
    /// On a **publishing failure** the task is failed but its worktree and
    /// commits are kept, so `task retry` can resume from here (issue #65).
    async fn finalize_success(&mut self, record: &TaskRecord) -> Result<(), EngineError> {
        // A finished task whose workflow vanished from config still holds the
        // agent's commits; treat it as a recoverable publish failure rather
        // than silently completing and deleting the worktree (never confuse a
        // missing workflow with an explicit `output = none`).
        let Some(policy) = workflows_by_name(&self.settings.workflows)
            .get(record.workflow.as_str())
            .map(|w| w.output)
        else {
            return self
                .fail_publish(
                    record,
                    format!(
                        "workflow `{}` is no longer configured → restore it (worktree and commits are kept) or `totsuka task cancel {}`",
                        record.workflow, record.id
                    ),
                )
                .await;
        };

        match self.execute_output_policy(record, policy).await {
            Ok(pr_url) => {
                // Success: on_success write-back (F-84) → Complete → cleanup.
                self.write_back_status(record, true).await;
                self.db.apply_event(
                    record.id,
                    TaskEvent::Complete,
                    Some(serde_json::json!({
                        "kind": "publish",
                        "policy": policy_str(policy),
                        "pr_url": pr_url,
                    })),
                )?;
                self.release_slot(record.id);
                self.drop_task_sessions(record.id);
                self.agent_output.remove(&record.id);
                self.stats.done += 1;
                self.cleanup_worktree(record.id).await?;
                notify_all(&self.plugins.notifiers, NotifierEvent::Done, record, None);
                tracing::info!(task_id = record.id, "task done");
                Ok(())
            }
            Err(reason) => self.fail_publish(record, reason).await,
        }
    }

    /// Fail a task at the publishing stage, KEEPING its worktree, commits and
    /// session so `task retry` can resume (issue #65). The accumulated agent
    /// output is dropped so a retry re-captures fresh output (no duplication in
    /// the PR body). The source status is intentionally left unchanged: a
    /// recoverable publish failure must not flap the source task to
    /// `on_failure` and back on the next successful retry.
    async fn fail_publish(
        &mut self,
        record: &TaskRecord,
        reason: String,
    ) -> Result<(), EngineError> {
        tracing::error!(task_id = record.id, "output policy failed: {reason}");
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": "publish", "reason": reason.clone() })),
        )?;
        self.release_slot(record.id);
        self.agent_output.remove(&record.id);
        self.stats.failed += 1;
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Failed,
            record,
            Some(reason),
        );
        Ok(())
    }

    /// Execute the output policy for a finished task. Returns the PR URL (for
    /// `pull_request`) or `None`, or an error reason on failure.
    async fn execute_output_policy(
        &self,
        record: &TaskRecord,
        policy: OutputPolicy,
    ) -> Result<Option<String>, String> {
        match policy {
            OutputPolicy::None => Ok(None),
            OutputPolicy::Source => self.publish_to_source(record).await.map(|()| None),
            OutputPolicy::PullRequest => {
                // F-82: plan mode must never push (config validation blocks
                // this, but never trust it at the publish point).
                if record.mode == "plan" {
                    return Err(
                        "plan mode must not open a pull request → use output = source or none"
                            .to_string(),
                    );
                }
                self.open_pull_request(record).await.map(Some)
            }
        }
    }

    /// The agent artifact persisted on the most recent `BeginPublish`
    /// transition (the `publish_artifact` field of its event `detail`), if any.
    /// Used to recover the artifact across a restart.
    fn persisted_artifact(&self, task_id: i64) -> Result<Option<String>, EngineError> {
        Ok(self
            .db
            .list_events(task_id)?
            .into_iter()
            .rev()
            .find_map(|e| {
                e.detail
                    .as_ref()
                    .and_then(|d| d.get("publish_artifact"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }))
    }

    /// `output = source` (F-07): hand the accumulated artifact to the task
    /// source plugin's `result/publish`.
    async fn publish_to_source(&self, record: &TaskRecord) -> Result<(), String> {
        let source = self
            .plugins
            .sources
            .get(&record.source)
            .ok_or_else(|| format!("task source `{}` is not launched", record.source))?;
        // The accumulated agent output is the artifact. When it is genuinely
        // unavailable — an agent that finished while the orchestrator was fully
        // down streamed nothing to anyone — publish an honest note rather than
        // pretend a result exists.
        let content = self
            .agent_output
            .get(&record.id)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "_totsuka: task `{}` completed, but the agent output was not captured (recovered run)._",
                    record.title
                )
            });
        let params = ResultPublishParams {
            task_id: record.source_task_id.clone(),
            content,
            format: Some("markdown".to_string()),
        };
        source
            .call::<_, Value>(method::RESULT_PUBLISH, &params)
            .await
            .map(|_| ())
            .map_err(|e| format!("result/publish failed: {e}"))
    }

    /// `output = pull_request` (F-86): verify commits exist, push the branch,
    /// open the PR. The Orchestrator — never the agent — pushes.
    async fn open_pull_request(&self, record: &TaskRecord) -> Result<String, String> {
        let (Some(path), Some(branch)) = (&record.worktree_path, &record.branch) else {
            return Err("no worktree/branch recorded → cannot push".to_string());
        };
        let worktree_path = PathBuf::from(path);

        // Pre-condition: the agent must have committed something (F-86).
        match self.worktrees.has_commits_to_publish(&worktree_path) {
            Ok(true) => {}
            Ok(false) => {
                return Err(
                    "the agent produced no commits to publish → nothing to open a PR for"
                        .to_string(),
                );
            }
            Err(e) => return Err(format!("could not inspect commits: {e}")),
        }

        self.worktrees
            .push_branch(&worktree_path, branch)
            .map_err(|e| format!("git push failed: {e}"))?;

        let summary = self
            .agent_output
            .get(&record.id)
            .cloned()
            .unwrap_or_default();
        let ctx = PrContext {
            title: &record.title,
            url: record.url.as_deref().unwrap_or(""),
            source: &record.source,
            task_id: &record.source_task_id,
            summary: &summary,
        };
        let req = PrRequest {
            worktree_path,
            head_branch: branch.clone(),
            title: render_template(&self.settings.pr_title_template, &ctx),
            body: render_template(&self.settings.pr_body_template, &ctx),
        };
        // `PrError` already carries a "pull-request creation failed" prefix;
        // return it as-is rather than doubling the prefix.
        self.pr_creator.create_pr(&req).map_err(|e| e.to_string())
    }

    /// Apply the workflow's `on_success`/`on_failure` status transition on the
    /// source (F-84). Failures are logged, never fatal: the task outcome is
    /// already decided.
    async fn write_back_status(&self, record: &TaskRecord, success: bool) {
        let workflows = workflows_by_name(&self.settings.workflows);
        let Some(wf) = workflows.get(record.workflow.as_str()) else {
            return;
        };
        let action = if success {
            wf.on_success.as_ref()
        } else {
            wf.on_failure.as_ref()
        };
        let Some(status) = action.and_then(|a| a.set_status.clone()) else {
            return;
        };
        let Some(source) = self.plugins.sources.get(&record.source) else {
            tracing::warn!(
                task_id = record.id,
                "cannot write back status: source plugin not launched"
            );
            return;
        };
        let params = TaskUpdateStatusParams {
            task_id: record.source_task_id.clone(),
            status: status.clone(),
        };
        match source
            .call::<_, Value>(method::TASK_UPDATE_STATUS, &params)
            .await
        {
            Ok(_) => {
                tracing::info!(task_id = record.id, status = %status, "source status updated (F-84)");
            }
            Err(e) => {
                tracing::warn!(task_id = record.id, "task/update_status failed: {e}");
            }
        }
    }

    /// Apply the cleanup policy to a finished task's worktree, in three stages
    /// (#210): decide → release the pane → remove. The pane is released only
    /// on a `Remove` decision, so `Retained`/`DirtySkipped` worktrees keep
    /// their pane as the human's entry point (F-23/F-85).
    async fn cleanup_worktree(&mut self, task_id: i64) -> Result<(), EngineError> {
        // Re-fetch: `finished_at` was just set by the terminal transition.
        let Some(record) = self.db.get_task(task_id)? else {
            return Ok(());
        };
        let (Some(path), Some(branch), Some(repo_name)) =
            (&record.worktree_path, &record.branch, &record.repo)
        else {
            return Ok(());
        };
        // Already removed (earlier run / manual cleanup): nothing to do. The
        // task will never be swept again, so drop its release memo too.
        if !Path::new(path).exists() {
            self.released_panes.remove(&task_id);
            return Ok(());
        }
        // Owned copy: `release_pane` below needs `&mut self`, which a borrow
        // into `self.settings` would block.
        let Some(repo_path) = self
            .settings
            .repos
            .iter()
            .find(|r| &r.name == repo_name)
            .map(|r| r.path.clone())
        else {
            return Ok(());
        };
        let policy = if record.mode == "plan" {
            self.settings.cleanup_plan
        } else {
            self.settings.cleanup_implement
        };
        let now = self.clock.now_rfc3339();
        let decision = match self.worktrees.decide_cleanup(
            Path::new(path),
            policy,
            record.finished_at.as_deref(),
            &now,
        ) {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(task_id, "worktree cleanup failed: {e}");
                return Ok(());
            }
        };
        match decision {
            CleanupDecision::Retain => {
                // Expected under retention/manual policies; the sweep re-checks
                // periodically, so keep this quiet.
                tracing::debug!(task_id, "worktree retained per policy");
                return Ok(());
            }
            CleanupDecision::Dirty => {
                // Data-loss guard (F-23): keep the worktree AND its pane — the
                // pane is the human's way in to the uncommitted work.
                tracing::info!(
                    task_id,
                    outcome = ?CleanupOutcome::DirtySkipped,
                    "worktree cleanup"
                );
                return Ok(());
            }
            CleanupDecision::Remove => {}
        }
        // The worktree is going away → its pane has nothing left to show.
        // Close it before the removal so the pane's lifetime tracks the
        // worktree's; at most once per task (a removal that keeps failing
        // must not re-release every sweep).
        if !self.released_panes.contains(&task_id) {
            self.release_pane(&record).await;
        }
        match self.worktrees.remove(&repo_path, Path::new(path), branch) {
            Ok(CleanupOutcome::DirtySkipped) => {
                // Turned dirty between decision and removal: the pane is
                // already gone, but data loss (irreversible) outranks a lost
                // pane (minor). The sweep retries the removal later.
                tracing::warn!(
                    task_id,
                    worktree = %path,
                    "worktree turned dirty after its pane was released; kept (F-23)"
                );
            }
            Ok(outcome) => {
                // Removed: this task is done being swept — drop its release
                // memo so `released_panes` stays bounded by the worktrees
                // still awaiting removal, not by every task a long `--watch`
                // run ever completed (same hygiene as `drop_task_sessions`).
                self.released_panes.remove(&task_id);
                tracing::info!(task_id, ?outcome, "worktree cleanup");
            }
            Err(e) => {
                tracing::warn!(task_id, "worktree cleanup failed: {e}");
            }
        }
        Ok(())
    }

    /// Release (close) a finished task's pane via `session/release` (#210).
    /// Best-effort: every failure only logs — a pane that could not be
    /// released must never block the worktree removal (an orphaned pane is
    /// `doctor`'s job, #211). Marks the task released once the RPC answered
    /// (whatever `released` says — `false` means "already gone or refused",
    /// both final) or when release is impossible for this run; a transport
    /// error leaves it unmarked so the next sweep retries.
    async fn release_pane(&mut self, record: &TaskRecord) {
        let session = match self.db.latest_session(record.id) {
            Ok(Some(session)) => session,
            Ok(None) => {
                // Never dispatched → no pane to release.
                self.released_panes.insert(record.id);
                return;
            }
            Err(e) => {
                tracing::warn!(
                    task_id = record.id,
                    "cannot resolve session for pane release: {e}"
                );
                return;
            }
        };
        let Some(agent) = self.plugins.agents.get(&session.plugin) else {
            // The owning plugin is not launched this run; that cannot change
            // until restart, so do not retry every sweep.
            tracing::debug!(
                task_id = record.id,
                plugin = %session.plugin,
                "pane release skipped: agent plugin not launched"
            );
            self.released_panes.insert(record.id);
            return;
        };
        if !agent.capabilities().pane_control {
            // No pane to control (e.g. orca): nothing to release, ever.
            self.released_panes.insert(record.id);
            return;
        }
        let params = SessionReleaseParams {
            session_id: session.session_id.clone(),
            // Identity guard against pane-id reuse: the worktree path is
            // unique per task and the DB is its source of truth. The label is
            // plugin-internal, so the orchestrator never composes one.
            expect_cwd: record.worktree_path.clone(),
            expect_label: None,
        };
        match agent
            .call::<_, SessionReleaseResult>(method::SESSION_RELEASE, &params)
            .await
        {
            Ok(result) => {
                self.released_panes.insert(record.id);
                if result.released {
                    tracing::info!(task_id = record.id, "pane released before worktree removal");
                } else {
                    tracing::debug!(
                        task_id = record.id,
                        "pane not released (already gone or identity mismatch)"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(task_id = record.id, "session/release failed: {e}");
            }
        }
    }

    /// An agent plugin's process exited (§5.3): fail its in-flight tasks; the
    /// Orchestrator itself keeps running.
    async fn on_plugin_closed(&mut self, plugin: &str) -> Result<(), EngineError> {
        tracing::warn!(plugin, "agent plugin process exited");
        let affected: Vec<i64> = self
            .sessions
            .iter()
            .filter(|((p, _), _)| p == plugin)
            .map(|(_, &task_id)| task_id)
            .collect();
        // The plugin is gone: its session routes can never fire again.
        self.sessions.retain(|(p, _), _| p != plugin);
        for task_id in affected {
            let Some(record) = self.db.get_task(task_id)? else {
                continue;
            };
            if record.state.is_terminal() {
                continue;
            }
            self.db.apply_event(
                task_id,
                TaskEvent::Fail,
                Some(serde_json::json!({ "kind": "plugin_crash", "plugin": plugin })),
            )?;
            self.release_slot(task_id);
            self.agent_output.remove(&task_id);
            self.stats.failed += 1;
            self.write_back_status(&record, false).await;
            notify_all(
                &self.plugins.notifiers,
                NotifierEvent::Failed,
                &record,
                Some(format!("agent plugin `{plugin}` crashed")),
            );
        }
        Ok(())
    }

    /// Report what a run would do, with zero side effects (§5.1 `--dry-run`).
    ///
    /// Since 0.2.0 every task_source is push-only: nothing is fetched ahead
    /// of time, so there is nothing to preview. Always returns an empty
    /// list; the signature and [`DryRunEntry`] type are kept for the CLI's
    /// existing `--dry-run` contract.
    pub async fn dry_run(&self) -> Result<Vec<DryRunEntry>, EngineError> {
        for wf in &self.settings.workflows {
            tracing::info!(
                workflow = %wf.name,
                source = %wf.source,
                "push source (task/submit) cannot be previewed: nothing is fetched ahead of time"
            );
        }
        Ok(Vec::new())
    }
}

/// Route one plugin-initiated request (P→O, 0.1.6) to the engine loop.
///
/// `task/submit` is parsed and budgeted here; anything else is answered
/// `METHOD_NOT_FOUND` immediately. The ack await is spawned off the caller's
/// loop so one slow ingest never delays the next submission's parsing (per-
/// source ordering is already fixed by the inline event-channel send).
fn forward_submit(
    source: &str,
    request: IncomingRequest,
    tx: &mpsc::UnboundedSender<PluginEvent>,
    budget: &Arc<Semaphore>,
) {
    use plugin_protocol::error_code;
    if request.method != method::TASK_SUBMIT {
        request.responder.err(jsonrpc::Error::new(
            error_code::METHOD_NOT_FOUND,
            format!("unknown plugin-initiated method: {}", request.method),
        ));
        return;
    }
    let params: TaskSubmitParams = match request
        .params
        .clone()
        .map(serde_json::from_value)
        .transpose()
    {
        Ok(Some(params)) => params,
        Ok(None) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                "task/submit requires params → send { \"task\": { … } }",
            ));
            return;
        }
        Err(e) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                format!("malformed task/submit params: {e}"),
            ));
            return;
        }
    };
    // Backpressure: a bounded in-flight budget per plugin. Exhaustion is the
    // retryable SUBMIT_OVERLOADED, never a dropped request.
    let Ok(permit) = budget.clone().try_acquire_owned() else {
        request.responder.err(jsonrpc::Error::new(
            error_code::SUBMIT_OVERLOADED,
            "task/submit in-flight budget exhausted → retry with backoff",
        ));
        return;
    };
    let (otx, orx) = tokio::sync::oneshot::channel();
    if tx
        .send(PluginEvent::TaskSubmit {
            source: source.to_string(),
            task: params.task,
            respond: otx,
        })
        .is_err()
    {
        request.responder.err(jsonrpc::Error::new(
            error_code::NOT_ACCEPTING,
            "orchestrator is not accepting submissions → retry with backoff",
        ));
        return;
    }
    let responder = request.responder;
    tokio::spawn(async move {
        let _permit = permit;
        match orx.await {
            Ok(Ok(result)) => match serde_json::to_value(&result) {
                Ok(value) => responder.ok(value),
                Err(e) => responder.err(jsonrpc::Error::new(
                    error_code::INTERNAL_ERROR,
                    format!("failed to encode task/submit ack: {e}"),
                )),
            },
            Ok(Err(error)) => responder.err(error),
            // The engine dropped the responder without answering: it is
            // shutting down before persisting. Retryable, never final.
            Err(_) => responder.err(jsonrpc::Error::new(
                error_code::NOT_ACCEPTING,
                "orchestrator is shutting down → retry with backoff",
            )),
        }
    });
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
    for plugin in notifiers.values() {
        if let Err(e) = plugin.notify(method::NOTIFY, &params) {
            tracing::warn!(plugin = %plugin.name(), "notify delivery failed (ignored, F-93): {e}");
        }
    }
}

/// Index workflows by name.
fn workflows_by_name(workflows: &[Workflow]) -> HashMap<&str, &Workflow> {
    workflows.iter().map(|w| (w.name.as_str(), w)).collect()
}

/// Reconstruct the normalized [`Task`] from a stored record: the full ingest
/// payload when present, else a minimal task from the columns.
fn task_from_record(record: &TaskRecord) -> Task {
    record
        .source_payload
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| Task {
            id: record.source_task_id.clone(),
            source: record.source.clone(),
            title: record.title.clone(),
            body: None,
            repo_hint: None,
            labels: Vec::new(),
            priority: record.priority,
            status: None,
            url: record.url.clone(),
            assignee: None,
            thread_key: None,
            message_key: None,
            instructions: None,
        })
}

/// The stable mode string persisted in `tasks.mode`.
fn mode_str(mode: WorkflowMode) -> &'static str {
    match mode {
        WorkflowMode::Plan => "plan",
        WorkflowMode::Implement => "implement",
    }
}

/// The output-policy name, for audit `detail`.
fn policy_str(policy: OutputPolicy) -> &'static str {
    match policy {
        OutputPolicy::PullRequest => "pull_request",
        OutputPolicy::Source => "source",
        OutputPolicy::None => "none",
    }
}

/// Parse a persisted mode string into the dispatch execution mode (F-31).
fn execution_mode(mode: &str) -> ExecutionMode {
    if mode == "plan" {
        ExecutionMode::Plan
    } else {
        ExecutionMode::Implement
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XDG bases resolved from a fake environment, mirroring what the CLI
    /// hands to [`settings_from_config`].
    fn test_paths(pairs: &[(&str, &str)]) -> Paths {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        Paths::from_env(|k| map.get(k).cloned()).unwrap()
    }

    #[test]
    fn settings_interpret_limits_and_cleanup() {
        let cfg = RootConfig::from_toml_str(
            r#"
max_concurrency = 2

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3

[[repositories]]
name = "web"
path = "~/repos/web"
max_concurrency = 1

[worktree]
cleanup = "immediate"
plan_cleanup = { retention_days = 2 }
"#,
        )
        .unwrap();
        let env = HashMap::from([("HOME".to_string(), "/home/t".to_string())]);
        let settings =
            settings_from_config(&cfg, &env, &test_paths(&[("HOME", "/home/t")])).unwrap();

        assert_eq!(settings.limits.global, 2);
        assert_eq!(settings.limits.per_repo.get("web"), Some(&1));
        assert_eq!(settings.limits.per_agent.get("herdr"), Some(&3));
        assert_eq!(settings.repos[0].path, PathBuf::from("/home/t/repos/web"));
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Immediate);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::RetentionDays(2));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        let paths = test_paths(&[("HOME", "/home/t"), ("XDG_STATE_HOME", "/xdg/state")]);
        let settings = settings_from_config(&cfg, &HashMap::new(), &paths).unwrap();
        assert_eq!(settings.limits.global, DEFAULT_GLOBAL_CONCURRENCY);
        // Implement keeps work (manual); plan cleans immediately (F-85).
        // #210 deliberately did NOT change these defaults.
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Manual);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::Immediate);
        assert_eq!(
            settings.location_template,
            "/xdg/state/totsuka/worktrees/{repo_name}/{branch}"
        );
        assert_eq!(settings.worktree_sweep_interval, WORKTREE_SWEEP_INTERVAL);
    }

    /// The default worktree location must resolve on a machine with no
    /// `XDG_STATE_HOME` (the macOS norm). It used to be the literal template
    /// `"${XDG_STATE_HOME}/totsuka/worktrees/..."`, which `expand_env` rejects
    /// when the variable is unset — `totsuka run` started fine and then failed
    /// *every* dispatch at worktree creation.
    #[test]
    fn default_location_falls_back_to_home_without_xdg_state_home() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        // Deliberately no XDG_STATE_HOME, in `paths` or in the expansion env.
        let env = HashMap::from([("HOME".to_string(), "/home/t".to_string())]);
        let settings =
            settings_from_config(&cfg, &env, &test_paths(&[("HOME", "/home/t")])).unwrap();

        assert_eq!(
            settings.location_template,
            "/home/t/.local/state/totsuka/worktrees/{repo_name}/{branch}"
        );
        // The template no longer carries a `${ENV}` reference, so rendering
        // cannot fail on an unset variable.
        assert!(!settings.location_template.contains("${"));
    }

    /// An operator-supplied `[worktree].location` keeps full `${ENV}` support —
    /// only the *default* stopped going through env expansion.
    #[test]
    fn explicit_location_still_wins_over_the_default() {
        let cfg = RootConfig::from_toml_str(
            r#"
[worktree]
location = "${MY_ROOT}/wt/{branch}"
"#,
        )
        .unwrap();
        let settings =
            settings_from_config(&cfg, &HashMap::new(), &test_paths(&[("HOME", "/home/t")]))
                .unwrap();
        assert_eq!(settings.location_template, "${MY_ROOT}/wt/{branch}");
    }

    #[test]
    fn cleanup_presets_map_to_retention_days() {
        // `keep_7d` / `keep_28d` (#210) are config-layer sugar: they desugar
        // to `RetentionDays` here and `CleanupPolicy` never sees them.
        let cfg = RootConfig::from_toml_str(
            r#"
[worktree]
cleanup = "keep_7d"
plan_cleanup = "keep_28d"
"#,
        )
        .unwrap();
        let settings =
            settings_from_config(&cfg, &HashMap::new(), &test_paths(&[("HOME", "/home/t")]))
                .unwrap();
        assert_eq!(settings.cleanup_implement, CleanupPolicy::RetentionDays(7));
        assert_eq!(settings.cleanup_plan, CleanupPolicy::RetentionDays(28));
    }

    /// A minimal engine (no plugins, no repos) whose only observable behavior
    /// is the sweep-throttle bookkeeping.
    async fn sweep_test_engine(
        interval: Duration,
    ) -> Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter> {
        let settings = EngineSettings {
            workflows: Vec::new(),
            repos: Vec::new(),
            limits: Limits::global(1),
            branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
            location_template: "/tmp/totsuka-sweep/{repo_name}/{branch}".to_string(),
            cleanup_implement: CleanupPolicy::Manual,
            cleanup_plan: CleanupPolicy::Immediate,
            env: HashMap::new(),
            select: SelectConfig::default(),
            readme_cache_dir: None,
            pr_title_template: "t".to_string(),
            pr_body_template: "b".to_string(),
            worktree_sweep_interval: interval,
            tools: crate::tool::builtin_registry(),
            default_tool: "claude".to_string(),
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

    #[tokio::test]
    async fn worktree_sweep_is_throttled_by_its_interval() {
        // A long interval: the startup cycle (None) always sweeps, later
        // cycles within the interval do not.
        let mut engine = sweep_test_engine(Duration::from_secs(3600)).await;
        engine.cycle().await.unwrap();
        let first = engine
            .last_worktree_sweep
            .expect("the startup cycle sweeps");
        engine.cycle().await.unwrap();
        assert_eq!(
            engine.last_worktree_sweep,
            Some(first),
            "a cycle inside the interval must not re-sweep"
        );

        // Duration::ZERO restores the pre-#210 behavior: every cycle sweeps.
        let mut engine = sweep_test_engine(Duration::ZERO).await;
        engine.cycle().await.unwrap();
        let first = engine.last_worktree_sweep.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        engine.cycle().await.unwrap();
        assert!(
            engine.last_worktree_sweep.unwrap() > first,
            "a zero interval sweeps every cycle"
        );
    }

    #[test]
    fn task_round_trips_through_record_payload() {
        let task = Task {
            id: "42".into(),
            source: "github".into(),
            title: "t".into(),
            body: Some("body".into()),
            repo_hint: Some("web".into()),
            labels: vec!["bug".into()],
            priority: 5,
            status: Some("実装待ち".into()),
            url: Some("https://example.com".into()),
            assignee: Some("me".into()),
            thread_key: None,
            message_key: None,
            instructions: None,
        };
        let db = StateDb::open_in_memory().unwrap();
        let id = db
            .upsert_task(&NewTask {
                source: "github".into(),
                source_task_id: task.id.clone(),
                workflow: "implement".into(),
                mode: "implement".into(),
                repo: None,
                priority: task.priority,
                title: task.title.clone(),
                url: task.url.clone(),
                source_payload: serde_json::to_value(&task).ok(),
                thread_key: task.thread_key.clone(),
                last_signal_at: None,
            })
            .unwrap();
        let record = db.get_task(id).unwrap().unwrap();
        assert_eq!(task_from_record(&record), task);
    }

    // -----------------------------------------------------------------
    // Conversation ingest (#242/#258)
    // -----------------------------------------------------------------

    /// An engine with one catch-all workflow, so `on_task_submit` always
    /// matches and the ingest path is what the test observes.
    async fn ingest_test_engine() -> Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter> {
        let mut engine = sweep_test_engine(Duration::from_secs(3600)).await;
        engine.settings.workflows = vec![Workflow {
            name: "implement".to_string(),
            source: "slack".to_string(),
            trigger: crate::domain::workflow::Trigger::new(toml::Table::new()),
            mode: WorkflowMode::Implement,
            agent: "mock_agent".to_string(),
            output: crate::config::OutputPolicy::None,
            on_success: None,
            on_failure: None,
            verification: crate::config::VerificationMode::None,
            timeout_secs: None,
            rubric: None,
            tool: None,
        }];
        engine
    }

    /// One delivery in the conversation `conv`, identified by `message_key`.
    fn delivery(conv: &str, message_key: Option<&str>, body: &str) -> Task {
        Task {
            id: conv.to_string(),
            source: "slack".into(),
            title: "会話".into(),
            body: Some(body.to_string()),
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            thread_key: None,
            message_key: message_key.map(str::to_string),
            instructions: None,
        }
    }

    /// The whole point of #242: a second message in the same conversation is
    /// accepted rather than silently dropped, and a finished conversation goes
    /// back to work.
    #[tokio::test]
    async fn a_second_message_reopens_a_finished_conversation() {
        let mut engine = ingest_test_engine().await;

        let ack = engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:100"), "one"))
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;

        // Drive it to a terminal state.
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done
        );

        // A new message in the same thread.
        let ack = engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:300"), "two"))
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        let record = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Queued, "the conversation reopened");
        assert_eq!(
            engine.db.list_tasks().unwrap().len(),
            1,
            "a reply is the same task, not a new one"
        );
        // Both messages are in the ledger, and the new one is queued for the
        // next dispatch while the first stays marked as delivered-or-not on
        // its own terms (nothing has dispatched yet here, so both are pending).
        let messages = engine.db.list_task_messages(id).unwrap();
        assert_eq!(
            messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    /// At-least-once delivery: the same `message_key` twice must change
    /// nothing — not the ledger, not the state.
    #[tokio::test]
    async fn a_redelivered_message_is_a_duplicate_and_changes_nothing() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:100"), "one"))
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }

        let ack = engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:100"), "one"))
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Duplicate);
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done,
            "a re-delivery must not reopen the conversation"
        );
        assert_eq!(engine.db.list_task_messages(id).unwrap().len(), 1);
    }

    /// A message arriving mid-flight waits in the ledger; requeueing a running
    /// task would throw away the work in progress.
    #[tokio::test]
    async fn a_message_for_a_running_conversation_is_queued_without_touching_its_state() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:100"), "one"))
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Start, None).unwrap();

        let ack = engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:200"), "two"))
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Running,
            "an in-flight conversation keeps running"
        );
        assert_eq!(engine.db.pending_task_messages(id).unwrap().len(), 2);
    }

    /// Sources that send one message per task (GitHub issues, Notion pages)
    /// omit `message_key`. They must behave exactly as before: ingested once,
    /// every re-delivery a `Duplicate`.
    #[tokio::test]
    async fn a_source_without_message_keys_keeps_its_at_most_once_behaviour() {
        let mut engine = ingest_test_engine().await;
        let issue = delivery("42", None, "fix the bug");

        assert_eq!(
            engine
                .on_task_submit("slack".into(), issue.clone())
                .unwrap()
                .status,
            TaskSubmitStatus::Accepted
        );
        let id = engine.db.find_by_source("slack", "42").unwrap().unwrap().id;
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }

        assert_eq!(
            engine.on_task_submit("slack".into(), issue).unwrap().status,
            TaskSubmitStatus::Duplicate,
            "without a message_key the conversation id is the dedup key"
        );
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done,
            "and a finished task is not reopened by a re-delivery"
        );
        assert_eq!(engine.db.list_task_messages(id).unwrap().len(), 1);
    }

    /// Even a brand-new conversation gets a ledger row, so the ledger is the
    /// whole history rather than "everything after the first message".
    #[tokio::test]
    async fn a_new_conversation_starts_its_ledger_with_the_first_message() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit("slack".into(), delivery("C1:100", Some("C1:100"), "one"))
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        let messages = engine.db.list_task_messages(id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_key, "C1:100");
        assert_eq!(messages[0].body, "one");
        assert!(messages[0].processed_at.is_none(), "queued for dispatch");
        // The payload keeps the whole normalized task for the audit trail.
        let payload: Task = serde_json::from_str(&messages[0].payload).unwrap();
        assert_eq!(payload.message_key.as_deref(), Some("C1:100"));
    }

    #[test]
    fn mode_strings_round_trip() {
        assert_eq!(mode_str(WorkflowMode::Plan), "plan");
        assert_eq!(execution_mode("plan"), ExecutionMode::Plan);
        assert_eq!(execution_mode("implement"), ExecutionMode::Implement);
        // Unknown persisted values fall back to implement (never plan: plan is
        // the restrictive read-oriented mode only when explicitly chosen).
        assert_eq!(execution_mode("bogus"), ExecutionMode::Implement);
    }
}
