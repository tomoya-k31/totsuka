use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile::{self, PidState};
use crate::sock_api::{ProcessDto, SupervisorClient};
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use std::io::Write;
use tabwriter::TabWriter;
use totsuka_core::Clock;

const CHILDREN: [&str; 4] = [
    "github-watcher",
    "qa-service",
    "orchestrator",
    "agent-adapter",
];

/// `status` result for exit-code mapping: a stopped stack is a normal
/// answer, not a command failure. systemctl-style: 0 = running, 3 = not
/// running (real failures stay `Err` → 1).
#[derive(Debug)]
pub enum StatusOutcome {
    Running,
    NotRunning,
}

impl StatusOutcome {
    pub fn exit_code(&self) -> std::process::ExitCode {
        match self {
            Self::Running => std::process::ExitCode::SUCCESS,
            Self::NotRunning => std::process::ExitCode::from(3),
        }
    }
}

#[derive(Debug)]
pub enum PgmqProbe {
    Running,
    Stopped,
    Unknown(String),
}

/// What we can still observe with the supervisor gone. The interesting
/// cases are the abnormal ones: a stale supervisor.pid (crash), leftover
/// sockets, and child pid files whose process is still alive (orphans).
#[derive(Debug)]
pub struct NotRunningReport {
    pub supervisor_pid: PidState,
    pub pgmq: PgmqProbe,
    pub stale_socks: Vec<String>,
    pub stale_pids: Vec<(String, PidState)>,
}

pub async fn run(
    paths: &Paths,
    clock: &dyn Clock,
    compose: &dyn ComposeExec,
) -> Result<StatusOutcome, TotsukactlError> {
    let client = SupervisorClient::new(paths.supervisor_sock());
    let entries = match client.list().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error=%e, "supervisor.sock unreachable");
            let report = gather_not_running_report(paths, compose).await;
            println!("{}", format_not_running(&report));
            return Ok(StatusOutcome::NotRunning);
        }
    };
    println!("{}", format_table(&entries, clock.now()));
    Ok(StatusOutcome::Running)
}

pub async fn gather_not_running_report(
    paths: &Paths,
    compose: &dyn ComposeExec,
) -> NotRunningReport {
    let supervisor_pid = pidfile::check(&paths.supervisor_pid()).unwrap_or(PidState::Absent);
    let pgmq = match compose.ps_running("pgmq").await {
        Ok(true) => PgmqProbe::Running,
        Ok(false) => PgmqProbe::Stopped,
        Err(e) => PgmqProbe::Unknown(e.to_string()),
    };
    let mut stale_socks: Vec<String> = std::fs::read_dir(&paths.sock_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    stale_socks.sort();
    let mut stale_pids = Vec::new();
    for name in CHILDREN {
        match pidfile::check(&paths.child_pid(name)) {
            Ok(PidState::Absent) | Err(_) => {}
            Ok(st) => stale_pids.push((name.to_string(), st)),
        }
    }
    NotRunningReport {
        supervisor_pid,
        pgmq,
        stale_socks,
        stale_pids,
    }
}

pub fn format_not_running(r: &NotRunningReport) -> String {
    let mut tw = TabWriter::new(Vec::new()).padding(2);
    let supervisor = match &r.supervisor_pid {
        PidState::Absent => "not running".to_string(),
        PidState::Stale(pid) => {
            format!("not running (stale supervisor.pid: pid {pid} is dead — crashed?)")
        }
        PidState::Alive(pid) => {
            format!("not responding (pid {pid} alive but supervisor.sock unreachable)")
        }
    };
    writeln!(tw, "SUPERVISOR\t{supervisor}").unwrap();
    let pgmq = match &r.pgmq {
        PgmqProbe::Running => "running".to_string(),
        PgmqProbe::Stopped => "stopped".to_string(),
        PgmqProbe::Unknown(reason) => format!("unknown ({reason})"),
    };
    writeln!(tw, "pgmq\t{pgmq}").unwrap();
    let socks = if r.stale_socks.is_empty() {
        "clean".to_string()
    } else {
        format!(
            "{} stale: {}",
            r.stale_socks.len(),
            r.stale_socks.join(", ")
        )
    };
    writeln!(tw, "sock/\t{socks}").unwrap();
    let pids = if r.stale_pids.is_empty() {
        "none".to_string()
    } else {
        let entries: Vec<String> = r
            .stale_pids
            .iter()
            .map(|(name, st)| match st {
                PidState::Alive(pid) => {
                    format!("{name}.pid (pid {pid} STILL ALIVE — orphan?)")
                }
                _ => format!("{name}.pid (dead)"),
            })
            .collect();
        format!("{} stale: {}", entries.len(), entries.join(", "))
    };
    writeln!(tw, "pid files\t{pids}").unwrap();
    tw.flush().unwrap();
    let mut out = String::from_utf8(tw.into_inner().unwrap()).unwrap();
    let has_orphan = r
        .stale_pids
        .iter()
        .any(|(_, st)| matches!(st, PidState::Alive(_)));
    out.push('\n');
    if has_orphan {
        out.push_str("hint: clean start with `totsukactl up`; orphan processes need manual kill\n");
    } else {
        out.push_str("hint: start the stack with `totsukactl up`\n");
    }
    out
}

pub fn format_table(entries: &[ProcessDto], now: DateTime<Utc>) -> String {
    let mut tw = TabWriter::new(Vec::new()).padding(2);
    writeln!(tw, "NAME\tSTATE\tPID\tUPTIME\tHEALTHZ\tRESTARTS").unwrap();
    for e in entries {
        let state =
            if e.name == "pgmq" && matches!(e.state, ChildState::Healthy | ChildState::Ready) {
                "running".to_string()
            } else {
                format!("{:?}", e.state).to_lowercase()
            };
        let pid = e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let uptime = e
            .started_at
            .map(|t| short_dur(now.signed_duration_since(t).num_seconds().max(0) as u64))
            .unwrap_or_else(|| "-".into());
        let hz = match e.last_healthz_at {
            Some(t) => format!(
                "ok({})",
                short_dur(now.signed_duration_since(t).num_seconds().max(0) as u64)
            ),
            None => "-".into(),
        };
        let restarts = if e.name == "pgmq" {
            "-".into()
        } else {
            e.restart_count.to_string()
        };
        writeln!(
            tw,
            "{}\t{}\t{}\t{}\t{}\t{}",
            e.name, state, pid, uptime, hz, restarts
        )
        .unwrap();
    }
    tw.flush().unwrap();
    String::from_utf8(tw.into_inner().unwrap()).unwrap()
}

fn short_dur(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}
