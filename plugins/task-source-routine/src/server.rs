//! The stdio server (F-51): a [`TaskSourceHandler`] plus the resident tick
//! that submits routines when they are due (#918, ADR-0117).
//!
//! Every [`TICK`] the tick re-reads `routines/<host>/` (so an edit needs no
//! restart) and, for each enabled routine, submits the latest slot inside its
//! catch-up window — at most one run, however many were missed. Nothing is
//! persisted here: the task id is `<name>@<slot>`, so the Orchestrator
//! answers a slot it already has with `duplicate`, and one whose previous run
//! is unfinished with `busy` ([`Task::series`]). The in-memory `last` map only
//! saves re-asking every tick.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use plugin_protocol::jsonrpc::{Error, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskSubmitParams, TaskTriggerParams, TaskUpdateStatusParams,
};
use plugin_protocol::{Capabilities, Task};
use plugin_sdk::{SubmitClient, SubmitOutcome, Submitter, TaskSourceHandler, not_initialized};
use serde_json::Value;
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::config::RoutineConfig;
use routine_schedule::{Routine, load_dir};

/// How often the routines are looked at. Under a minute, so no minute of a
/// schedule falls between two ticks while the machine is awake.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// The routine task-source stdio server.
pub struct Server {
    submit: SubmitClient,
    offset: UtcOffset,
    session: Option<Session>,
}

struct Session {
    site: Arc<Site>,
    ticker: Option<tokio::task::AbortHandle>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(ticker) = &self.ticker {
            ticker.abort();
        }
    }
}

/// What a tick and a manual run both need to turn a file into a task.
#[derive(Debug, Clone)]
pub struct Site {
    /// `<config dir>/routines/<host>`; `None` when the Orchestrator could not
    /// say where that is.
    pub dir: Option<PathBuf>,
    /// `Task.source`.
    pub source: String,
    /// The default catch-up window.
    pub window: Duration,
    /// The workflows naming this source.
    pub workflows: Vec<String>,
    /// The `[[repositories]]` names.
    pub repos: Vec<String>,
    /// The local time-zone offset cron fields are read in.
    pub offset: UtcOffset,
}

impl Site {
    fn load(&self) -> Vec<(String, Result<Routine, String>)> {
        self.dir.as_deref().map(load_dir).unwrap_or_default()
    }

    /// The task for one run of `routine`, with `id`, or why it cannot run.
    pub fn task(&self, routine: &Routine, id: String) -> Result<TaskSubmitParams, String> {
        let workflow = routine.check(&self.repos, &self.workflows)?;
        Ok(TaskSubmitParams {
            task: Task {
                id,
                source: self.source.clone(),
                title: routine.name.clone(),
                body: Some(routine.prompt.clone()),
                repo_hint: Some(routine.repo.clone()),
                labels: vec![],
                priority: 0,
                status: None,
                url: None,
                assignee: None,
                message_key: None,
                instructions: None,
                handle: Some(routine.name.clone()),
                branch_hint: None,
                series: Some(routine.name.clone()),
            },
            workflow,
        })
    }

    /// The slot a tick at `now` owes `routine`, if any.
    pub fn due(&self, routine: &Routine, now: OffsetDateTime) -> Option<OffsetDateTime> {
        let window = routine.catchup_window.unwrap_or(self.window);
        routine
            .schedule
            .latest_within(now.to_offset(self.offset), window)
    }
}

/// `2026-10-19 09:00 +09:00` → `20261019T0000Z`: the slot part of a task id,
/// in UTC so it means one instant whatever the offset.
fn stamp(t: OffsetDateTime) -> String {
    let t = t.to_offset(UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    )
}

/// What the tick remembers between runs: the id it last settled per routine,
/// and the problem it last logged per file (so a broken file is reported
/// once, not every [`TICK`]).
#[derive(Default)]
pub struct Memory {
    last: HashMap<String, String>,
    reported: HashMap<String, String>,
}

/// One tick: submit every routine that is due and not yet settled.
pub async fn tick<S: Submitter>(site: &Site, submit: &S, memory: &mut Memory, now: OffsetDateTime) {
    for (name, parsed) in site.load() {
        let params = parsed.and_then(|routine| {
            if !routine.enabled {
                return Ok(None);
            }
            let Some(slot) = site.due(&routine, now) else {
                return Ok(None);
            };
            site.task(&routine, format!("{name}@{}", stamp(slot)))
                .map(Some)
        });
        let params = match params {
            Ok(params) => {
                memory.reported.remove(&name);
                params
            }
            Err(problem) => {
                if memory.reported.get(&name) != Some(&problem) {
                    tracing::warn!(routine = %name, "{name}.md: {problem}");
                    memory.reported.insert(name.clone(), problem);
                }
                None
            }
        };
        let Some(TaskSubmitParams { task, workflow }) = params else {
            continue;
        };
        if memory.last.get(&name) == Some(&task.id) {
            continue;
        }
        let id = task.id.clone();
        let settled = match submit.submit(task, &workflow).await {
            SubmitOutcome::Accepted => {
                tracing::info!(routine = %name, task = %id, "routine started");
                true
            }
            // Already started for this slot (a restart re-asking): nothing to do.
            SubmitOutcome::Duplicate => true,
            SubmitOutcome::Busy { reason } => {
                tracing::info!(
                    routine = %name, task = %id,
                    "skipped, the previous run is unfinished: {}",
                    reason.as_deref().unwrap_or("no reason given")
                );
                true
            }
            SubmitOutcome::Rejected { reason } => {
                tracing::warn!(
                    routine = %name, task = %id,
                    "rejected: {}", reason.as_deref().unwrap_or("no reason given")
                );
                true
            }
            // Not settled: the next tick asks again.
            SubmitOutcome::GaveUp { error } => {
                tracing::error!(routine = %name, task = %id, "submission gave up: {error}");
                false
            }
        };
        if settled {
            memory.last.insert(name, id);
        }
    }
}

impl Server {
    /// A fresh, uninitialized server pushing through `submit`, reading cron
    /// fields in `offset`.
    pub fn new(submit: SubmitClient, offset: UtcOffset) -> Self {
        Self {
            submit,
            offset,
            session: None,
        }
    }
}

fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver")
}

fn parse_config(config: Value) -> Result<(RoutineConfig, Duration), String> {
    let config: RoutineConfig = serde_json::from_value(config)
        .map_err(|e| format!("invalid routine plugin config: {e}"))?;
    let window = config.window()?;
    Ok((config, window))
}

impl TaskSourceHandler for Server {
    fn initialized(&self) -> bool {
        self.session.is_some()
    }

    async fn initialize(&mut self, init: InitializeParams) -> Result<InitializeResult, Error> {
        let config_error = |message: String| Error::new(error_code::CONFIG_INVALID, message);
        let (config, window) = parse_config(init.config).map_err(config_error)?;
        // This source reads no trigger keys: a key on its workflow is a typo.
        let errors = plugin_sdk::unknown_trigger_keys(&init.workflows, &[]);
        if !errors.is_empty() {
            return Err(config_error(errors.join("; ")));
        }
        let dir = match (&init.config_dir, &init.host) {
            (Some(dir), Some(host)) => Some(PathBuf::from(dir).join("routines").join(host)),
            _ => {
                tracing::warn!(
                    "the Orchestrator did not say where the config lives or this host's name \
                     (an unusable hostname?) → no routines will run"
                );
                None
            }
        };
        let site = Arc::new(Site {
            dir,
            source: config.source_name,
            window,
            workflows: init.workflows.iter().map(|w| w.workflow.clone()).collect(),
            repos: init.repositories.iter().map(|r| r.name.clone()).collect(),
            offset: self.offset,
        });
        let ticker = (site.dir.is_some() && !site.workflows.is_empty()).then(|| {
            let (site, submit) = (Arc::clone(&site), self.submit.clone());
            tokio::spawn(async move {
                let mut memory = Memory::default();
                loop {
                    tick(&site, &submit, &mut memory, OffsetDateTime::now_utc()).await;
                    tokio::time::sleep(TICK).await;
                }
            })
            .abort_handle()
        });
        if let Some(dir) = &site.dir {
            tracing::info!(dir = %dir.display(), "routines are read from here");
        }
        self.session = Some(Session { site, ticker });
        Ok(InitializeResult {
            plugin_version: plugin_version(),
            capabilities: Capabilities {
                task_trigger: true,
                config_schema: true,
                ..Default::default()
            },
            claimed_repos: Vec::new(),
            claimed_options: Vec::new(),
        })
    }

    async fn config_validate(
        &mut self,
        params: ConfigValidateParams,
    ) -> Result<ConfigValidateResult, Error> {
        let errors: Vec<String> = parse_config(params.config).err().into_iter().collect();
        Ok(ConfigValidateResult {
            valid: errors.is_empty(),
            errors,
            warnings: Vec::new(),
        })
    }

    async fn config_schema(
        &mut self,
        _params: plugin_protocol::methods::ConfigSchemaParams,
    ) -> Result<plugin_protocol::methods::ConfigSchemaResult, Error> {
        use plugin_sdk::config_schema::{no_keys, of, workflow};
        Ok(plugin_protocol::methods::ConfigSchemaResult {
            // A project names this source and nothing else.
            project: Some(no_keys()),
            // Neither trigger keys nor workflow options: a routine says what
            // runs, in its own file.
            workflow: Some(workflow(serde_json::Map::new(), serde_json::Map::new())),
            ..of::<RoutineConfig>()
        })
    }

    /// Nothing to write back: a routine has no status at the source.
    async fn update_status(&mut self, _: TaskUpdateStatusParams) -> Result<Value, Error> {
        self.session.as_ref().ok_or_else(not_initialized)?;
        Ok(Value::Null)
    }

    /// Never sent: this source declares no `outputs`.
    async fn result_publish(&mut self, _: ResultPublishParams) -> Result<Value, Error> {
        Err(Error::new(
            error_code::METHOD_NOT_FOUND,
            "the routine source publishes nothing → use output = \"none\" on its workflows",
        ))
    }

    /// `totsuka routine run <name>`: the routine's task, now, under an id of
    /// its own so it never takes a scheduled slot's place.
    async fn task_trigger(&mut self, params: TaskTriggerParams) -> Result<TaskSubmitParams, Error> {
        let session = self.session.as_ref().ok_or_else(not_initialized)?;
        let site = &session.site;
        let refuse = |message: String| Error::new(error_code::INVALID_PARAMS, message);
        let Some(dir) = &site.dir else {
            return Err(refuse(
                "this machine has no routines directory (the hostname is unusable)".into(),
            ));
        };
        let routine = site
            .load()
            .into_iter()
            .find(|(name, _)| *name == params.key)
            .ok_or_else(|| {
                refuse(format!(
                    "no routine named `{}` in {}",
                    params.key,
                    dir.display()
                ))
            })?
            .1
            .map_err(|problem| refuse(format!("{}.md: {problem}", params.key)))?;
        let now = OffsetDateTime::now_utc();
        // To the second: two manual runs in one minute are two runs.
        let id = format!(
            "{}@manual-{}",
            routine.name,
            stamp(now).replace('Z', &format!("{:02}Z", now.second()))
        );
        site.task(&routine, id)
            .map_err(|problem| refuse(format!("{}.md: {problem}", params.key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use time::macros::{datetime, offset};

    /// Records every submit and answers with the next staged outcome
    /// (`Accepted` once they run out).
    #[derive(Default)]
    struct Recorder {
        submitted: Mutex<Vec<(String, String)>>,
        outcomes: Mutex<Vec<SubmitOutcome>>,
    }

    impl Submitter for Recorder {
        async fn submit(&self, task: Task, workflow: &str) -> SubmitOutcome {
            self.submitted
                .lock()
                .unwrap()
                .push((task.id, workflow.to_string()));
            self.outcomes
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(SubmitOutcome::Accepted)
        }
    }

    impl Recorder {
        fn ids(&self) -> Vec<String> {
            self.submitted
                .lock()
                .unwrap()
                .iter()
                .map(|(id, _)| id.clone())
                .collect()
        }
    }

    fn site(name: &str, files: &[(&str, &str)]) -> Site {
        let dir = test_support::scratch(name);
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        Site {
            dir: Some(dir),
            source: "routine".into(),
            window: Duration::hours(24),
            workflows: vec!["routine".into()],
            repos: vec!["totsuka".into()],
            offset: offset!(+9),
        }
    }

    const DAILY_9: &str = "---\nrepo: totsuka\nschedule: 0 9 * * *\n---\nCheck it.\n";

    /// Missed slots collapse to the latest one, which is submitted once — the
    /// next tick does not ask again, and the id carries the slot in UTC.
    #[tokio::test]
    async fn a_due_routine_is_submitted_once_for_its_latest_slot() {
        let site = site("routine-tick-once", &[("daily.md", DAILY_9)]);
        let recorder = Recorder::default();
        let mut memory = Memory::default();
        // Two days of 09:00 missed; only today's is owed (24h window).
        let now = datetime!(2026-10-19 13:00 +9);
        tick(&site, &recorder, &mut memory, now).await;
        tick(&site, &recorder, &mut memory, now + Duration::minutes(1)).await;
        assert_eq!(recorder.ids(), ["daily@20261019T0000Z"]);
        assert_eq!(recorder.submitted.lock().unwrap()[0].1, "routine");
        // The next day's slot is a new run.
        tick(&site, &recorder, &mut memory, now + Duration::days(1)).await;
        assert_eq!(recorder.ids().len(), 2);
    }

    /// `busy` and `duplicate` settle the slot like `accepted`; a submission
    /// that gave up is asked again next tick.
    #[tokio::test]
    async fn only_a_submission_that_gave_up_is_retried() {
        let site = site("routine-tick-retry", &[("daily.md", DAILY_9)]);
        let recorder = Recorder::default();
        let mut memory = Memory::default();
        let now = datetime!(2026-10-19 9:00:10 +9);
        *recorder.outcomes.lock().unwrap() = vec![
            SubmitOutcome::Busy { reason: None },
            SubmitOutcome::GaveUp {
                error: "gone".into(),
            },
        ];
        tick(&site, &recorder, &mut memory, now).await; // gave up
        tick(&site, &recorder, &mut memory, now).await; // busy: settled
        tick(&site, &recorder, &mut memory, now).await; // not asked again
        assert_eq!(recorder.ids().len(), 2);
    }

    /// A disabled, broken, or not-yet-due routine submits nothing, and does
    /// not stop the others.
    #[tokio::test]
    async fn only_enabled_valid_due_routines_run() {
        let site = site(
            "routine-tick-filter",
            &[
                ("a-broken.md", "no frontmatter"),
                (
                    "b-off.md",
                    "---\nrepo: totsuka\nschedule: * * * * *\nenabled: false\n---\nx\n",
                ),
                (
                    "c-elsewhere.md",
                    "---\nrepo: other\nschedule: * * * * *\n---\nx\n",
                ),
                (
                    "d-later.md",
                    "---\nrepo: totsuka\nschedule: 0 23 * * *\ncatchup_window: 1h\n---\nx\n",
                ),
                (
                    "e-now.md",
                    "---\nrepo: totsuka\nschedule: * * * * *\n---\nx\n",
                ),
            ],
        );
        let recorder = Recorder::default();
        tick(
            &site,
            &recorder,
            &mut Memory::default(),
            datetime!(2026-10-19 13:00 +9),
        )
        .await;
        assert_eq!(recorder.ids(), ["e-now@20261019T0400Z"]);
    }

    #[test]
    fn the_workflow_must_be_named_when_there_is_a_choice() {
        let mut site = site("routine-workflow", &[]);
        let routine = routine_schedule::parse("r", DAILY_9).unwrap();
        assert_eq!(
            site.task(&routine, "id".into()).unwrap().workflow,
            "routine"
        );
        site.workflows.push("weekly".into());
        let err = site.task(&routine, "id".into()).unwrap_err();
        assert!(
            err.contains("no `workflow` is named") && err.contains("weekly"),
            "{err}"
        );
        let named = routine_schedule::parse(
            "r",
            "---\nrepo: totsuka\nschedule: * * * * *\nworkflow: weekly\n---\nx\n",
        )
        .unwrap();
        let params = site.task(&named, "r@1".into()).unwrap();
        assert_eq!(params.workflow, "weekly");
        assert_eq!(params.task.series.as_deref(), Some("r"));
        assert_eq!(params.task.repo_hint.as_deref(), Some("totsuka"));
        assert_eq!(params.task.body.as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn task_trigger_answers_a_fresh_run_or_says_why_not() {
        let site = site("routine-trigger", &[("daily.md", DAILY_9), ("bad.md", "x")]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = plugin_sdk::Writer::from_channel(tx);
        let mut server = Server::new(SubmitClient::new(writer), offset!(+9));
        server.session = Some(Session {
            site: Arc::new(site),
            ticker: None,
        });
        let params = server
            .task_trigger(TaskTriggerParams {
                key: "daily".into(),
            })
            .await
            .unwrap();
        assert!(
            params.task.id.starts_with("daily@manual-"),
            "{}",
            params.task.id
        );
        assert_eq!(params.task.series.as_deref(), Some("daily"));
        for (key, needle) in [("nope", "no routine named `nope`"), ("bad", "bad.md:")] {
            let err = server
                .task_trigger(TaskTriggerParams { key: key.into() })
                .await
                .unwrap_err();
            assert!(err.message.contains(needle), "{key}: {}", err.message);
        }
    }
}
