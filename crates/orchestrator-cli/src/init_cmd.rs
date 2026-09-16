//! `totsuka init` — config skeleton generation + environment check (§5.1).
//!
//! Never overwrites existing files: re-running is always safe.

use std::path::Path;
use std::process::Command;

use crate::common::{CliError, Cx};

/// The generated `config.toml` skeleton (§4.6 example, commented out).
///
/// Held as a file rather than a string literal so that
/// `scripts/config-template-lint.sh` can diff its keys against the config
/// structs in `orchestrator-core` and in every plugin crate.
///
/// A Rust-side check cannot do that job: **Rust has no way to enumerate a
/// struct's fields**, so short of a new derive macro the test would have to
/// carry a hand-written list of the keys — and that list is precisely the
/// thing that drifts next. Reading the sources as text needs no such list.
///
/// The same trick `orchestrator_core::hooks` uses for its seven shell scripts.
const CONFIG_TEMPLATE: &str = include_str!("../templates/config.toml");

/// Create the XDG directories totsuka writes into (§5.6).
///
/// Shared with `totsuka setup`, which needs the same directories to exist
/// before it writes anything — so a fresh machine does not have to run `init`
/// first just to make `setup` work.
pub fn ensure_dirs(cx: &Cx) -> Result<(), CliError> {
    for (label, dir) in [
        ("config", cx.config_path.parent().unwrap_or(Path::new("."))),
        ("data", cx.paths.data_dir()),
        ("state", cx.paths.state_dir()),
        ("cache", cx.paths.cache_dir()),
    ] {
        std::fs::create_dir_all(dir)?;
        println!("ok: {label} directory {}", dir.display());
    }
    Ok(())
}

/// Execute `totsuka init`.
pub fn run(cx: &Cx) -> Result<(), CliError> {
    // 1. Directories (XDG, §5.6).
    ensure_dirs(cx)?;

    // 2. config.toml skeleton — never overwrite.
    if cx.config_path.exists() {
        println!(
            "skipped: {} already exists (left untouched)",
            cx.config_path.display()
        );
    } else {
        std::fs::write(&cx.config_path, CONFIG_TEMPLATE)?;
        println!("created: {}", cx.config_path.display());
    }

    // 3. Environment checks.
    match git_version() {
        Some(version) => println!("ok: git {version}"),
        None => println!("warning: git not found on PATH → install git (worktrees require it)"),
    }

    println!(
        "next: `totsuka setup` fills the config in interactively — or edit it by hand, install \
         plugins (`totsuka plugin install --bundled --all --enable`), then `totsuka run --dry-run`"
    );
    Ok(())
}

/// The installed git version string, if git is on PATH.
pub fn git_version() -> Option<String> {
    let out = Command::new("git").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.trim()
            .strip_prefix("git version ")
            .unwrap_or(text.trim())
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::CONFIG_TEMPLATE;

    /// Every line of the skeleton is a comment.
    ///
    /// The file's whole contract is that generating it configures *nothing* —
    /// so a fresh `config.toml` loads, and everything in it is there as
    /// documentation until a human uncomments it. An active line slipped in
    /// while editing 400 lines of commented examples would turn that on its
    /// head silently: the key would take effect for every operator who ran
    /// `init` after that release. Parsing the file is the cheapest statement of
    /// the invariant — an empty table means no line survived the comment
    /// stripping.
    ///
    /// `scripts/config-template-lint.sh` is the other half (that every key is
    /// *present*); it reads the file as text and cannot tell a commented key
    /// from an active one.
    #[test]
    fn the_skeleton_activates_nothing() {
        let parsed: toml::Table = CONFIG_TEMPLATE
            .parse()
            .expect("the skeleton must be valid TOML");
        assert!(
            parsed.is_empty(),
            "the skeleton must be entirely commented out, but it sets: {:?}",
            parsed.keys().collect::<Vec<_>>()
        );
    }
}
