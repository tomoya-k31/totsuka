//! What one `run` invocation reports on its way out (#464).
//!
//! The `totsuka run --json` document (#462) is this module's `Serialize`
//! impls, so the field names here are a public contract, not an internal
//! shape.

use super::*;

/// Counters accumulated over one `run` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
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
    /// Tasks that stepped aside because another member's instance claimed
    /// them first (#556): reached `skipped` this run.
    pub skipped: usize,
    /// Plugin processes that exited without being asked to (#495). Counted
    /// whatever happens next, so a crash is visible even when nothing is
    /// relaunched — which is exactly the `[plugins.{name}].restart = false`
    /// case, where [`plugin_restarts`](Self::plugin_restarts) stays 0.
    pub plugin_crashes: usize,
    /// Plugin processes successfully relaunched after crashing (#495). Read
    /// against `plugin_crashes`: equal means every death was repaired, lower
    /// means at least one plugin is still down.
    pub plugin_restarts: usize,
}

/// The summary printed when `run` exits (§5.1 one-shot contract).
///
/// [`Serialize`](serde::Serialize) is the `totsuka run --json` document
/// (#462): the **serialized field names are the public contract**, independent
/// of who reads the struct from Rust. Renaming one breaks every caller parsing
/// the output even if nothing in this workspace refers to it by name.
#[derive(Debug, Default, serde::Serialize)]
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
    /// Per-plugin RPC accounting (#497), keyed by plugin instance name.
    /// Empty when no plugin was called (a dry run, a config-only failure).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub plugins: std::collections::BTreeMap<String, PluginReport>,
}

/// What one plugin did over the run (#497).
///
/// Reported per plugin because "the run made 40 RPCs and 3 timed out" does not
/// say **whose** — and the whole point of this is being able to name the
/// plugin that is slow or failing without reading the log.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PluginReport {
    /// Times this plugin's process exited without being asked to (#495).
    pub crashes: usize,
    /// Times it was successfully relaunched (#495). Lower than `crashes`
    /// means it is currently down.
    pub restarts: usize,
    /// Per-method call accounting, keyed by JSON-RPC method name.
    pub methods: std::collections::BTreeMap<String, MethodReport>,
}

/// What one method did on one plugin (#497).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MethodReport {
    /// Calls started, whatever the outcome.
    pub calls: usize,
    /// Count per outcome (`ok` / `timeout` / `crashed` / `closed` /
    /// `rpc_error` / `json` / `io`). `ok` is included: a failure count alone
    /// cannot be read as a rate.
    pub outcomes: std::collections::BTreeMap<String, usize>,
    /// Median latency over the retained samples, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<u64>,
    /// 95th-percentile latency over the retained samples, in milliseconds.
    ///
    /// **Over the most recent samples, not the whole run.** A `--watch`
    /// process stays up for weeks, and keeping every latency would be
    /// unbounded for a number whose useful window is "lately" anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<u64>,
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

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
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
