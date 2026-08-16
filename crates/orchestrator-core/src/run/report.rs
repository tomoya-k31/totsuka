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
