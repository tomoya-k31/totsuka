//! Keeping the plugin roster alive (#495).
//!
//! [`plugin_host`](crate::adapters::plugin_host) reports *that* a plugin is
//! gone and *why* ([`Liveness`]); this module decides what to do about it.
//! The split matters because the answer is kind-specific and needs engine
//! state that the transport layer has no business holding.
//!
//! # What was wrong before
//!
//! Death was only noticed for `agent_ide`, and only indirectly: the event was
//! emitted when an agent's *notification stream* ended. A `task_source` that
//! died produced no event at all — its incoming-request loop simply returned —
//! so `totsuka run --watch` kept running as a process that would never receive
//! another task, with one `WARN` line as the only trace.
//!
//! That asymmetry was reasonable when it was written: only agents held
//! in-flight tasks to roll back, and a polling `task_source` was repaired by
//! the next poll. **Protocol 0.2.0 removed `tasks/fetch`** (ADR-0008), and a
//! host that never fetches cannot tell a silent source from an idle one.
//!
//! # Shape
//!
//! 1. [`wire_liveness`] watches every plugin, of every kind, and turns a
//!    [`Liveness::Crashed`] into [`PluginEvent::Closed`]. An orderly
//!    [`Liveness::ShutDown`] emits nothing.
//! 2. [`Engine::on_plugin_closed`] runs the kind-specific teardown, then asks
//!    for a relaunch.
//! 3. The backoff is slept in a spawned task which sends
//!    [`PluginEvent::RestartDue`], so the engine loop keeps serving events
//!    while a plugin is down.
//! 4. [`Engine::on_restart_due`] relaunches and **re-wires the new process's
//!    streams** — the receivers are one-shot takes off a specific `Plugin`
//!    instance, so a consumer task left pointing at the dead one would sit
//!    there forever.

use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use super::ingest::{PluginRequestBudgets, forward_plugin_request};
use super::{
    Engine, EngineError, LOOKUP_IN_FLIGHT_BUDGET, PluginEvent, SUBMIT_IN_FLIGHT_BUDGET,
    deliver_notification, state_event,
};
use crate::adapters::plugin_host::{HostError, Liveness, Plugin};
use crate::ports::git::GitRunner;
use crate::ports::llm::LlmRouter;
use plugin_protocol::manifest::PluginKind as ManifestKind;
use plugin_protocol::methods::{NotifierEvent, NotifyParams};

/// Relaunch attempts for one plugin, as timestamps inside the policy window.
///
/// A sliding window rather than a lifetime counter: a plugin that crashes once
/// a week is not the failure this budget exists to stop, and a `--watch` run
/// can stay up for weeks.
#[derive(Debug, Default)]
pub(super) struct RestartLedger {
    attempts: Vec<Instant>,
}

impl RestartLedger {
    /// Attempts still inside `window`, dropping the ones that aged out.
    fn recent(&mut self, now: Instant, window: Duration) -> usize {
        self.attempts
            .retain(|at| now.saturating_duration_since(*at) < window);
        self.attempts.len()
    }

    fn record(&mut self, now: Instant) {
        self.attempts.push(now);
    }
}

/// Consume an agent's `state/notification` stream into engine events.
pub(super) async fn wire_agent(
    name: &str,
    plugin: &Plugin,
    tx: &mpsc::UnboundedSender<PluginEvent>,
) {
    let Some(mut notifications) = plugin.take_notifications().await else {
        return;
    };
    let name = name.to_string();
    let tx = tx.clone();
    tokio::spawn(async move {
        while let Some(note) = notifications.recv().await {
            if let Some(event) = state_event(&name, note)
                && tx.send(event).is_err()
            {
                return;
            }
        }
        // No `Closed` here any more: `wire_liveness` owns that, from the
        // child's exit rather than from this stream. An agent that never
        // subscribes still has a stream that ends, and an agent that declares
        // no `state_stream` never had one to end.
    });
}

/// Consume a task source's plugin-initiated requests (`task/submit`,
/// `task/lookup`) with a fresh per-plugin in-flight budget.
pub(super) async fn wire_source(
    name: &str,
    plugin: &Plugin,
    tx: &mpsc::UnboundedSender<PluginEvent>,
) {
    let Some(mut incoming) = plugin.take_incoming_requests().await else {
        return;
    };
    let name = name.to_string();
    let tx = tx.clone();
    let budgets = PluginRequestBudgets {
        submit: std::sync::Arc::new(Semaphore::new(SUBMIT_IN_FLIGHT_BUDGET)),
        lookup: std::sync::Arc::new(Semaphore::new(LOOKUP_IN_FLIGHT_BUDGET)),
    };
    tokio::spawn(async move {
        while let Some(request) = incoming.recv().await {
            forward_plugin_request(&name, request, &tx, &budgets);
        }
    });
}

/// Turn this plugin's death into a [`PluginEvent::Closed`], for any kind.
///
/// The receiver outlives the `Plugin` it was taken from, which is what lets a
/// restart replace the watched value; when the old `Plugin` is dropped the
/// sender goes with it and this task simply ends.
pub(super) fn wire_liveness(name: &str, plugin: &Plugin, tx: &mpsc::UnboundedSender<PluginEvent>) {
    let mut rx = plugin.liveness();
    let name = name.to_string();
    let tx = tx.clone();
    tokio::spawn(async move {
        let reason = loop {
            let current = *rx.borrow_and_update();
            if current != Liveness::Live {
                break current;
            }
            if rx.changed().await.is_err() {
                // Every sender dropped without the value ever leaving `Live`.
                // Nothing to report. (The usual drop path does *not* land
                // here: `kill_on_drop` kills the child, so a dropped plugin
                // normally marks `Crashed` first — harmless, because the only
                // instance a restart drops is the old one, whose watcher has
                // already fired and returned.)
                return;
            }
        };
        if reason == Liveness::Crashed {
            let _ = tx.send(PluginEvent::Closed(name));
        }
    });
}

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// A plugin process exited on its own (§5.3, #495).
    ///
    /// Kind-specific teardown first, relaunch second. **The order is
    /// load-bearing for agents**: the in-flight tasks must be failed and their
    /// session routes dropped before a new process can hand out session ids,
    /// or a fresh id could be matched against a task belonging to the dead one.
    pub(super) async fn on_plugin_closed(&mut self, plugin: &str) -> Result<(), EngineError> {
        tracing::warn!(plugin, "plugin process exited");
        // Counted before anything decides what to do about it: a crash that
        // was repaired is still a crash, and an operator who only ever sees
        // `plugin_restarts` cannot tell "never died" from "died and stayed
        // down" (the `restart = false` case, where nothing is relaunched).
        self.stats.plugin_crashes += 1;
        if self.plugins.agents.contains_key(plugin) {
            self.fail_sessions_of(plugin).await?;
        }
        self.schedule_restart(plugin);
        Ok(())
    }

    /// Fail every in-flight task an exited agent plugin was running.
    async fn fail_sessions_of(&mut self, plugin: &str) -> Result<(), EngineError> {
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
                crate::domain::state::TaskEvent::Fail,
                Some(serde_json::json!({ "kind": "plugin_crash", "plugin": plugin })),
            )?;
            self.release_slot(task_id);
            self.agent_output.remove(&task_id);
            self.stats.failed += 1;
            self.write_back_status(&record, false).await;
            super::notify_all(
                &self.plugins.notifiers,
                NotifierEvent::Failed,
                &record,
                Some(format!("agent plugin `{plugin}` crashed")),
            );
        }
        Ok(())
    }

    /// Book a relaunch attempt, or give up and say so.
    fn schedule_restart(&mut self, plugin: &str) {
        if !self.plugins.specs.contains_key(plugin) {
            // Nothing to relaunch from. Engines built by hand (tests) take
            // this path, and so would any future caller that assembles a
            // `PluginSet` without specs — detection still happened.
            tracing::warn!(plugin, "no launch spec recorded → not restarting");
            self.escalate_dead_plugin(plugin, "no launch spec was recorded for it");
            return;
        }
        if self.settings.restart_disabled.contains(plugin) {
            tracing::warn!(
                plugin,
                "restart is disabled for this plugin ([plugins.{plugin}].restart = false) \
                 → leaving it down"
            );
            // Escalating here is the whole point of the switch being about
            // *relaunching* and not about *noticing*. Someone who sets
            // `restart = false` wants the corpse kept, not the alarm silenced.
            self.escalate_dead_plugin(
                plugin,
                "restart is disabled for it ([plugins.<name>].restart = false)",
            );
            return;
        }
        let policy = &self.settings.plugin_restart;
        let now = Instant::now();
        let ledger = self.restarts.entry(plugin.to_string()).or_default();
        let used = ledger.recent(now, policy.window);
        if used >= policy.max_attempts as usize {
            let window_secs = policy.window.as_secs();
            tracing::error!(
                plugin,
                "gave up restarting after {used} attempts in {window_secs}s"
            );
            self.escalate_dead_plugin(
                plugin,
                &format!("{used} restart attempts in {window_secs}s all failed"),
            );
            return;
        }
        ledger.record(now);
        // 1s, 2s, 4s, … — `used` is the count *before* this attempt.
        let delay = policy.first_backoff.saturating_mul(1u32 << used.min(16));
        tracing::info!(
            plugin,
            "restarting in {}ms (attempt {}/{})",
            delay.as_millis(),
            used + 1,
            policy.max_attempts
        );
        let name = plugin.to_string();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(PluginEvent::RestartDue(name));
        });
    }

    /// A booked relaunch came due: launch **off the loop**.
    ///
    /// `Plugin::launch` sends `initialize` and waits for the reply, bounded
    /// only by the plugin's own RPC timeout (120s by default). Awaiting that
    /// here would stall the engine loop for the duration — no hook signals, no
    /// `task/submit` acks — for a plugin that is already down. The backoff is
    /// slept off the loop for the same reason; doing the launch on it would
    /// have undone that.
    pub(super) fn on_restart_due(&mut self, plugin: &str) {
        let Some(spec) = self.plugins.specs.get(plugin).cloned() else {
            return;
        };
        let name = plugin.to_string();
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let outcome = Plugin::launch(spec).await;
            let _ = tx.send(PluginEvent::Restarted {
                name,
                outcome: Box::new(outcome),
            });
        });
    }

    /// A relaunch attempt finished (#495).
    pub(super) async fn on_restarted(
        &mut self,
        name: String,
        outcome: Result<Plugin, HostError>,
    ) -> Result<(), EngineError> {
        match outcome {
            Ok(launched) => {
                let Some(kind) = self.plugins.specs.get(&name).map(|s| s.manifest.kind) else {
                    return Ok(());
                };
                self.install_restarted(&name, kind, launched).await;
                self.stats.plugin_restarts += 1;
                tracing::info!(plugin = %name, "plugin restarted");
            }
            Err(e) => {
                tracing::warn!(plugin = %name, "restart failed: {e}");
                // A failed launch is a spent attempt like any other, so the
                // same budget applies and this terminates.
                self.schedule_restart(&name);
            }
        }
        Ok(())
    }

    /// Put a relaunched plugin back in its map and re-establish its streams.
    async fn install_restarted(&mut self, name: &str, kind: ManifestKind, plugin: Plugin) {
        let tx = self.events_tx.clone();
        wire_liveness(name, &plugin, &tx);
        match kind {
            ManifestKind::TaskSource => {
                wire_source(name, &plugin, &tx).await;
                self.plugins.sources.insert(name.to_string(), plugin);
            }
            ManifestKind::AgentIde => {
                wire_agent(name, &plugin, &tx).await;
                self.plugins.agents.insert(name.to_string(), plugin);
            }
            ManifestKind::Notifier => {
                self.plugins.notifiers.insert(name.to_string(), plugin);
            }
        }
    }

    /// Tell the operator a plugin is staying down.
    ///
    /// Not [`notify_all`](super::notify_all): a dead plugin is not any one
    /// task's problem, and attaching it to whichever task happened to be
    /// running would misattribute it. `task_id` and `workflow` are `None` on
    /// purpose.
    fn escalate_dead_plugin(&self, plugin: &str, reason: &str) {
        let params = NotifyParams {
            event: NotifierEvent::Escalated,
            task_id: None,
            workflow: None,
            title: format!("plugin `{plugin}` is down"),
            body: Some(format!(
                "It exited and is staying down: {reason}. Tasks needing this \
                 plugin will not be processed until it is fixed and `totsuka \
                 run` is restarted."
            )),
        };
        deliver_notification(&self.plugins.notifiers, &params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_slides_rather_than_counting_a_lifetime() {
        let mut ledger = RestartLedger::default();
        let start = Instant::now();
        ledger.record(start);
        ledger.record(start);
        assert_eq!(ledger.recent(start, Duration::from_secs(300)), 2);
        // Both attempts age out once the window has passed, so a plugin that
        // crashes rarely never exhausts its budget.
        let later = start + Duration::from_secs(301);
        assert_eq!(ledger.recent(later, Duration::from_secs(300)), 0);
    }

    #[test]
    fn backoff_doubles_per_attempt_and_zero_stays_zero() {
        let first = Duration::from_secs(1);
        let delay = |used: usize| first.saturating_mul(1u32 << used.min(16));
        assert_eq!(delay(0), Duration::from_secs(1));
        assert_eq!(delay(1), Duration::from_secs(2));
        assert_eq!(delay(2), Duration::from_secs(4));
        // The test seam: a zero base stays zero however many attempts in.
        let instant = |used: usize| Duration::ZERO.saturating_mul(1u32 << used.min(16));
        assert_eq!(instant(3), Duration::ZERO);
    }
}
