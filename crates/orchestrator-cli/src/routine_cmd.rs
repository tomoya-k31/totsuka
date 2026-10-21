//! `totsuka routine ...` — the routines of this machine (#918, ADR-0117):
//! `list` shows what the routine plugin will do, `run` starts one now.
//!
//! `list` reads the files itself, through the same `routine-schedule` crate
//! the plugin uses, so it works with `run` stopped and cannot disagree with
//! the plugin about what a file means. `run` has to go through the running
//! orchestrator: only the plugin process can produce the task, and only the
//! engine can ingest it (`POST /routine/run`).

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Subcommand;
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::TaskId;
use orchestrator_core::domain::state::TaskState;
use routine_schedule::load_dir;
use serde::Serialize;
use time::OffsetDateTime;

use crate::common::{CliError, Cx, JsonFlag, print_json, safe};
use crate::focus_cmd::post_to_running;

/// Routine subcommands.
#[derive(Debug, Subcommand)]
pub enum RoutineCommand {
    /// List this machine's routines: schedule, next run, last run, and any
    /// problem with the file.
    List {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Run a routine now, once (needs `totsuka run --watch` or the app
    /// running). Refused while its previous run is unfinished; it does not
    /// use up a scheduled run.
    Run {
        /// The routine's name: its file name without `.md`.
        name: String,
    },
}

impl RoutineCommand {
    /// Whether errors should be emitted as JSON.
    pub fn wants_json(&self) -> bool {
        matches!(self, Self::List { json } if json.json)
    }
}

pub fn run(cx: &Cx, command: RoutineCommand) -> Result<(), CliError> {
    match command {
        RoutineCommand::List { json } => list(cx, json.json),
        RoutineCommand::Run { name } => run_now(cx, &name),
    }
}

/// Where this machine's routines live and what they may refer to.
pub(crate) struct Site {
    /// `<config dir>/routines/<host>`.
    pub dir: PathBuf,
    /// The enabled plugin instances whose manifest is `routine` — the
    /// `Task.source` of every routine task.
    pub sources: Vec<String>,
    repos: Vec<String>,
    workflows: Vec<String>,
}

/// The routine site of this config, `Ok(None)` when no routine plugin is
/// enabled, `Err` when there is one but no directory can be named.
pub(crate) fn site(cx: &Cx, cfg: &RootConfig) -> Result<Option<Site>, String> {
    let store = cx.store();
    let sources: Vec<String> = cfg
        .plugins
        .iter()
        .filter(|(name, plugin)| {
            plugin.enabled && matches!(store.manifest_of(name), Ok(Some(m)) if m.name == "routine")
        })
        .map(|(name, _)| name.clone())
        .collect();
    if sources.is_empty() {
        return Ok(None);
    }
    let host = cx.host.as_deref().ok_or(
        "this machine's hostname cannot name a routines/<host> directory, so no routine runs here",
    )?;
    Ok(Some(Site {
        dir: cx.config_home().join("routines").join(host),
        repos: cfg.repositories.iter().map(|r| r.name.clone()).collect(),
        workflows: cfg
            .domain_workflows()
            .into_iter()
            .filter(|w| sources.contains(&w.source))
            .map(|w| w.name)
            .collect(),
        sources,
    }))
}

/// One routine as `list` shows it.
#[derive(Debug, Serialize)]
pub(crate) struct Row {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub enabled: bool,
    /// RFC 3339, local offset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<LastRun>,
    /// Why this routine cannot run, when it cannot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LastRun {
    pub task_id: TaskId,
    pub state: TaskState,
    pub started: String,
}

impl Site {
    /// Every routine file, read and checked the way the plugin does.
    /// `last_runs` maps a routine name to its newest task.
    pub fn rows(&self, now: OffsetDateTime, mut last_runs: HashMap<String, LastRun>) -> Vec<Row> {
        load_dir(&self.dir)
            .into_iter()
            .map(|(name, parsed)| {
                let last = last_runs.remove(&name);
                match parsed {
                    Err(problem) => Row {
                        name,
                        schedule: None,
                        repo: None,
                        workflow: None,
                        enabled: false,
                        next: None,
                        last,
                        problem: Some(problem),
                    },
                    Ok(routine) => {
                        let checked = routine.check(&self.repos, &self.workflows);
                        let next = routine
                            .enabled
                            .then(|| routine.schedule.next_after(now))
                            .flatten()
                            .map(local_time);
                        Row {
                            schedule: Some(routine.schedule_text.clone()),
                            repo: Some(routine.repo.clone()),
                            workflow: checked.as_ref().ok().cloned(),
                            enabled: routine.enabled,
                            next: if checked.is_ok() { next } else { None },
                            last,
                            problem: checked.err(),
                            name,
                        }
                    }
                }
            })
            .collect()
    }
}

/// `2026-10-19T09:00:00+09:00` → `2026-10-19 09:00`.
fn local_time(t: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    )
}

/// The newest task of each routine in `sources`, keyed by routine name (its
/// `series`). Empty when there is no state DB yet.
fn last_runs(cx: &Cx, sources: &[String]) -> Result<HashMap<String, LastRun>, CliError> {
    if !cx.state_db_path().exists() {
        return Ok(HashMap::new());
    }
    let offset = orchestrator_core::logging::local_offset();
    let mut runs = HashMap::new();
    // Newest first, so the first task seen for a series is its last run.
    for task in cx.open_state_db()?.list_tasks()? {
        if !sources.contains(&task.source) {
            continue;
        }
        let Some(series) = task
            .source_payload
            .as_ref()
            .and_then(|p| p.get("series"))
            .and_then(|s| s.as_str())
        else {
            continue;
        };
        runs.entry(series.to_string()).or_insert(LastRun {
            task_id: task.id,
            state: task.state,
            started: local_time(task.created_at.to_offset(offset)),
        });
    }
    Ok(runs)
}

fn list(cx: &Cx, json: bool) -> Result<(), CliError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cfg = cx.load_config(&env)?;
    let Some(site) = site(cx, &cfg)? else {
        return Err(
            "no routine plugin is enabled → add [plugins.routine] (see `[routine]` in \
                    the config reference)"
                .into(),
        );
    };
    let now = OffsetDateTime::now_utc().to_offset(orchestrator_core::logging::local_offset());
    let rows = site.rows(now, last_runs(cx, &site.sources)?);
    if json {
        return print_json(&serde_json::json!({ "dir": site.dir, "routines": rows }));
    }
    println!("routines in {}", site.dir.display());
    if rows.is_empty() {
        println!(
            "none yet → add <name>.md there (frontmatter `repo` and `schedule`, the prompt as the body)"
        );
        return Ok(());
    }
    println!(
        "{:<20} {:<16} {:<16} {:<16} LAST",
        "NAME", "SCHEDULE", "REPO", "NEXT"
    );
    for row in &rows {
        let next = match (&row.problem, row.enabled, &row.next) {
            (Some(_), ..) => "-".to_string(),
            (None, false, _) => "disabled".to_string(),
            (None, true, Some(next)) => next.clone(),
            (None, true, None) => "never".to_string(),
        };
        let last = row
            .last
            .as_ref()
            .map(|l| format!("#{} {} ({})", l.task_id.0, l.state, l.started))
            .unwrap_or_else(|| "-".to_string());
        // Every column but NAME comes out of a file anyone can edit, so it is
        // stripped of control characters like task titles are.
        println!(
            "{:<20} {:<16} {:<16} {:<16} {}",
            safe(&row.name),
            safe(row.schedule.as_deref().unwrap_or("-")),
            safe(row.repo.as_deref().unwrap_or("-")),
            next,
            last
        );
        if let Some(problem) = &row.problem {
            println!("  ! {}", safe(problem));
        }
    }
    Ok(())
}

fn run_now(cx: &Cx, name: &str) -> Result<(), CliError> {
    let body = serde_json::json!({ "key": name }).to_string();
    let reply = post_to_running(cx, "/routine/run", &body)?;
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
    match (reply["ok"].as_bool(), reply["task_id"].as_i64()) {
        (Some(true), Some(id)) => {
            println!("routine {name}: started task {id} → `totsuka task show {id}`");
            Ok(())
        }
        _ => Err(format!(
            "routine {name} did not start: {}",
            reply["reason"].as_str().unwrap_or("no reason given")
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(files: &[(&str, &str)]) -> Site {
        let dir = test_support::scratch("routine-cmd-rows");
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        Site {
            dir,
            sources: vec!["routine".into()],
            repos: vec!["totsuka".into()],
            workflows: vec!["routine".into()],
        }
    }

    #[test]
    fn rows_show_next_run_last_run_and_problems() {
        let site = site(&[
            (
                "daily.md",
                "---\nrepo: totsuka\nschedule: 0 9 * * *\n---\ngo\n",
            ),
            (
                "off.md",
                "---\nrepo: totsuka\nschedule: 0 9 * * *\nenabled: false\n---\ngo\n",
            ),
            (
                "elsewhere.md",
                "---\nrepo: other\nschedule: 0 9 * * *\n---\ngo\n",
            ),
            ("broken.md", "nope"),
        ]);
        let last = HashMap::from([(
            "daily".to_string(),
            LastRun {
                task_id: TaskId(7),
                state: TaskState::Done,
                started: "2026-10-19 09:00".into(),
            },
        )]);
        let rows = site.rows(
            // 2026-10-19 13:00 +09:00
            OffsetDateTime::from_unix_timestamp(1792382400)
                .unwrap()
                .to_offset(time::UtcOffset::from_hms(9, 0, 0).unwrap()),
            last,
        );
        let by = |name: &str| rows.iter().find(|r| r.name == name).unwrap();

        let daily = by("daily");
        assert_eq!(daily.next.as_deref(), Some("2026-10-20 09:00"));
        assert_eq!(daily.workflow.as_deref(), Some("routine"));
        assert_eq!(daily.last.as_ref().unwrap().task_id, TaskId(7));
        assert!(daily.problem.is_none());

        assert_eq!(by("off").next, None, "disabled: no next run");
        let elsewhere = by("elsewhere");
        assert!(
            elsewhere
                .problem
                .as_deref()
                .unwrap()
                .contains("repo `other`")
        );
        assert_eq!(elsewhere.next, None, "cannot run: no next run");
        assert!(
            by("broken")
                .problem
                .as_deref()
                .unwrap()
                .contains("frontmatter")
        );
    }
}
