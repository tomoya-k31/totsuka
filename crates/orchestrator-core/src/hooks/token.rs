//! The hook Bearer token (#785, ADR-0099).
//!
//! `totsuka run` generates it on first start and keeps it in
//! `$XDG_STATE_HOME/totsuka/hook-token` (0600); every later start reuses it,
//! so restarting `run` does not turn the hooks of surviving agents into 401s.
//! The other commands that talk to the receiver (`focus`, `doctor`) read the
//! same file. Rotation is deleting it and restarting `run`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::paths::Paths;
use crate::ports::secret::SecretString;

/// `$XDG_STATE_HOME/totsuka/hook-token`.
pub fn path(paths: &Paths) -> PathBuf {
    paths.state_dir().join("hook-token")
}

/// The stored token, or `None` when `run` has not created one yet (a missing
/// or empty file).
pub fn read(path: &Path) -> io::Result<Option<SecretString>> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Ok(Some(SecretString::new(s.trim()))),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The stored token, generating and storing a fresh one when there is none.
/// The mode is re-applied either way, so a loosened file is tightened back.
pub fn load_or_create(path: &Path) -> io::Result<SecretString> {
    if let Some(token) = read(path)? {
        super::set_mode(path, 0o600)?;
        return Ok(token);
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(token.as_bytes())?;
    super::set_mode(path, 0o600)?;
    Ok(SecretString::new(token))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn created_once_then_reused_and_kept_0600() {
        let dir = std::env::temp_dir().join(format!("totsuka-hook-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("hook-token");
        assert!(read(&path).unwrap().is_none());

        let first = load_or_create(&path).unwrap();
        assert_eq!(first.expose().len(), 64);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(
            first.expose(),
            second.expose(),
            "a restart reuses the token"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a loosened file is tightened back");
        assert_eq!(read(&path).unwrap().unwrap().expose(), first.expose());

        std::fs::remove_file(&path).unwrap();
        let rotated = load_or_create(&path).unwrap();
        assert_ne!(
            rotated.expose(),
            first.expose(),
            "deleting the file rotates"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
