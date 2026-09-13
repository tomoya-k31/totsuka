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
/// no persistable path could be resolved — the caller degrades to an
/// in-memory store rather than failing startup.
pub fn drafts_path(state_dir: Option<&Path>, source_name: &str) -> Option<PathBuf> {
    plugin_file(
        state_dir,
        source_name,
        "drafts.json",
        "the draft store stays in-memory",
    )
}

/// This plugin instance's Event Gateway receipt:
/// `{state_dir}/plugins/{source_name}/gateway-receipt.json` (#662).
///
/// It holds one fact — when a delivery was last pulled off the queue — so that
/// `config/validate` can tell **"never received anything"** from **"quiet for
/// a while"**. Those look identical from the outside and mean opposite things:
/// the first is an unfinished setup (a Request URL not entered in the Slack
/// app is the usual cause), the second is what an idle weekend looks like.
///
/// Separate from the draft store on purpose. The two have different lifetimes
/// and different consequences on loss — deleting this one costs a warning,
/// deleting drafts costs the operator's unsent text.
pub fn gateway_receipt_path(state_dir: Option<&Path>, source_name: &str) -> Option<PathBuf> {
    plugin_file(
        state_dir,
        source_name,
        "gateway-receipt.json",
        "the Event Gateway receipt is not recorded",
    )
}

/// `{state_dir}/plugins/{source_name}/{file}`, or `None` when no path
/// resolves. `consequence` completes the warning logged when `source_name`
/// is unusable, so each caller says what it loses rather than sharing a
/// vague one.
fn plugin_file(
    state_dir: Option<&Path>,
    source_name: &str,
    file: &str,
    consequence: &str,
) -> Option<PathBuf> {
    // `source_name` is operator-supplied config; as defense in depth, refuse
    // anything that is not a single plain path segment so the file can
    // never land outside `{state_dir}/plugins/` (e.g. `..`, `a/b`).
    if source_name.is_empty()
        || source_name == "."
        || source_name == ".."
        || source_name.contains(['/', '\\'])
    {
        tracing::warn!(
            source_name,
            consequence,
            "source_name is not a plain directory name"
        );
        return None;
    }
    let base = match state_dir {
        Some(dir) => dir.to_path_buf(),
        None => xdg_state_dir(|key| std::env::var(key).ok())?,
    };
    Some(base.join("plugins").join(source_name).join(file))
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
    // **Unique per process.** Two totsuka processes can hold the same state
    // directory at once — `totsuka doctor` and `config validate` both launch
    // the plugin, and `initialize` starts its drain loops, so running either
    // while `totsuka run` is live gives two writers for a few seconds. With a
    // shared temp path one can unlink or truncate the other's file mid-write;
    // the rename stays atomic, but the loser's `rename` fails with ENOENT and
    // the worse interleaving leaves a torn temp file for the winner to
    // publish. A crash can now leave a `<pid>` temp behind — a few dozen
    // bytes, against a corrupted file the reader would silently take as
    // "never received anything".
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
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

    /// The receipt sits beside the draft store and refuses the same names
    /// (#662): `source_name` is operator-supplied, so a path segment is the
    /// only shape allowed to reach the filesystem.
    #[test]
    fn the_gateway_receipt_sits_beside_the_drafts_and_refuses_traversal() {
        let dir = Path::new("/tmp/state");
        let receipt = gateway_receipt_path(Some(dir), "slack").expect("resolves");
        let drafts = drafts_path(Some(dir), "slack").expect("resolves");
        assert_eq!(receipt.parent(), drafts.parent());
        assert_eq!(receipt.file_name().unwrap(), "gateway-receipt.json");

        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert!(
                gateway_receipt_path(Some(dir), bad).is_none(),
                "accepted `{bad}`"
            );
        }
    }

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
    fn drafts_path_rejects_non_segment_source_names() {
        // Defense in depth: a source_name that is not a single plain path
        // segment must not resolve (the store degrades to in-memory).
        let base = Some(Path::new("/custom/state"));
        for bad in ["", ".", "..", "a/b", "a\\b", "../escape"] {
            assert_eq!(drafts_path(base, bad), None, "{bad:?} must be refused");
        }
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
        // No temp file survives a successful write — checked by scanning the
        // directory rather than by naming one, because the temp name now
        // carries the pid and a test that names it would pass by accident.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .expect("readable")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
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
