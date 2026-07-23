//! OpenCode asset installation (#196 Phase 3).
//!
//! OpenCode has neither Claude's per-launch `--settings` nor Codex's
//! `hooks.json`: completion detection is a **JS plugin** auto-loaded from the
//! user-global `$XDG_CONFIG_HOME/opencode/plugins/`, and plan mode is a
//! custom **agent markdown** under `agents/` (launched via
//! `--agent totsuka-plan`). Both are embedded in the binary and synced here.
//!
//! Safety mirrors the codex module: the plugin fires for every opencode
//! session, so it registers no hooks unless `TOTSUKA_HOOK_ENDPOINT` /
//! `TOTSUKA_JOB_ID` are present (set only in orchestrator panes via
//! `ToolLaunchSpec.env`), and nothing is written unless the config actually
//! references an opencode-kind tool. Unlike codex there is no trust step —
//! opencode runs whatever sits in the plugins dir (its own security model),
//! so installation alone completes the setup.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::RootConfig;
use crate::paths::Paths;
use crate::tool::ToolKind;

use super::AssetIssue;

/// The embedded assets: (config-dir-relative path, content, mode).
/// The plugin only needs to be readable by opencode (never executed as a
/// program), and the agent is plain markdown — 0600 keeps both totsuka-owned.
const ASSETS: &[(&str, &str, u32)] = &[
    (
        "plugins/totsuka-opencode.js",
        include_str!("totsuka-opencode.js"),
        0o600,
    ),
    (
        "agents/totsuka-plan.md",
        include_str!("totsuka-plan.md"),
        0o600,
    ),
];

/// The opencode config directory: `$XDG_CONFIG_HOME/opencode`, else
/// `~/.config/opencode`. `None` when neither variable resolves.
pub fn opencode_config_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(xdg) = env("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("opencode"));
    }
    env("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".config").join("opencode"))
}

/// Whether the config can resolve any task to an opencode-kind tool
/// (mirrors `codex::references_codex`).
pub fn references_opencode(cfg: &RootConfig) -> bool {
    super::config_references_kind(cfg, ToolKind::Opencode)
}

/// What [`sync_assets`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The config references no opencode-kind tool — nothing touched.
    NotReferenced,
    /// An opencode tool is referenced but the config dir does not exist
    /// (opencode has never run); nothing written. `doctor` reports this.
    NoConfigDir,
    /// All assets were already up to date.
    Unchanged,
    /// At least one asset was created or refreshed.
    Updated,
}

/// Write the plugin + plan agent under the opencode config dir (idempotent by
/// content hash, like [`super::install`]).
pub fn sync_assets(config_dir: Option<&Path>, cfg: &RootConfig) -> io::Result<SyncOutcome> {
    if !references_opencode(cfg) {
        return Ok(SyncOutcome::NotReferenced);
    }
    let Some(dir) = config_dir.filter(|d| d.is_dir()) else {
        return Ok(SyncOutcome::NoConfigDir);
    };
    let mut wrote = false;
    for (rel, content, mode) in ASSETS {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        wrote |= super::write_if_changed(&path, content.as_bytes(), *mode)?;
    }
    Ok(if wrote {
        SyncOutcome::Updated
    } else {
        SyncOutcome::Unchanged
    })
}

/// Verify the assets exist un-drifted **without** writing (read-only
/// counterpart to [`sync_assets`], for `doctor`).
pub fn verify_assets(config_dir: &Path, _paths: &Paths) -> Vec<AssetIssue> {
    let mut issues = Vec::new();
    for (rel, content, mode) in ASSETS {
        super::verify_one(
            &config_dir.join(rel),
            content.as_bytes(),
            *mode,
            &mut issues,
        );
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("totsuka-opencode-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn opencode_cfg() -> RootConfig {
        RootConfig::from_toml_str(
            r#"
default_tool = "oc"

[tools.oc]
kind = "opencode"
"#,
        )
        .unwrap()
    }

    #[test]
    fn config_dir_resolution_prefers_xdg() {
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(
            opencode_config_dir(env),
            Some(PathBuf::from("/xdg/opencode"))
        );
        let env = |k: &str| (k == "HOME").then(|| "/home/u".to_string());
        assert_eq!(
            opencode_config_dir(env),
            Some(PathBuf::from("/home/u/.config/opencode"))
        );
    }

    #[test]
    fn sync_skips_without_reference_or_config_dir() {
        let base = unique_dir("skip");
        let claude_only = RootConfig::from_toml_str("").unwrap();
        assert_eq!(
            sync_assets(Some(&base.join("opencode")), &claude_only).unwrap(),
            SyncOutcome::NotReferenced
        );
        assert_eq!(
            sync_assets(Some(&base.join("opencode")), &opencode_cfg()).unwrap(),
            SyncOutcome::NoConfigDir
        );
        assert_eq!(
            sync_assets(None, &opencode_cfg()).unwrap(),
            SyncOutcome::NoConfigDir
        );
    }

    #[test]
    fn sync_writes_assets_idempotently_and_verify_flags_drift() {
        let base = unique_dir("write");
        let dir = base.join("opencode");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            sync_assets(Some(&dir), &opencode_cfg()).unwrap(),
            SyncOutcome::Updated
        );
        let plugin = dir.join("plugins/totsuka-opencode.js");
        assert!(plugin.is_file());
        assert!(dir.join("agents/totsuka-plan.md").is_file());
        // Second sync: nothing rewritten.
        assert_eq!(
            sync_assets(Some(&dir), &opencode_cfg()).unwrap(),
            SyncOutcome::Unchanged
        );
        let paths = crate::paths::Paths::from_env(|k| {
            (k == "HOME").then(|| base.to_string_lossy().into_owned())
        })
        .unwrap();
        assert!(verify_assets(&dir, &paths).is_empty());

        // Tampering is flagged, and a resync repairs it.
        std::fs::write(&plugin, "tampered").unwrap();
        let issues = verify_assets(&dir, &paths);
        assert!(
            issues
                .iter()
                .any(|i| i.path == plugin && i.problem.contains("content")),
            "{:?}",
            issues.iter().map(|i| &i.problem).collect::<Vec<_>>()
        );
        assert_eq!(
            sync_assets(Some(&dir), &opencode_cfg()).unwrap(),
            SyncOutcome::Updated
        );
        assert!(verify_assets(&dir, &paths).is_empty());
    }

    #[test]
    fn embedded_plugin_is_env_gated() {
        // The safety contract for a globally-loaded plugin: without the
        // orchestrator env it must bail before registering any hook.
        let js = include_str!("totsuka-opencode.js");
        assert!(js.contains("if (!ENDPOINT || !JOB_ID) return {}"));
        assert!(js.contains("TOTSUKA_HOOK_ENDPOINT"));
    }
}
