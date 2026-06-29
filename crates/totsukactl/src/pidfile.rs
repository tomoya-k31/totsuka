use crate::error::TotsukactlError;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum PidState {
    Absent,
    Alive(i32),
    Stale(i32),
}

pub fn write_pid(path: &Path, pid: i32) -> Result<(), TotsukactlError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{pid}\n"))?;
    Ok(())
}

pub fn read_pid(path: &Path) -> Result<Option<i32>, TotsukactlError> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .trim()
            .parse::<i32>()
            .map(Some)
            .map_err(|e| TotsukactlError::Internal(format!("malformed pid file {path:?}: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn process_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid), None), Ok(()))
}

pub fn check(path: &Path) -> Result<PidState, TotsukactlError> {
    match read_pid(path)? {
        None => Ok(PidState::Absent),
        Some(pid) if process_alive(pid) => Ok(PidState::Alive(pid)),
        Some(pid) => Ok(PidState::Stale(pid)),
    }
}

pub fn remove(path: &Path) -> Result<(), TotsukactlError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
