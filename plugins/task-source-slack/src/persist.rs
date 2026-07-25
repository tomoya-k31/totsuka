//! Plugin-local file persistence for the draft store (#122): XDG state-path
//! resolution and an atomic, owner-only write primitive.
//!
//! The XDG semantics deliberately mirror the orchestrator's `paths.rs`
//! (absolute `XDG_STATE_HOME` wins, else `$HOME/.local/state`; relative XDG
//! values are ignored per the spec) — reimplemented here because plugins may
//! only depend on plugin-protocol / plugin-sdk (the arch-lint boundary), not
//! on orchestrator-core.

use std::io;
use std::path::{Path, PathBuf};

/// `${XDG_STATE_HOME:-$HOME/.local/state}/totsuka`, or `None` when neither
/// an absolute `XDG_STATE_HOME` nor `HOME` is available. `env` mirrors
/// [`std::env::var`] so tests can inject a fake environment.
fn xdg_state_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        // The XDG spec says relative values must be ignored.
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("totsuka"))
}

/// This plugin instance's draft-store file:
/// `{state_dir}/plugins/{source_name}/drafts.json`, where `{state_dir}` is
/// the `state_dir` config override (tests) or the XDG default. `None` means
/// no state directory could be resolved — the caller degrades to an
/// in-memory store rather than failing startup.
pub fn drafts_path(state_dir: Option<&Path>, source_name: &str) -> Option<PathBuf> {
    let base = match state_dir {
        Some(dir) => dir.to_path_buf(),
        None => xdg_state_dir(|key| std::env::var(key).ok())?,
    };
    Some(base.join("plugins").join(source_name).join("drafts.json"))
}

/// Write `bytes` to `path` atomically (temp file + rename) with 0600
/// permissions, creating parent directories as needed. The rename keeps a
/// crash mid-write from ever leaving a torn file behind.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    // Owner-only from the moment the file exists (a later chmod would leave
    // a umask-mode window where the draft text is world-readable): the store
    // holds draft text and thread coordinates (no tokens). Remove any
    // leftover temp file first — `mode` only applies to a fresh creation.
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn xdg_state_home_wins_when_absolute() {
        let dir = xdg_state_dir(env_from(&[
            ("HOME", "/home/alice"),
            ("XDG_STATE_HOME", "/xdg/state"),
        ]));
        assert_eq!(dir, Some(PathBuf::from("/xdg/state/totsuka")));
    }

    #[test]
    fn relative_or_empty_xdg_falls_back_to_home() {
        for bad in ["relative/state", ""] {
            let dir = xdg_state_dir(env_from(&[("HOME", "/home/bob"), ("XDG_STATE_HOME", bad)]));
            assert_eq!(dir, Some(PathBuf::from("/home/bob/.local/state/totsuka")));
        }
    }

    #[test]
    fn no_home_and_no_xdg_resolves_to_none() {
        assert_eq!(xdg_state_dir(env_from(&[])), None);
        assert_eq!(xdg_state_dir(env_from(&[("HOME", "")])), None);
    }

    #[test]
    fn drafts_path_honors_the_override() {
        let path = drafts_path(Some(Path::new("/custom/state")), "slack").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/custom/state/plugins/slack/drafts.json")
        );
    }

    #[test]
    fn atomic_write_creates_dirs_and_restricts_permissions() {
        let dir =
            std::env::temp_dir().join(format!("totsuka-slack-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/drafts.json");

        atomic_write(&path, b"{\"v\":1}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":1}");
        // The temp file must not survive the rename.
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "drafts.json must be owner-only");
        }

        // Overwrite goes through the same atomic path.
        atomic_write(&path, b"{\"v\":2}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":2}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
