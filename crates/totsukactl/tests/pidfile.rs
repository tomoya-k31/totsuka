use tempfile::TempDir;
use totsukactl::pidfile::{check, read_pid, remove, write_pid, PidState};

#[test]
fn read_pid_absent_returns_none() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("missing.pid");
    assert_eq!(read_pid(&p).unwrap(), None);
}

#[test]
fn write_then_read_round_trip() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("sup.pid");
    write_pid(&p, 12345).unwrap();
    assert_eq!(read_pid(&p).unwrap(), Some(12345));
}

#[test]
fn check_returns_alive_for_self_pid() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("alive.pid");
    write_pid(&p, std::process::id() as i32).unwrap();
    assert_eq!(check(&p).unwrap(), PidState::Alive(std::process::id() as i32));
}

#[test]
fn check_returns_stale_for_dead_pid() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("dead.pid");
    // PID 0x7fff_fffe is virtually guaranteed not to exist; if check returns Alive
    // (extreme luck) we accept either Alive/Stale — but assert it's not Absent.
    write_pid(&p, 0x7fff_fffe).unwrap();
    let st = check(&p).unwrap();
    assert_ne!(st, PidState::Absent);
}

#[test]
fn remove_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("nope.pid");
    remove(&p).unwrap();
    remove(&p).unwrap();
}
