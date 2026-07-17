//! `run` main loop (#63, §5.1): fetch → match → ingest → repo select →
//! worktree → dispatch → monitor → finalize.
//!
//! The [`Engine`] integrates the pieces built by earlier tasks — plugin host
//! (#51), worktree lifecycle (#53), workflow matching (#54), scheduler (#55),
//! repo selection (#56), and restart recovery (#57) — into one cycle:
//!
//! 1. `tasks/fetch` per workflow trigger; ingest idempotently (F-73).
//! 2. Repository selection (F-10–F-14); ambiguity → `pending` + Notifier.
//! 3. Slot-gated dispatch (F-40–F-43): worktree create → `task/dispatch` →
//!    `state/subscribe`.
//! 4. `state/notification` events drive the task state machine; terminal
//!    handling runs the output policy (stubbed until #65), the
//!    `on_success`/`on_failure` status write-back (F-84), and worktree cleanup
//!    (F-23/F-85). `waiting_input`/`pending`/`done`/`failed` are delivered to
//!    Notifier plugins (F-35/F-90).
//!
//! **One-shot** (default): a single cycle, then the loop drains until every
//! dispatched task reaches a terminal or waiting state. **`--watch`**: keeps
//! polling each source at its interval (F-06) until shutdown. **Dry run**:
//! [`Engine::dry_run`] reports what would happen with zero side effects.

pub mod output;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use plugin_protocol::method;
use plugin_protocol::methods::{
    AgentState, ExecutionMode, NotifierEvent, NotifyParams, ResultPublishParams, StateNotification,
    TaskDispatchParams, TaskDispatchResult, TaskUpdateStatusParams, TasksFetchParams,
    TasksFetchResult,
};
use plugin_protocol::{Notification, Task};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::adapters::plugin_host::Plugin;
use crate::adapters::state_db::{NewTask, StateDb, StateError, TaskRecord};
use crate::adapters::{EngineSignalSink, hook_uds};
use crate::config::{
    CleanupPolicyConfig, CleanupPolicyName, DEFAULT_GLOBAL_CONCURRENCY, DEFAULT_POLL_INTERVAL_SECS,
    OutputPolicy, PluginKind, RootConfig, WorkflowMode, resolve::ResolveError,
};
use crate::domain::signal::AgentSignal;
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::workflow::{Workflow, match_workflow};
use crate::ports::agent_session::AttachOutcome;
use crate::ports::git::GitRunner;
use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};
use crate::ports::secret::SecretString;
use crate::recovery::{self, RecoveryReport, RetryPlan};
use crate::repo_select::{ReadmeCache, RepoCandidate, RepoDecision, SelectConfig, select_repo};
use crate::run::output::{
    DEFAULT_PR_BODY_TEMPLATE, DEFAULT_PR_TITLE_TEMPLATE, GhPrCreator, PrContext, PrCreator,
    PrRequest, render_template,
};
use crate::scheduler::{Limits, ReadyTask, SlotManager, counts_toward_slot, plan_dispatch};
use crate::worktree::{
    CleanupPolicy, CreateRequest, DEFAULT_BRANCH_TEMPLATE, DEFAULT_LOCATION_TEMPLATE,
    WorktreeManager,
};

/// Lines of a repository README shown to the LLM as selection context (F-11).
const README_HEAD_LINES: usize = 30;

/// How long the one-shot drain loop sleeps between settle checks.
const SETTLE_TICK: Duration = Duration::from_millis(200);

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
    /// Per-source polling intervals for `--watch` (F-06).
    pub poll_intervals: HashMap<String, Duration>,
    /// README head cache directory (`$XDG_CACHE_HOME/totsuka`), if any.
    pub readme_cache_dir: Option<PathBuf>,
    /// Pull-request title template (F-86).
    pub pr_title_template: String,
    /// Pull-request body template (F-86).
    pub pr_body_template: String,
}

/// Interpret a parsed [`RootConfig`] into [`EngineSettings`].
///
/// `env` supplies `${ENV}`/`~` expansion for repository paths and worktree
/// templates (injectable for tests).
pub fn settings_from_config(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
) -> Result<EngineSettings, ResolveError> {
    let env_fn = |k: &str| env.get(k).cloned();

    let mut repos = Vec::with_capacity(cfg.repositories.len());
    for repo in &cfg.repositories {
        repos.push(RepoSettings {
            name: repo.name.clone(),
            path: crate::config::expand_path(&repo.path.to_string_lossy(), &env_fn)?,
            summary: repo.summary.clone(),
            worktree_location: repo.worktree_location.clone(),
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

    let poll_intervals = cfg
        .plugins
        .iter()
        .filter(|(_, p)| p.enabled && p.kind == PluginKind::TaskSource)
        .map(|(name, p)| {
            let secs = p.poll_interval_secs.unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
            (name.clone(), Duration::from_secs(secs.max(1)))
        })
        .collect();

    Ok(EngineSettings {
        workflows: Workflow::from_configs(&cfg.workflows),
        repos,
        limits,
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        location_template: cfg
            .worktree
            .location
            .clone()
            .unwrap_or_else(|| DEFAULT_LOCATION_TEMPLATE.to_string()),
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
        poll_intervals,
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
    })
}

/// Map a config cleanup policy to the worktree policy, with a default.
fn cleanup_policy(config: Option<CleanupPolicyConfig>, default: CleanupPolicy) -> CleanupPolicy {
    match config {
        None => default,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Immediate)) => CleanupPolicy::Immediate,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Manual)) => CleanupPolicy::Manual,
        Some(CleanupPolicyConfig::Retention { retention_days }) => {
            CleanupPolicy::RetentionDays(retention_days)
        }
    }
}

/// Configuration for the UDS hook-receiving server (#136, E-03).
///
/// Built by the CLI (`run_cmd`), which resolves the Bearer token via the
/// platform secret store and computes the socket path. The server starts even
/// when `[hooks]` is unset — a config without a hook-capable agent simply never
/// receives a POST.
pub struct HookServerSettings {
    /// Absolute path of the Unix domain socket to listen on. Created `0600`;
    /// any stale socket is unlinked before bind and on shutdown.
    pub socket_path: PathBuf,
    /// Resolved Bearer token every POST must present (`Authorization: Bearer
    /// <token>`). `None` disables the check (0600 socket only); the CLI logs a
    /// warning in that case.
    pub auth_token: Option<SecretString>,
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
}

/// Counters accumulated over one `run` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunStats {
    /// Tasks returned by `tasks/fetch` (before dedup).
    pub fetched: usize,
    /// Newly ingested tasks (F-73: repeats do not count).
    pub ingested: usize,
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
    /// UDS hook receiver settings (#136); `None` = do not start the server
    /// (default for tests). Consumed (`take`n) when [`Engine::run`] starts.
    hook_server: Option<HookServerSettings>,
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
        Self::with_pr_creator(db, settings, plugins, git, llm, Box::new(GhPrCreator)).await
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
            hook_server: None,
            stats: RunStats::default(),
        }
    }

    /// Borrow the state DB (status queries, tests).
    pub fn db(&self) -> &StateDb {
        &self.db
    }

    /// Enable the UDS hook-receiving server (#136). [`Engine::run`] binds the
    /// socket and spawns [`hook_uds::serve`], forwarding each received signal
    /// onto this engine's event channel. Call before `run`.
    pub fn set_hook_server(&mut self, settings: HookServerSettings) {
        self.hook_server = Some(settings);
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
    /// waiting state (§5.1); `--watch` keeps polling each source at its
    /// interval (F-06) until `shutdown` resolves (SIGINT → graceful: in-flight
    /// tasks stay in the state DB for next-start recovery).
    pub async fn run<F>(&mut self, watch: bool, shutdown: F) -> Result<RunSummary, EngineError>
    where
        F: std::future::Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut interrupted = false;

        // Start the UDS hook receiver (#136), if configured. It runs as a
        // detached task; a `watch` channel signals graceful shutdown so it can
        // unlink the socket. `take` avoids borrowing `self` across the loop.
        let hook_handle = match self.hook_server.take() {
            Some(hs) => match hook_uds::bind(&hs.socket_path) {
                Ok(listener) => {
                    tracing::info!(socket = %hs.socket_path.display(), "hook receiver listening");
                    let sink = EngineSignalSink::new(self._events_tx.clone());
                    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                    let handle = tokio::spawn(hook_uds::serve(
                        listener,
                        hs.socket_path,
                        sink,
                        hs.auth_token,
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

        self.cycle(None).await?;
        // Per-source next-fetch times (F-06: each source polls at its own
        // interval, not the global minimum).
        let mut due: HashMap<String, tokio::time::Instant> = self
            .poll_sources()
            .into_iter()
            .map(|(source, interval)| (source, tokio::time::Instant::now() + interval))
            .collect();

        loop {
            if !watch && self.settled()? {
                break;
            }
            let next_poll = due.values().min().copied().unwrap_or_else(|| {
                tokio::time::Instant::now() + Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
            });
            tokio::select! {
                _ = &mut shutdown => {
                    interrupted = true;
                    break;
                }
                event = self.events.recv() => {
                    if let Some(event) = event {
                        self.on_event(event).await?;
                        self.dispatch_ready().await?;
                    }
                }
                _ = tokio::time::sleep_until(next_poll), if watch => {
                    let now = tokio::time::Instant::now();
                    let ready: HashSet<String> = due
                        .iter()
                        .filter(|(_, at)| **at <= now)
                        .map(|(source, _)| source.clone())
                        .collect();
                    self.cycle(Some(&ready)).await?;
                    for source in &ready {
                        due.insert(source.clone(), now + self.poll_interval_for(source));
                    }
                }
                _ = tokio::time::sleep(SETTLE_TICK), if !watch => {
                    // Safety tick: re-check settling and pick up freed slots.
                    self.dispatch_ready().await?;
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

    /// Every source referenced by a workflow (or with a configured interval),
    /// with its polling interval (F-06).
    fn poll_sources(&self) -> Vec<(String, Duration)> {
        let mut sources: HashSet<String> = self
            .settings
            .workflows
            .iter()
            .map(|w| w.source.clone())
            .collect();
        sources.extend(self.settings.poll_intervals.keys().cloned());
        sources
            .into_iter()
            .map(|source| {
                let interval = self.poll_interval_for(&source);
                (source, interval)
            })
            .collect()
    }

    /// The polling interval for one source (configured or default, F-06).
    fn poll_interval_for(&self, source: &str) -> Duration {
        self.settings
            .poll_intervals
            .get(source)
            .copied()
            .unwrap_or(Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS))
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

    /// One full cycle: fetch + ingest, repo selection, dispatch. `only_sources`
    /// restricts fetching (per-source watch pacing); `None` fetches all.
    pub async fn cycle(
        &mut self,
        only_sources: Option<&HashSet<String>>,
    ) -> Result<(), EngineError> {
        self.fetch_and_ingest(only_sources).await?;
        self.select_repos().await?;
        self.dispatch_ready().await?;
        self.sweep_finished_worktrees()?;
        Ok(())
    }

    /// Re-apply the cleanup policy to finished tasks whose worktree still
    /// exists (F-23: a `retention_days` policy elapses long after the
    /// finishing run's immediate cleanup attempt retained the worktree).
    fn sweep_finished_worktrees(&self) -> Result<(), EngineError> {
        for state in [TaskState::Done, TaskState::Cancelled] {
            for record in self.db.tasks_in_state(state)? {
                if record
                    .worktree_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).exists())
                {
                    self.cleanup_worktree(record.id)?;
                }
            }
        }
        Ok(())
    }

    /// Fetch tasks per workflow trigger and ingest them idempotently (F-73).
    async fn fetch_and_ingest(
        &mut self,
        only_sources: Option<&HashSet<String>>,
    ) -> Result<(), EngineError> {
        // Clone the workflow list so `self` stays free for ingest below.
        let workflows = self.settings.workflows.clone();
        for wf in &workflows {
            if let Some(only) = only_sources
                && !only.contains(&wf.source)
            {
                continue;
            }
            let Some(source) = self.plugins.sources.get(&wf.source) else {
                tracing::warn!(
                    workflow = %wf.name,
                    source = %wf.source,
                    "task source plugin not launched → enable and install it"
                );
                continue;
            };
            let params = TasksFetchParams {
                trigger: wf.trigger.to_json(),
            };
            let fetched: TasksFetchResult = match source.call(method::TASKS_FETCH, &params).await {
                Ok(result) => result,
                Err(e) => {
                    // Transient source failures skip the cycle, not the run
                    // (retries with backoff live inside the plugin, §5.3).
                    tracing::warn!(workflow = %wf.name, "tasks/fetch failed: {e}");
                    continue;
                }
            };
            self.stats.fetched += fetched.tasks.len();

            for mut task in fetched.tasks {
                // Normalize the origin to the plugin *instance* name: workflow
                // matching and ingest key on the `[plugins.<name>]` key, while
                // plugins stamp their own notion of a source name (e.g. the
                // GitHub plugin's `source_name` defaults to "github" whatever
                // the instance is called).
                task.source = wf.source.clone();
                // Defensive re-check (F-81): the plugin filtered on the
                // trigger, but the *first* matching workflow is authoritative.
                // A task whose authoritative match is a different workflow is
                // ingested by that workflow's fetch instead.
                match match_workflow(&workflows, &task) {
                    Some(authoritative) if authoritative.name == wf.name => {}
                    Some(_) => continue,
                    None => {
                        tracing::debug!(
                            task = %task.id,
                            workflow = %wf.name,
                            "fetched task does not match its trigger; skipped"
                        );
                        continue;
                    }
                }
                let is_new = self.db.find_by_source(&wf.source, &task.id)?.is_none();
                let new_task = NewTask {
                    source: wf.source.clone(),
                    source_task_id: task.id.clone(),
                    workflow: wf.name.clone(),
                    mode: mode_str(wf.mode).to_string(),
                    repo: None,
                    priority: task.priority,
                    title: task.title.clone(),
                    url: task.url.clone(),
                    // The full normalized task, so dispatch can reconstruct it.
                    source_payload: serde_json::to_value(&task).ok(),
                    // Carry the source's conversation-continuation key (E-09).
                    thread_key: task.thread_key.clone(),
                    last_signal_at: None,
                };
                let id = self.db.upsert_task(&new_task)?;
                if is_new {
                    self.stats.ingested += 1;
                    tracing::info!(task_id = id, workflow = %wf.name, title = %task.title, "ingested task");
                }
            }
        }
        Ok(())
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
        let worktree_path = match (&record.worktree_path, &record.branch) {
            (Some(path), Some(_)) => PathBuf::from(path),
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
                    Err(e) => {
                        return self.fail_dispatch(&record, e.to_string()).await;
                    }
                }
            }
        };

        // task/dispatch (F-31) → session id → persist (F-37) → subscribe (F-38).
        let agent = self.plugins.agents.get(&agent_name).expect("checked above");
        let params = TaskDispatchParams {
            task: task_from_record(&record),
            worktree_path: worktree_path.display().to_string(),
            mode: execution_mode(&record.mode),
            extra_context: None,
            job_id: None,
            resume_session_id: None,
            hook: None,
        };
        let dispatched: TaskDispatchResult = match agent.call(method::TASK_DISPATCH, &params).await
        {
            Ok(result) => result,
            Err(e) => {
                return self.fail_dispatch(&record, e.to_string()).await;
            }
        };
        self.db
            .record_session(record.id, &agent_name, &dispatched.session_id)?;
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
            // #138 wires hook signals into the state machine (idempotent
            // `hook_events` INSERT → completion/verification/escalation). Until
            // then, log receipt so the ingest path is observable end-to-end
            // instead of silently dropping — never `unreachable!()`, the adapter
            // really does deliver these.
            PluginEvent::HookSignal(signal) => {
                tracing::info!(
                    job_id = %signal.job_id,
                    event = ?signal.event,
                    "received hook signal (engine handling arrives in #138)"
                );
                Ok(())
            }
        }
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
                self.cleanup_worktree(record.id)?;
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

    /// Apply the cleanup policy to a finished task's worktree (F-23/F-85).
    fn cleanup_worktree(&self, task_id: i64) -> Result<(), EngineError> {
        // Re-fetch: `finished_at` was just set by the terminal transition.
        let Some(record) = self.db.get_task(task_id)? else {
            return Ok(());
        };
        let (Some(path), Some(branch), Some(repo_name)) =
            (&record.worktree_path, &record.branch, &record.repo)
        else {
            return Ok(());
        };
        // Already removed (earlier run / manual cleanup): nothing to do.
        if !Path::new(path).exists() {
            return Ok(());
        }
        let Some(repo) = self.settings.repos.iter().find(|r| &r.name == repo_name) else {
            return Ok(());
        };
        let policy = if record.mode == "plan" {
            self.settings.cleanup_plan
        } else {
            self.settings.cleanup_implement
        };
        match self.worktrees.cleanup(
            &repo.path,
            Path::new(path),
            branch,
            policy,
            record.finished_at.as_deref(),
            &now_rfc3339(),
        ) {
            Ok(crate::worktree::CleanupOutcome::Retained) => {
                // Expected under retention/manual policies; the sweep re-checks
                // every cycle, so keep this quiet.
                tracing::debug!(task_id, "worktree retained per policy");
            }
            Ok(outcome) => {
                tracing::info!(task_id, ?outcome, "worktree cleanup");
            }
            Err(e) => {
                tracing::warn!(task_id, "worktree cleanup failed: {e}");
            }
        }
        Ok(())
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

    /// Report what a run would do, with zero side effects (§5.1 `--dry-run`):
    /// no ingest, no worktree, no dispatch, no state changes.
    pub async fn dry_run(&self) -> Result<Vec<DryRunEntry>, EngineError> {
        let mut entries = Vec::new();
        for wf in &self.settings.workflows {
            let Some(source) = self.plugins.sources.get(&wf.source) else {
                tracing::warn!(workflow = %wf.name, source = %wf.source, "task source plugin not launched");
                continue;
            };
            let params = TasksFetchParams {
                trigger: wf.trigger.to_json(),
            };
            let fetched: TasksFetchResult = match source.call(method::TASKS_FETCH, &params).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!(workflow = %wf.name, "tasks/fetch failed: {e}");
                    continue;
                }
            };
            for mut task in fetched.tasks {
                // Same instance-name normalization as the real ingest path.
                task.source = wf.source.clone();
                match match_workflow(&self.settings.workflows, &task) {
                    Some(authoritative) if authoritative.name == wf.name => {}
                    _ => continue,
                }
                let already_ingested = self
                    .db
                    .find_by_source(&wf.source, &task.id)?
                    .map(|t| t.state.to_string());
                let repo = match self.decide_repo(&task).await {
                    RepoDecision::Selected { repo, reason } => format!("{repo} ({reason})"),
                    RepoDecision::Pending { reason } => format!("pending: {reason}"),
                    RepoDecision::Failed { reason } => format!("failed: {reason}"),
                };
                entries.push(DryRunEntry {
                    source: wf.source.clone(),
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    workflow: wf.name.clone(),
                    mode: mode_str(wf.mode),
                    agent: wf.agent.clone(),
                    repo,
                    already_ingested,
                });
            }
        }
        Ok(entries)
    }
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

/// Current time as RFC 3339 UTC (worktree retention comparison).
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting of current UTC time is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_interpret_limits_polls_and_cleanup() {
        let cfg = RootConfig::from_toml_str(
            r#"
max_concurrency = 2

[plugins.github]
enabled = true
kind = "task_source"
poll_interval_secs = 15

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
        let settings = settings_from_config(&cfg, &env).unwrap();

        assert_eq!(settings.limits.global, 2);
        assert_eq!(settings.limits.per_repo.get("web"), Some(&1));
        assert_eq!(settings.limits.per_agent.get("herdr"), Some(&3));
        assert_eq!(settings.repos[0].path, PathBuf::from("/home/t/repos/web"));
        assert_eq!(
            settings.poll_intervals.get("github"),
            Some(&Duration::from_secs(15))
        );
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Immediate);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::RetentionDays(2));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        let settings = settings_from_config(&cfg, &HashMap::new()).unwrap();
        assert_eq!(settings.limits.global, DEFAULT_GLOBAL_CONCURRENCY);
        // Implement keeps work (manual); plan cleans immediately (F-85).
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Manual);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::Immediate);
        assert_eq!(settings.location_template, DEFAULT_LOCATION_TEMPLATE);
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
