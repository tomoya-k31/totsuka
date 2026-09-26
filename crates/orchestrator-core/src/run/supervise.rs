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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use super::ingest::{PluginRequestBudgets, forward_plugin_request};
use super::{
    Engine, EngineError, LOOKUP_IN_FLIGHT_BUDGET, MethodReport, PluginEvent, PluginReport,
    SUBMIT_IN_FLIGHT_BUDGET, StatusMoment, deliver_notification, state_event,
};
use crate::adapters::plugin_host::{CallStats, HostError, Liveness, Plugin};
use crate::domain::EventDetail;
use crate::domain::TaskId;
use crate::ports::git::GitRunner;
use crate::ports::llm::RepoClassifier;
use plugin_protocol::manifest::PluginKind as ManifestKind;
use plugin_protocol::methods::{NotifierEvent, NotifyParams};

/// What the supervisor remembers about each plugin over the run (#495 /
/// #497 / #499, pulled out of `Engine` in #758).
///
/// Bookkeeping only (ADR-0103): relaunching, notifying and reading stats off a
/// live [`Plugin`] stay with the engine, so everything here is testable
/// without a process.
#[derive(Debug, Default)]
pub(super) struct SupervisionLedger {
    /// Relaunch attempts per plugin, as timestamps inside the policy window.
    ///
    /// A sliding window rather than a lifetime counter: a plugin that crashes
    /// once a week is not the failure this budget exists to stop, and a
    /// `--watch` run can stay up for weeks.
    attempts: HashMap<String, Vec<Instant>>,
    /// Plugins the supervisor has stopped trying to relaunch (#495/#499).
    /// A task waiting on one of these is waiting forever, so dispatch fails it
    /// with a reason instead of parking it.
    abandoned: HashSet<String>,
    /// Call stats harvested from plugin instances that have been replaced
    /// (#497). A restart (#495) creates a **new** `Plugin`, so its counters
    /// start at zero; without carrying the old ones forward, the plugin that
    /// crashed most would report the fewest calls — the opposite of the truth.
    retired_stats: HashMap<String, CallStats>,
    /// Per-plugin `(crashes, restarts)` tallies (#497), so the summary can name
    /// *which* plugin is flapping rather than only how many times something
    /// did.
    tallies: HashMap<String, (usize, usize)>,
}

/// The answer to "may this plugin be relaunched again?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestartBooking {
    /// Booked. `used` is the attempts inside the window **before** this one.
    Booked { used: usize },
    /// The window already holds `used` attempts, the whole budget.
    Exhausted { used: usize },
}

impl SupervisionLedger {
    /// Book a relaunch attempt for `plugin` at `now` if fewer than
    /// `max_attempts` fall inside `window`; attempts that aged out are
    /// dropped first. An exhausted budget books nothing.
    pub(super) fn book_restart(
        &mut self,
        plugin: &str,
        now: Instant,
        window: Duration,
        max_attempts: u32,
    ) -> RestartBooking {
        let attempts = self.attempts.entry(plugin.to_string()).or_default();
        attempts.retain(|at| now.saturating_duration_since(*at) < window);
        let used = attempts.len();
        if used >= max_attempts as usize {
            return RestartBooking::Exhausted { used };
        }
        attempts.push(now);
        RestartBooking::Booked { used }
    }

    /// Count a crash, whatever is decided about it afterwards.
    pub(super) fn crashed(&mut self, plugin: &str) {
        self.tallies.entry(plugin.to_string()).or_default().0 += 1;
    }

    /// The plugin came back: count the restart, and a task waiting on it
    /// should wait rather than fail. Clearing here (not on the attempt) means
    /// the flag only ever says "down for good" while that is true.
    pub(super) fn restarted(&mut self, plugin: &str) {
        self.abandoned.remove(plugin);
        self.tallies.entry(plugin.to_string()).or_default().1 += 1;
    }

    /// The supervisor gave up on `plugin`.
    pub(super) fn abandon(&mut self, plugin: &str) {
        self.abandoned.insert(plugin.to_string());
    }

    /// Whether the supervisor gave up on `plugin`.
    pub(super) fn is_abandoned(&self, plugin: &str) -> bool {
        self.abandoned.contains(plugin)
    }

    /// Fold the call stats of an instance about to be replaced into the
    /// retired accumulator (#497).
    pub(super) fn retire(&mut self, plugin: &str, stats: &CallStats) {
        let retired = self.retired_stats.entry(plugin.to_string()).or_default();
        for (method, m) in stats {
            retired.entry(method.clone()).or_default().merge(m);
        }
    }

    /// Per-plugin RPC accounting for the run summary (#497): the `live`
    /// instances' stats plus everything retired from instances a restart
    /// replaced, so the numbers describe **the plugin over the run**, not
    /// whichever process happens to be current.
    pub(super) fn reports<'a>(
        &self,
        live: impl IntoIterator<Item = (&'a String, CallStats)>,
    ) -> BTreeMap<String, PluginReport> {
        let mut merged: BTreeMap<String, CallStats> = self
            .retired_stats
            .iter()
            .map(|(name, stats)| (name.clone(), stats.clone()))
            .collect();
        for (name, stats) in live {
            let target = merged.entry(name.clone()).or_default();
            for (method, m) in &stats {
                target.entry(method.clone()).or_default().merge(m);
            }
        }
        // A plugin that only ever crashed made no calls, but its crash count
        // is exactly what the operator needs — so the key set is the union.
        for name in self.tallies.keys() {
            merged.entry(name.clone()).or_default();
        }
        merged
            .into_iter()
            .map(|(name, stats)| {
                let (crashes, restarts) = self.tallies.get(&name).copied().unwrap_or((0, 0));
                let methods = stats
                    .into_iter()
                    .map(|(method, m)| {
                        let report = MethodReport {
                            calls: m.calls,
                            outcomes: m
                                .outcomes
                                .iter()
                                .map(|(o, n)| (o.as_str().to_string(), *n))
                                .collect(),
                            p50_ms: m.percentile_ms(0.50),
                            p95_ms: m.percentile_ms(0.95),
                        };
                        (method, report)
                    })
                    .collect();
                (
                    name,
                    PluginReport {
                        crashes,
                        restarts,
                        methods,
                    },
                )
            })
            .collect()
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

impl<G: GitRunner, L: RepoClassifier + 'static> Engine<G, L> {
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
        self.count_plugin_crash(plugin);
        if self.plugins.agents.contains_key(plugin) {
            self.fail_sessions_of(plugin).await?;
        }
        self.schedule_restart(plugin);
        Ok(())
    }

    /// Record that `plugin` died, without deciding anything about it.
    ///
    /// Split out of [`on_plugin_closed`](Self::on_plugin_closed) because the
    /// two halves are wanted in different places: the shutdown drain in
    /// [`run`](Self::run) needs the tally and must **not** run the teardown.
    /// There, failing in-flight tasks would contradict the graceful-shutdown
    /// contract, booking a restart would spawn a timer no loop is left to
    /// consume, and the write-back inside `fail_sessions_of` awaits a plugin
    /// RPC — a 120s hang after the run already decided to exit.
    pub(super) fn count_plugin_crash(&mut self, plugin: &str) {
        self.stats.plugin_crashes += 1;
        self.supervision.crashed(plugin);
    }

    /// Fail every in-flight task an exited agent plugin was running.
    async fn fail_sessions_of(&mut self, plugin: &str) -> Result<(), EngineError> {
        let affected: Vec<TaskId> = self
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
            if let Err(e) = self.db.apply_event(
                record.task_ref(),
                crate::domain::state::TaskEvent::Fail,
                Some(EventDetail::PluginCrash {
                    plugin: plugin.to_string(),
                }),
            ) {
                self.isolate_task(task_id, Err(e.into()))?;
                continue;
            }
            self.release_slot(task_id);
            self.agent_output.remove(&task_id);
            self.stats.failed += 1;
            self.write_back_status(&record, StatusMoment::Failure).await;
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
        let used = match self.supervision.book_restart(
            plugin,
            Instant::now(),
            policy.window,
            policy.max_attempts,
        ) {
            RestartBooking::Booked { used } => used,
            RestartBooking::Exhausted { used } => {
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
        };
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
                // It came back, so a task waiting on it should wait rather
                // than fail (the ledger clears the "abandoned" flag).
                self.supervision.restarted(&name);
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
        // Harvest the outgoing instance's counters before it is dropped
        // (#497). The `insert` below is what drops it, so this has to happen
        // first — afterwards the stats are simply gone.
        self.harvest_stats(name);
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

    /// Fold a live plugin's call stats into the retired accumulator (#497).
    pub(super) fn harvest_stats(&mut self, name: &str) {
        let live = self
            .plugins
            .sources
            .get(name)
            .or_else(|| self.plugins.agents.get(name))
            .or_else(|| self.plugins.notifiers.get(name))
            .map(|p| p.stats());
        if let Some(live) = live {
            self.supervision.retire(name, &live);
        }
    }

    /// Per-plugin RPC accounting for the run summary (#497).
    ///
    /// Live instances plus everything harvested from instances a restart
    /// replaced, so the numbers describe **the plugin over the run**, not
    /// whichever process happens to be current.
    pub(super) fn plugin_reports(&self) -> BTreeMap<String, PluginReport> {
        let live = self
            .plugins
            .sources
            .iter()
            .chain(self.plugins.agents.iter())
            .chain(self.plugins.notifiers.iter())
            .map(|(name, plugin)| (name, plugin.stats()));
        self.supervision.reports(live)
    }

    /// Tell the operator a plugin is staying down.
    ///
    /// Not [`notify_all`](super::notify_all): a dead plugin is not any one
    /// task's problem, and attaching it to whichever task happened to be
    /// running would misattribute it. `task_id` and `workflow` are `None` on
    /// purpose.
    fn escalate_dead_plugin(&mut self, plugin: &str, reason: &str) {
        // Marked **here**, not at the three call sites, so "we gave up" and
        // "dispatch knows we gave up" cannot drift apart. Every path that
        // leaves a plugin down runs through this function (#499).
        self.supervision.abandon(plugin);
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
        // Logged as well as delivered, because delivery is not reliable and
        // is least reliable exactly here. `notify` is fire-and-forget down a
        // pipe: writing to a dead plugin's stdin still returns `Ok` while the
        // writer task drains, so a failed delivery leaves no error either.
        //
        // The case that motivated this is the sharpest one: with a single
        // notifier configured, the escalation saying **that notifier is down**
        // is handed to the notifier that is down. Nobody hears it, and before
        // this line nothing recorded that we tried — an outage announcement
        // that goes silent, inside the change whose whole purpose is removing
        // silent failure.
        tracing::error!(
            plugin,
            notifiers = self.plugins.notifiers.len(),
            "escalating: {reason}"
        );
        deliver_notification(&self.plugins.notifiers, &params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(300);

    #[test]
    fn the_window_slides_rather_than_counting_a_lifetime() {
        let mut ledger = SupervisionLedger::default();
        let start = Instant::now();
        ledger.book_restart("p", start, WINDOW, 10);
        ledger.book_restart("p", start, WINDOW, 10);
        assert_eq!(
            ledger.book_restart("p", start, WINDOW, 10),
            RestartBooking::Booked { used: 2 }
        );
        // Every attempt ages out once the window has passed, so a plugin that
        // crashes rarely never exhausts its budget.
        let later = start + Duration::from_secs(301);
        assert_eq!(
            ledger.book_restart("p", later, WINDOW, 10),
            RestartBooking::Booked { used: 0 }
        );
    }

    #[test]
    fn an_exhausted_budget_books_nothing_and_is_per_plugin() {
        let mut ledger = SupervisionLedger::default();
        let now = Instant::now();
        assert_eq!(
            ledger.book_restart("p", now, WINDOW, 2),
            RestartBooking::Booked { used: 0 }
        );
        assert_eq!(
            ledger.book_restart("p", now, WINDOW, 2),
            RestartBooking::Booked { used: 1 }
        );
        assert_eq!(
            ledger.book_restart("p", now, WINDOW, 2),
            RestartBooking::Exhausted { used: 2 }
        );
        assert_eq!(
            ledger.book_restart("p", now, WINDOW, 2),
            RestartBooking::Exhausted { used: 2 },
            "a refusal is not itself an attempt"
        );
        assert_eq!(
            ledger.book_restart("other", now, WINDOW, 2),
            RestartBooking::Booked { used: 0 }
        );
    }

    #[test]
    fn a_restart_clears_abandonment_and_counts() {
        let mut ledger = SupervisionLedger::default();
        ledger.crashed("p");
        ledger.abandon("p");
        assert!(ledger.is_abandoned("p"));
        ledger.restarted("p");
        assert!(
            !ledger.is_abandoned("p"),
            "back up → no longer down for good"
        );
        let report = &ledger.reports([])["p"];
        assert_eq!((report.crashes, report.restarts), (1, 1));
    }

    /// #497: the summary describes the plugin over the run — a crash-only
    /// plugin still appears, and retired stats add to the live instance's.
    #[test]
    fn reports_merge_retired_and_live_stats_and_list_crash_only_plugins() {
        let mut ledger = SupervisionLedger::default();
        let calls = |n: usize| -> CallStats {
            let mut m = crate::adapters::plugin_host::MethodStats::default();
            m.calls = n;
            CallStats::from([("task/dispatch".to_string(), m)])
        };
        ledger.retire("agent", &calls(3));
        ledger.crashed("dead");
        let name = "agent".to_string();
        let reports = ledger.reports([(&name, calls(2))]);

        assert_eq!(reports["agent"].methods["task/dispatch"].calls, 5);
        assert_eq!(reports["dead"].crashes, 1);
        assert!(reports["dead"].methods.is_empty());
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
