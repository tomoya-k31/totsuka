//! Locating the plugins that ship alongside an installed `totsuka` (F-52).
//!
//! The release tarball puts `plugins/<name>/{<name>, plugin.toml}` next to the
//! binary, so `totsuka plugin install --bundled <name>` can find them with no
//! path from the user. A `cargo install`ed build has no bundled tree — that is
//! not an error, it just means the caller should fall back to installing from
//! a directory.
//!
//! Deliberately **not** driven by an environment variable: an unrecognised
//! `TOTSUKA_*` prints a warning to the child's stderr (ADR-0009), and the CLI
//! E2Es parse child stderr as a JSON error envelope, so a new env name there
//! breaks them (ADR-0018). The override is a hidden CLI flag instead.

use std::path::{Path, PathBuf};

/// A plugin found in a bundled plugins tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledPlugin {
    /// Directory name, which is also the plugin name and the binary name
    /// (ADR-0027).
    pub name: String,
    /// The directory to hand to `plugin install`.
    pub dir: PathBuf,
}

/// Where a bundled plugins tree may live, relative to the directory holding
/// the running executable, in priority order.
///
/// `<exe dir>/plugins` is the release tarball's own layout. The `libexec` form
/// is there for a future prefix-style install (a Homebrew formula would put the
/// binary in `bin/` and its private files in `libexec/`); it costs one `is_dir`
/// call and saves changing the lookup later.
pub fn candidate_roots(exe_dir: &Path) -> Vec<PathBuf> {
    vec![
        exe_dir.join("plugins"),
        exe_dir.join("../libexec/totsuka/plugins"),
    ]
}

/// Directories to search, given the executable path as invoked and its
/// symlink-resolved form.
///
/// Both are needed. `std::env::current_exe` does **not** resolve symlinks on
/// macOS — it reports the path the process was launched with (`_NSGetExecutablePath`),
/// unlike Linux where `/proc/self/exe` is already resolved. The documented
/// install shape is `/usr/local/bin/totsuka` symlinked to
/// `/usr/local/lib/totsuka/totsuka` with the plugins next to the *target*, so
/// searching only the invoked path would find nothing there. Searching the
/// invoked path first still matters for a plain copy-into-`bin` install.
fn search_dirs(exe: &Path, resolved: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for path in [Some(exe), resolved].into_iter().flatten() {
        if let Some(dir) = path.parent()
            && !dirs.iter().any(|d| d == dir)
        {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs
}

/// Resolve the bundled plugins root, or `None` when this build has no bundled
/// tree. `exists` decides whether a candidate is a usable directory (injected
/// so the search order is testable without laying out real files).
pub fn locate_in(
    exe: &Path,
    resolved: Option<&Path>,
    explicit: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    // An explicit root is used as given: if the caller pointed at something
    // that is not there, that is an error to report, not a reason to silently
    // fall back to a different tree.
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    search_dirs(exe, resolved)
        .into_iter()
        .flat_map(|dir| candidate_roots(&dir))
        .find(|p| exists(p))
}

/// Resolve the bundled plugins root for the running executable.
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    let exe = std::env::current_exe().ok()?;
    let resolved = std::fs::canonicalize(&exe).ok();
    locate_in(&exe, resolved.as_deref(), None, &|p| p.is_dir())
}

/// The plugins in a bundled tree, sorted by name.
///
/// A subdirectory without a `plugin.toml` is skipped rather than reported: the
/// tree is ours to lay out, and a stray directory is not the user's problem.
pub fn list(root: &Path) -> Vec<BundledPlugin> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<BundledPlugin> = entries
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            if !dir.join("plugin.toml").is_file() {
                return None;
            }
            Some(BundledPlugin {
                name: e.file_name().to_string_lossy().into_owned(),
                dir,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_binary_s_own_plugins_dir() {
        let found = locate_in(Path::new("/opt/totsuka/totsuka"), None, None, &|p| {
            p == Path::new("/opt/totsuka/plugins")
        });
        assert_eq!(found, Some(PathBuf::from("/opt/totsuka/plugins")));
    }

    #[test]
    fn falls_back_to_libexec() {
        let found = locate_in(Path::new("/usr/local/bin/totsuka"), None, None, &|p| {
            p == Path::new("/usr/local/bin/../libexec/totsuka/plugins")
        });
        assert_eq!(
            found,
            Some(PathBuf::from("/usr/local/bin/../libexec/totsuka/plugins"))
        );
    }

    #[test]
    fn follows_the_symlink_target_when_the_invoked_path_has_nothing() {
        // The documented install shape. `current_exe` does not resolve
        // symlinks on macOS, so without the resolved path this finds nothing.
        let invoked = Path::new("/usr/local/bin/totsuka");
        let resolved = Path::new("/usr/local/lib/totsuka/totsuka");
        let found = locate_in(invoked, Some(resolved), None, &|p| {
            p == Path::new("/usr/local/lib/totsuka/plugins")
        });
        assert_eq!(found, Some(PathBuf::from("/usr/local/lib/totsuka/plugins")));
    }

    #[test]
    fn the_invoked_path_is_searched_before_the_symlink_target() {
        let invoked = Path::new("/opt/a/totsuka");
        let resolved = Path::new("/opt/b/totsuka");
        // Both exist; the invoked path wins.
        let found = locate_in(invoked, Some(resolved), None, &|p| {
            p == Path::new("/opt/a/plugins") || p == Path::new("/opt/b/plugins")
        });
        assert_eq!(found, Some(PathBuf::from("/opt/a/plugins")));
    }

    #[test]
    fn no_bundled_tree_is_not_an_error() {
        assert_eq!(
            locate_in(Path::new("/usr/local/bin/totsuka"), None, None, &|_| false),
            None
        );
    }

    #[test]
    fn explicit_root_wins_and_is_not_probed() {
        // Even when nothing exists, an explicit root is returned verbatim so
        // the caller can report "you pointed at X and it is not there".
        let found = locate_in(
            Path::new("/opt/totsuka/totsuka"),
            None,
            Some(Path::new("/tmp/x")),
            &|_| false,
        );
        assert_eq!(found, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn listing_skips_directories_without_a_manifest_and_sorts() {
        let tmp = std::env::temp_dir().join(format!("totsuka-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for name in ["slack", "github"] {
            std::fs::create_dir_all(tmp.join(name)).unwrap();
            std::fs::write(tmp.join(name).join("plugin.toml"), "name = \"x\"").unwrap();
        }
        std::fs::create_dir_all(tmp.join("not-a-plugin")).unwrap();

        let names: Vec<String> = list(&tmp).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["github".to_string(), "slack".to_string()]);

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn listing_a_missing_root_is_empty() {
        assert!(list(Path::new("/nonexistent/totsuka/plugins")).is_empty());
    }
}
