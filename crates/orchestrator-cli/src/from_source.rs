//! Installing plugins straight out of a totsuka checkout (#346).
//!
//! The development loop used to be four commands per plugin: `cargo build`,
//! copy the binary under its `plugin.toml` name, copy the manifest next to it,
//! then `plugin install` that staging directory. #343 removed the rename (the
//! Cargo bin name is now the `plugin.toml` name) and `prepare_install_from`
//! removed the staging directory (manifest and binary may live apart), so what
//! is left is one `cargo build` plus one install — which is what this module
//! does in a single command.
//!
//! Everything here is a pure function or a plain filesystem read — the one
//! place that actually spawns `cargo` is `plugin_cmd`'s `from_source_sources`.
//! That split is deliberate: `docs/quality/test-strategy.md` (ADR-0018) forbids
//! calling `cargo build` from a test, so the resolution logic has to be
//! testable without it, and the CLI exposes `--print-plan` to cover the wiring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plugin_protocol::manifest::Manifest;

/// A plugin as it exists in a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePlugin {
    /// `plugin.toml`'s `name` — also the Cargo bin name and the installed
    /// filename (ADR-0027).
    pub name: String,
    /// Cargo package name, which differs from `name` (`task-source-slack` vs
    /// `slack`) and is what `-p` needs.
    pub package: String,
    /// `<root>/plugins/<dir>/plugin.toml`.
    pub manifest_path: PathBuf,
}

/// Walk up from `start` to the first directory `looks_like_checkout` accepts.
///
/// Split out as a pure function because the interesting cases — a parent that
/// is a *different* workspace, no checkout anywhere above — are awkward to
/// stage on a real filesystem and easy to state as a predicate.
pub fn find_checkout_root(
    start: &Path,
    looks_like_checkout: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| looks_like_checkout(dir))
        .map(Path::to_path_buf)
}

/// Whether `dir` is a totsuka checkout: a Cargo workspace root that also has a
/// `plugins/` directory.
///
/// Deliberately not `git rev-parse --show-toplevel`: that answers for *any*
/// repository, so running the command from an unrelated clone would report a
/// root and then fail confusingly deeper in. Requiring the workspace table and
/// the plugins directory together is what makes the answer specific.
pub fn is_checkout(dir: &Path) -> bool {
    if !dir.join("plugins").is_dir() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    text.parse::<toml::Table>()
        .is_ok_and(|t| t.contains_key("workspace"))
}

/// The plugins in a checkout, keyed by `plugin.toml` name.
///
/// A `plugins/` subdirectory that is not a plugin (no `plugin.toml`, or no
/// `Cargo.toml`) is skipped rather than reported — the same tolerance
/// `bundled::list` has.
pub fn resolve_plugins(root: &Path) -> BTreeMap<String, SourcePlugin> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(root.join("plugins")) else {
        return found;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let manifest_path = dir.join("plugin.toml");
        let Ok(manifest_text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = Manifest::from_toml_str(&manifest_text) else {
            continue;
        };
        let Some(package) = cargo_package_name(&dir.join("Cargo.toml")) else {
            continue;
        };
        found.insert(
            manifest.name.clone(),
            SourcePlugin {
                name: manifest.name,
                package,
                manifest_path,
            },
        );
    }
    found
}

/// `[package] name` from a Cargo manifest.
fn cargo_package_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    table
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// The `cargo` arguments that build `packages`.
///
/// `--bins` rather than an explicit `--bin` per plugin: `scripts/arch-lint.sh`
/// guarantees each plugin package has exactly one bin target, so the two are
/// equivalent, and `--bins` cannot go stale. One invocation for all packages
/// takes the target-directory lock once instead of per plugin.
pub fn cargo_argv(release: bool, packages: &[&str]) -> Vec<String> {
    let mut argv = vec!["build".to_string()];
    if release {
        argv.push("--release".to_string());
    }
    for package in packages {
        argv.push("-p".to_string());
        argv.push((*package).to_string());
    }
    argv.push("--bins".to_string());
    argv
}

/// Where Cargo puts the binaries for a profile.
pub fn profile_dir(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_checkout_walking_up() {
        let start = Path::new("/w/totsuka/crates/orchestrator-cli/src");
        let found = find_checkout_root(start, &|d| d == Path::new("/w/totsuka"));
        assert_eq!(found, Some(PathBuf::from("/w/totsuka")));
    }

    #[test]
    fn stops_at_the_nearest_when_an_ancestor_also_matches() {
        // A checkout nested inside another workspace must resolve to the inner
        // one — otherwise `-p` would name packages the outer root does not have.
        let start = Path::new("/w/outer/inner/plugins/task-source-slack");
        let found = find_checkout_root(start, &|d| {
            d == Path::new("/w/outer") || d == Path::new("/w/outer/inner")
        });
        assert_eq!(found, Some(PathBuf::from("/w/outer/inner")));
    }

    #[test]
    fn no_checkout_above_is_none() {
        assert_eq!(
            find_checkout_root(Path::new("/tmp/elsewhere"), &|_| false),
            None
        );
    }

    #[test]
    fn cargo_argv_builds_every_package_in_one_invocation() {
        assert_eq!(
            cargo_argv(true, &["task-source-slack", "agent-ide-herdr"]),
            vec![
                "build",
                "--release",
                "-p",
                "task-source-slack",
                "-p",
                "agent-ide-herdr",
                "--bins",
            ]
        );
    }

    #[test]
    fn cargo_argv_omits_release_for_the_dev_profile() {
        let argv = cargo_argv(false, &["notifier-macos"]);
        assert!(!argv.iter().any(|a| a == "--release"), "{argv:?}");
        assert_eq!(profile_dir(false), "debug");
        assert_eq!(profile_dir(true), "release");
    }

    #[test]
    fn resolves_this_very_checkout() {
        // The repository is its own fixture: whatever plugins exist here must
        // resolve, and their Cargo bin name must equal the plugin.toml name
        // (the invariant arch-lint enforces).
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(
            is_checkout(root),
            "{} is not seen as a checkout",
            root.display()
        );

        let plugins = resolve_plugins(root);
        assert!(!plugins.is_empty());
        for (name, plugin) in &plugins {
            assert_eq!(name, &plugin.name);
            assert!(plugin.manifest_path.ends_with("plugin.toml"));
            assert_ne!(
                plugin.name, plugin.package,
                "the mapping would be pointless if these were equal"
            );
        }
        assert!(plugins.contains_key("slack"), "{plugins:?}");
        assert_eq!(plugins["slack"].package, "task-source-slack");
    }

    #[test]
    fn a_directory_that_is_not_a_checkout_is_rejected() {
        assert!(!is_checkout(Path::new(env!("CARGO_MANIFEST_DIR"))));
        assert!(!is_checkout(Path::new("/nonexistent")));
    }
}
