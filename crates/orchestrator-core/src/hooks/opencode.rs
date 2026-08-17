//! OpenCode asset installation (#196 Phase 3).
//!
//! OpenCode has neither Claude's per-launch `--settings` nor Codex's
//! `hooks.json`: completion detection is a **JS plugin** auto-loaded from the
//! user-global `$XDG_CONFIG_HOME/opencode/plugins/`, and plan mode is a
//! custom **agent markdown** under `agents/` (launched via
//! `--agent totsuka-plan`). Both are synced here, but they are not the same
//! kind of asset: the JS plugin is embedded in the binary and not
//! configurable (it is executed code), while the agent file is a fixed
//! frontmatter block — carrying the `permission` deny map — concatenated with
//! built-in prose (#316, ADR-0023; the prose was configurable until #465).
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
use crate::tool::ToolKind;

use super::AssetIssue;

/// Assets whose bytes are fixed in the binary: (config-dir-relative path,
/// content, mode). The plugin only needs to be readable by opencode (never
/// executed as a program), so 0600 keeps it totsuka-owned.
///
/// This is the code-execution surface — the JS plugin runs in every opencode
/// session. No configuration has ever been able to reach it (ADR-0023).
const STATIC_ASSETS: &[(&str, &str, u32)] = &[(
    "plugins/totsuka-opencode.js",
    include_str!("totsuka-opencode.js"),
    0o600,
)];

/// The plan-mode agent's YAML frontmatter, **fixed in Rust and deliberately
/// not configurable** (#316, [ADR-0023]).
///
/// `permission: {edit: deny, bash: deny, task: deny}` is the *whole* of what
/// carries plan intent here — not a guarantee (nothing is; ADR-0045), and
/// never measured on real opencode, but the only mechanism there is. opencode
/// has no structural plan flag — unlike claude's `--permission-mode plan` or
/// codex's `--sandbox read-only`, this file is
/// it — so a config key able to author this block would let text
/// that reads like prose grant `bash: allow` to every plan-mode task. That is
/// privilege escalation through a string field.
///
/// Only the prose body below it comes from
/// [`prompts`](crate::prompts::Prompts::opencode_plan_agent) — built-in since
/// #465, so there is no longer any value here that a config could reach.
///
/// [ADR-0023]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0023-configurable-prompt-surface.md
const PLAN_AGENT_FRONTMATTER: &str = "\
---
description: totsuka's plan/design mode agent. Produces a plan from reading and analysis; does not edit files, run commands, or delegate to subagents.
mode: primary
permission:
  edit: deny
  bash: deny
  task: deny
---

";

/// Assets rendered from config: (config-dir-relative path, content, mode).
///
/// Only the plan agent, and only its prose. Kept separate from
/// [`STATIC_ASSETS`] so the split between "what runs" and "what the model is
/// told" is visible at the call site rather than buried in a helper.
///
/// **No longer config-derived.** #465 removed `[prompts].opencode_plan_agent`,
/// so the body is the built-in one and this is constant for a given build. It
/// stays a function because the value is a runtime `format!` rather than a
/// `const`, and the "what runs / what is told" split is worth keeping visible.
fn rendered_assets() -> Vec<(&'static str, String, u32)> {
    let body = crate::prompts::Prompts::builtin()
        .opencode_plan_agent()
        .to_string();
    vec![(
        "agents/totsuka-plan.md",
        format!("{PLAN_AGENT_FRONTMATTER}{body}"),
        0o600,
    )]
}

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
    for (rel, content, mode) in STATIC_ASSETS {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        wrote |= super::write_if_changed(&path, content.as_bytes(), *mode)?;
    }
    for (rel, content, mode) in rendered_assets() {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        wrote |= super::write_if_changed(&path, content.as_bytes(), mode)?;
    }
    Ok(if wrote {
        SyncOutcome::Updated
    } else {
        SyncOutcome::Unchanged
    })
}

/// Verify the assets exist un-drifted **without** writing (read-only
/// counterpart to [`sync_assets`], for `doctor`).
///
/// Took a `&RootConfig` until #465, because the plan agent's prose used to be
/// config-derived (#316). It no longer is, and the expectation is still
/// recomputed on every call, so on-disk tampering is caught exactly as before.
pub fn verify_assets(config_dir: &Path) -> Vec<AssetIssue> {
    let mut issues = Vec::new();
    for (rel, content, mode) in STATIC_ASSETS {
        super::verify_one(
            &config_dir.join(rel),
            content.as_bytes(),
            *mode,
            &mut issues,
        );
    }
    for (rel, content, mode) in rendered_assets() {
        super::verify_one(&config_dir.join(rel), content.as_bytes(), mode, &mut issues);
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
        assert!(verify_assets(&dir).is_empty());

        // Tampering is flagged, and a resync repairs it.
        std::fs::write(&plugin, "tampered").unwrap();
        let issues = verify_assets(&dir);
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
        assert!(verify_assets(&dir).is_empty());
    }

    /// The exact bytes totsuka writes to `agents/totsuka-plan.md`.
    ///
    /// **This started as the behaviour-preservation proof for #316** — the
    /// expectation was the pre-#316 embedded file, transcribed rather than
    /// re-derived from `defaults.toml`. #465 translated the prose to English,
    /// so it no longer proves anything about the pre-#316 bytes; that proof
    /// ended deliberately, at a commit that says so.
    ///
    /// What it still does is worth keeping: the expectation is written out here
    /// rather than derived from the source, so a mangled edit to `defaults.toml`
    /// or to the frontmatter fails here instead of shipping. Update it by
    /// reading the new intended file, not by pasting what the code produced.
    #[test]
    fn plan_agent_renders_its_exact_bytes() {
        let expected = "\
---
description: totsuka's plan/design mode agent. Produces a plan from reading and analysis; does not edit files, run commands, or delegate to subagents.
mode: primary
permission:
  edit: deny
  bash: deny
  task: deny
---

You are operating in design and planning mode. Editing files, running commands,
and delegating to subagents are all denied by permission — including edits made
through a subagent. Work from reading and analysis, and present the plan or
design in prose.
";
        let rendered = rendered_assets();
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].0, "agents/totsuka-plan.md");
        assert_eq!(rendered[0].1, expected);
        assert_eq!(rendered[0].2, 0o600);
    }

    /// The permission deny map was never reachable from config (ADR-0023), and
    /// since #465 neither is the prose. This pins the *structural* half that
    /// survives both: whatever the body is, it lands after the fixed
    /// frontmatter, so a `permission:` line in it is inert text.
    #[test]
    fn plan_agent_frontmatter_is_not_configurable() {
        let content = &rendered_assets()[0].1;
        assert!(
            content.starts_with(PLAN_AGENT_FRONTMATTER),
            "the fixed frontmatter always comes first"
        );
        assert!(content.contains("  bash: deny"));
        assert!(
            !PLAN_AGENT_FRONTMATTER.contains("allow"),
            "the deny map has no `allow` in it"
        );
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
