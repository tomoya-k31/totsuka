use chrono::{Duration, TimeZone, Utc};
use totsukactl::commands::status::format_table;
use totsukactl::sock_api::ProcessDto;
use totsukactl::state::ChildState;

#[test]
fn table_has_expected_header_and_rows() {
    let now = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let started = now - Duration::seconds(83 * 60);
    let healthz = now - Duration::seconds(5);
    let pgmq = ProcessDto {
        name: "pgmq".into(),
        pid: None,
        state: ChildState::Healthy,
        started_at: Some(started),
        last_healthz_at: Some(healthz),
        last_readyz_at: None,
        consecutive_failures: 0,
        restart_count: 0,
    };
    let adapter = ProcessDto {
        name: "agent-adapter".into(),
        pid: Some(1234),
        state: ChildState::Healthy,
        started_at: Some(started),
        last_healthz_at: Some(healthz),
        last_readyz_at: None,
        consecutive_failures: 0,
        restart_count: 0,
    };
    let s = format_table(&[pgmq, adapter], now);
    assert!(s.lines().next().unwrap().starts_with("NAME"));
    assert!(s.contains("pgmq"));
    assert!(s.contains("running")); // pgmq special-case
    assert!(s.contains("agent-adapter"));
    assert!(s.contains("healthy"));
    assert!(s.contains("1234"));
    assert!(s.contains("ok(5s)"));
    assert!(s.contains("1h23m"));
}

#[test]
fn missing_pid_and_times_render_dashes() {
    let now = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let stopped = ProcessDto {
        name: "qa-service".into(),
        pid: None,
        state: ChildState::Stopped,
        started_at: None,
        last_healthz_at: None,
        last_readyz_at: None,
        consecutive_failures: 0,
        restart_count: 2,
    };
    let s = format_table(&[stopped], now);
    assert!(s.contains("qa-service"));
    assert!(s.contains("stopped"));
    let row = s.lines().nth(1).unwrap();
    assert!(
        row.contains('-'),
        "expected dashes for pid/uptime/healthz, got {row}"
    );
    assert!(row.contains('2'), "restarts should still show count");
}
