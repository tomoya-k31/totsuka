//! Not-running `status` output: a diagnostic report (pgmq, stale socks,
//! stale/orphan pid files) instead of a bare error, and exit code 3
//! (systemctl-style "unit not active") instead of 1.
//! Spec: docs/superpowers/specs/2026-07-03-status-not-running-design.md

use std::sync::Arc;
use tempfile::TempDir;
use totsukactl::commands::status::{
    format_not_running, gather_not_running_report, run, NotRunningReport, PgmqProbe, StatusOutcome,
};
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::paths::Paths;
use totsukactl::pidfile::PidState;

fn tmp_paths(tmp: &TempDir) -> Paths {
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    paths
}

fn clean_report() -> NotRunningReport {
    NotRunningReport {
        supervisor_pid: PidState::Absent,
        pgmq: PgmqProbe::Running,
        stale_socks: vec![],
        stale_pids: vec![],
    }
}

#[test]
fn clean_stop_renders_all_clear_and_up_hint() {
    let s = format_not_running(&clean_report());
    assert!(s.contains("SUPERVISOR"), "got: {s}");
    assert!(s.contains("not running"), "got: {s}");
    assert!(s.contains("pgmq"), "got: {s}");
    assert!(s.contains("running"), "got: {s}");
    assert!(s.contains("clean"), "got: {s}");
    assert!(s.contains("none"), "got: {s}");
    assert!(s.contains("totsukactl up"), "got: {s}");
    assert!(!s.contains("orphan"), "clean stop must not warn: {s}");
    assert!(!s.contains("crashed"), "clean stop must not warn: {s}");
}

#[test]
fn stale_supervisor_pid_warns_crashed() {
    let r = NotRunningReport {
        supervisor_pid: PidState::Stale(12345),
        ..clean_report()
    };
    let s = format_not_running(&r);
    assert!(s.contains("12345"), "got: {s}");
    assert!(s.contains("crashed?"), "got: {s}");
}

#[test]
fn alive_child_pid_warns_orphan_and_manual_kill() {
    let r = NotRunningReport {
        stale_pids: vec![
            ("agent-adapter".into(), PidState::Alive(999)),
            ("orchestrator".into(), PidState::Stale(444)),
        ],
        ..clean_report()
    };
    let s = format_not_running(&r);
    assert!(s.contains("agent-adapter"), "got: {s}");
    assert!(s.contains("999"), "got: {s}");
    assert!(s.to_lowercase().contains("orphan"), "got: {s}");
    assert!(s.contains("orchestrator"), "got: {s}");
    assert!(s.contains("dead"), "got: {s}");
    assert!(s.contains("manual"), "orphan needs a manual-kill hint: {s}");
}

#[test]
fn stale_socks_are_listed() {
    let r = NotRunningReport {
        stale_socks: vec!["qa.sock".into(), "adapter.sock".into()],
        ..clean_report()
    };
    let s = format_not_running(&r);
    assert!(s.contains("qa.sock"), "got: {s}");
    assert!(s.contains("adapter.sock"), "got: {s}");
}

#[test]
fn pgmq_stopped_and_unknown_render() {
    let stopped = NotRunningReport {
        pgmq: PgmqProbe::Stopped,
        ..clean_report()
    };
    assert!(format_not_running(&stopped).contains("stopped"));
    let unknown = NotRunningReport {
        pgmq: PgmqProbe::Unknown("docker unreachable".into()),
        ..clean_report()
    };
    let s = format_not_running(&unknown);
    assert!(s.contains("unknown"), "got: {s}");
    assert!(s.contains("docker unreachable"), "got: {s}");
}

#[tokio::test]
async fn gather_collects_stale_state_from_disk() {
    let tmp = TempDir::new().unwrap();
    let paths = tmp_paths(&tmp);
    // stale supervisor pid (nonexistent process)
    std::fs::write(paths.supervisor_pid(), "2147483646\n").unwrap();
    // one dead child pid, one leftover socket
    std::fs::write(paths.child_pid("orchestrator"), "2147483645\n").unwrap();
    std::fs::write(paths.sock_dir.join("qa.sock"), "").unwrap();

    let compose = MockCompose::default();
    *compose.running.lock().unwrap() = true;
    let r = gather_not_running_report(&paths, &compose).await;

    assert_eq!(r.supervisor_pid, PidState::Stale(2147483646));
    assert!(matches!(r.pgmq, PgmqProbe::Running));
    assert_eq!(r.stale_socks, vec!["qa.sock".to_string()]);
    assert_eq!(
        r.stale_pids,
        vec![("orchestrator".to_string(), PidState::Stale(2147483645))]
    );
}

#[tokio::test]
async fn run_returns_not_running_outcome_when_sock_unreachable() {
    let tmp = TempDir::new().unwrap();
    let paths = tmp_paths(&tmp);
    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let clock = totsuka_core::SystemClock;
    let outcome = run(&paths, &clock, compose.as_ref()).await.unwrap();
    assert!(matches!(outcome, StatusOutcome::NotRunning));
    // exit-code contract: 0 = running, 3 = not running
    assert_eq!(
        format!("{:?}", StatusOutcome::NotRunning.exit_code()),
        format!("{:?}", std::process::ExitCode::from(3))
    );
    assert_eq!(
        format!("{:?}", StatusOutcome::Running.exit_code()),
        format!("{:?}", std::process::ExitCode::SUCCESS)
    );
}
