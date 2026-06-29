use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::sock_api::{ProcessDto, SupervisorClient};
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use std::io::Write;
use tabwriter::TabWriter;
use totsuka_core::Clock;

pub async fn run(paths: &Paths, clock: &dyn Clock) -> Result<(), TotsukactlError> {
    let client = SupervisorClient::new(paths.supervisor_sock());
    let entries = match client.list().await {
        Ok(v) => v,
        Err(_) => {
            println!("stack not running");
            return Err(TotsukactlError::NotRunning);
        }
    };
    println!("{}", format_table(&entries, clock.now()));
    Ok(())
}

pub fn format_table(entries: &[ProcessDto], now: DateTime<Utc>) -> String {
    let mut tw = TabWriter::new(Vec::new()).padding(2);
    writeln!(tw, "NAME\tSTATE\tPID\tUPTIME\tHEALTHZ\tRESTARTS").unwrap();
    for e in entries {
        let state = if e.name == "pgmq"
            && matches!(e.state, ChildState::Healthy | ChildState::Ready)
        {
            "running".to_string()
        } else {
            format!("{:?}", e.state).to_lowercase()
        };
        let pid = e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let uptime = e
            .started_at
            .map(|t| {
                short_dur(
                    now.signed_duration_since(t)
                        .num_seconds()
                        .max(0) as u64,
                )
            })
            .unwrap_or_else(|| "-".into());
        let hz = match e.last_healthz_at {
            Some(t) => format!(
                "ok({})",
                short_dur(
                    now.signed_duration_since(t)
                        .num_seconds()
                        .max(0) as u64
                )
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
