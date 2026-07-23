//! Codex CLI hook registration (#196 Phase 2).
//!
//! Codex has no per-launch `--settings` equivalent: hooks live in the
//! user-global `$CODEX_HOME/hooks.json` and fire for **every** codex session.
//! Two mechanisms keep that safe:
//!
//! - The scripts gate on `TOTSUKA_JOB_ID` (set only in orchestrator panes via
//!   `ToolLaunchSpec.env`), so personal sessions are untouched.
//! - This module owns only its own entries inside `hooks.json`. A managed
//!   entry is recognized **structurally** — its command path lives under the
//!   totsuka hooks dir (`hooks.json` is strict JSON, so there are no marker
//!   comments to lean on). Every other entry and field is preserved
//!   *semantically* — the file is re-serialized, so formatting and object key
//!   order may change, but no foreign value is altered and no array index
//!   shifts (codex keys its trust records by index). A file that fails to
//!   parse is never overwritten.
//!
//! Trust: codex persists per-entry trust in `$CODEX_HOME/config.toml` as
//! `[hooks.state."<hooks.json path>:<event>:<group>:<hook>"] trusted_hash`.
//! The hash covers the *config entry* (command string, timeout, …), not the
//! script bytes — so script updates via [`super::install`] need no re-trust,
//! but the entry itself changing (or its index shifting because neighbouring
//! entries were added/removed) does. Untrusted entries are **silently
//! skipped** by codex; [`untrusted_events`] lets `doctor` surface that, and
//! the one-time approval is the TUI startup review (`codex` → "Trust all and
//! continue"), documented in docs/operations.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::RootConfig;
use crate::paths::Paths;
use crate::tool::{ToolKind, ToolProfile};

use super::AssetIssue;

/// The totsuka-managed hook entries: codex event name → (script, timeout).
/// `SessionEnd` must stay ≤ 3 — codex clamps SessionEnd hook timeouts to 3s.
/// Codex has no `Notification` event; `PermissionRequest` (fires before an
/// approval prompt) is relayed through the same `on-notification.sh`, which
/// synthesizes the message and keeps stdout empty (an output would decide the
/// approval instead of relaying it).
const MANAGED_HOOKS: &[(&str, &str, u64)] = &[
    ("Stop", "on-stop.sh", 30),
    ("SessionStart", "on-session-start.sh", 10),
    ("SessionEnd", "on-session-end.sh", 3),
    ("UserPromptSubmit", "on-user-prompt-submit.sh", 10),
    ("PermissionRequest", "on-notification.sh", 10),
];

/// The codex home directory: `$CODEX_HOME`, else `$HOME/.codex`. `None` when
/// neither variable resolves.
pub fn codex_home(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(home) = env("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    env("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".codex"))
}

/// The managed hooks file inside a codex home.
pub fn hooks_json_path(codex_home: &Path) -> PathBuf {
    codex_home.join("hooks.json")
}

/// Whether the config can resolve any task to a codex-kind tool: the global
/// default, a repository default, or a workflow pin. Drives whether
/// [`sync_registration`] touches `$CODEX_HOME` at all — a claude-only setup
/// must never write there.
pub fn references_codex(cfg: &RootConfig) -> bool {
    let kind_of = |name: &str| {
        cfg.tool(name)
            .map(|t| t.kind)
            .or_else(|| ToolProfile::builtin(name).map(|p| p.kind))
    };
    cfg.default_tool
        .as_deref()
        .into_iter()
        .chain(cfg.repositories.iter().filter_map(|r| r.tool.as_deref()))
        .chain(cfg.workflows.iter().filter_map(|w| w.tool.as_deref()))
        .any(|name| kind_of(name) == Some(ToolKind::Codex))
}

/// What [`sync_registration`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The config references no codex-kind tool — `$CODEX_HOME` untouched.
    NotReferenced,
    /// A codex tool is referenced but the codex home does not exist (codex is
    /// not installed); nothing written. `doctor` reports this pairing.
    NoCodexHome,
    /// The managed entries were already up to date.
    Unchanged,
    /// `hooks.json` was created or its managed entries were (re)written.
    Updated,
}

/// Ensure the totsuka entries in `$CODEX_HOME/hooks.json` match the current
/// script set, preserving every non-managed entry semantically and at its
/// existing index (the rewrite may still normalize formatting/key order).
/// Idempotent; errors rather than overwriting a file it cannot parse.
pub fn sync_registration(
    codex_home: Option<&Path>,
    paths: &Paths,
    cfg: &RootConfig,
) -> io::Result<SyncOutcome> {
    if !references_codex(cfg) {
        return Ok(SyncOutcome::NotReferenced);
    }
    let Some(home) = codex_home.filter(|h| h.is_dir()) else {
        return Ok(SyncOutcome::NoCodexHome);
    };
    let path = hooks_json_path(home);
    let mut root = read_hooks_json(&path)?.unwrap_or_else(|| json!({ "hooks": {} }));
    apply_managed_entries(&mut root, &super::hooks_dir(paths))?;
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&root).expect("hooks JSON is always serializable")
    );
    if std::fs::read_to_string(&path).ok().as_deref() == Some(rendered.as_str()) {
        return Ok(SyncOutcome::Unchanged);
    }
    std::fs::write(&path, rendered)?;
    Ok(SyncOutcome::Updated)
}

/// Verify the managed entries are present and un-drifted **without** writing.
/// The read-only counterpart to [`sync_registration`], for `doctor`.
pub fn verify_registration(codex_home: &Path, paths: &Paths) -> Vec<AssetIssue> {
    let path = hooks_json_path(codex_home);
    let root = match read_hooks_json(&path) {
        Ok(Some(root)) => root,
        Ok(None) => {
            return vec![AssetIssue {
                path,
                problem: "missing".to_string(),
            }];
        }
        Err(e) => {
            return vec![AssetIssue {
                path,
                problem: format!("unreadable or not valid JSON: {e}"),
            }];
        }
    };
    let hooks_dir = super::hooks_dir(paths);
    let mut issues = Vec::new();
    for (event, script, timeout) in MANAGED_HOOKS {
        let expected = managed_group(&hooks_dir, script, *timeout);
        let found = root["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|group| is_managed(group, &hooks_dir))
            .collect::<Vec<_>>();
        if found.len() != 1 || *found[0] != expected {
            issues.push(AssetIssue {
                path: path.clone(),
                problem: format!("managed `{event}` entry missing, duplicated, or drifted"),
            });
        }
    }
    issues
}

/// The managed events codex would currently **skip silently**: their entry in
/// `hooks.json` has no matching `trusted_hash` recorded in
/// `$CODEX_HOME/config.toml`, or is explicitly disabled. Presence-only (the
/// hash algorithm is codex-internal), so a stale hash after an entry edit
/// still shows as trusted here — codex's own startup review remains the
/// authority. `doctor` turns a non-empty result into "run `codex` once and
/// trust the hooks".
pub fn untrusted_events(codex_home: &Path, paths: &Paths) -> io::Result<Vec<String>> {
    let path = hooks_json_path(codex_home);
    let root = read_hooks_json(&path)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "hooks.json missing"))?;
    let states = hook_states(&codex_home.join("config.toml"))?;
    let hooks_dir = super::hooks_dir(paths);
    let mut untrusted = Vec::new();
    for (event, _, _) in MANAGED_HOOKS {
        let Some(groups) = root["hooks"][event].as_array() else {
            continue;
        };
        for (idx, group) in groups.iter().enumerate() {
            if !is_managed(group, &hooks_dir) {
                continue;
            }
            let key = format!("{}:{}:{idx}:0", path.display(), event_key(event));
            let trusted = states
                .get(&key)
                .is_some_and(|(enabled, has_hash)| *enabled && *has_hash);
            if !trusted {
                untrusted.push((*event).to_string());
            }
        }
    }
    Ok(untrusted)
}

/// Parse `[hooks.state]` from a codex `config.toml`: key → (enabled,
/// has trusted_hash). A missing file yields an empty map (nothing trusted).
fn hook_states(config_toml: &Path) -> io::Result<std::collections::HashMap<String, (bool, bool)>> {
    let text = match std::fs::read_to_string(config_toml) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e),
    };
    let value: toml::Table = text
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("config.toml: {e}")))?;
    let mut states = std::collections::HashMap::new();
    if let Some(table) = value
        .get("hooks")
        .and_then(|h| h.get("state"))
        .and_then(|s| s.as_table())
    {
        for (key, state) in table {
            let enabled = state
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            let has_hash = state
                .get("trusted_hash")
                .and_then(toml::Value::as_str)
                .is_some_and(|h| !h.is_empty());
            states.insert(key.clone(), (enabled, has_hash));
        }
    }
    Ok(states)
}

/// The snake_case event segment codex uses in `[hooks.state]` keys.
fn event_key(event: &str) -> &'static str {
    match event {
        "Stop" => "stop",
        "SessionStart" => "session_start",
        "SessionEnd" => "session_end",
        "UserPromptSubmit" => "user_prompt_submit",
        "PermissionRequest" => "permission_request",
        other => unreachable!("unknown managed event {other}"),
    }
}

/// Read and parse `hooks.json`. `Ok(None)` when the file does not exist;
/// `Err` when it exists but is unreadable or not a JSON object (the caller
/// must never overwrite a file it cannot faithfully merge into).
fn read_hooks_json(path: &Path) -> io::Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not valid JSON ({e}) — refusing to rewrite it",
                path.display()
            ),
        )
    })?;
    if !value.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not a JSON object — refusing to rewrite it",
                path.display()
            ),
        ));
    }
    Ok(Some(value))
}

/// Bring each event's managed entry up to date **in place**: an existing
/// managed group is replaced at its current position (extras from tampering
/// are dropped); only a missing one is appended. Codex trust records are keyed
/// by position inside the event array, so shifting *any* group's index — ours
/// or a user's — invalidates its trust; in-place replacement keeps every
/// index stable across a (re)registration.
fn apply_managed_entries(root: &mut Value, hooks_dir: &Path) -> io::Result<()> {
    let obj = root.as_object_mut().expect("checked by read_hooks_json");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "`hooks` is not a JSON object — refusing to rewrite it",
        ));
    };
    for (event, script, timeout) in MANAGED_HOOKS {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("`hooks.{event}` is not a JSON array — refusing to rewrite it"),
            ));
        };
        let desired = managed_group(hooks_dir, script, *timeout);
        match groups.iter().position(|g| is_managed(g, hooks_dir)) {
            Some(first) => {
                groups[first] = desired;
                // Duplicates only exist after manual tampering; dropping them
                // shifts later indices, but that state was already broken.
                let mut idx = first + 1;
                while idx < groups.len() {
                    if is_managed(&groups[idx], hooks_dir) {
                        groups.remove(idx);
                    } else {
                        idx += 1;
                    }
                }
            }
            None => groups.push(desired),
        }
    }
    Ok(())
}

/// One totsuka matcher group: a single command hook running `script`.
fn managed_group(hooks_dir: &Path, script: &str, timeout: u64) -> Value {
    json!({
        "hooks": [{
            "type": "command",
            "command": hooks_dir.join(script).to_string_lossy(),
            "timeout": timeout,
            "statusMessage": "totsuka orchestrator hook"
        }]
    })
}

/// Whether a matcher group is totsuka-managed: any of its hooks' command
/// strings references a path **inside** the totsuka hooks dir. The trailing
/// separator anchors the match so a sibling dir sharing the prefix (e.g.
/// `…/hooks-mine/`) is never mistaken for ours.
fn is_managed(group: &Value, hooks_dir: &Path) -> bool {
    let dir = format!("{}/", hooks_dir.display());
    group["hooks"].as_array().into_iter().flatten().any(|hook| {
        hook["command"]
            .as_str()
            .is_some_and(|cmd| cmd.contains(&dir))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("totsuka-codex-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A Paths whose data dir (and thus hooks dir) lives under `base`.
    fn paths_under(base: &Path) -> Paths {
        let base = base.to_path_buf();
        Paths::from_env(move |k| match k {
            "HOME" => Some(base.to_string_lossy().into_owned()),
            "XDG_DATA_HOME" => Some(base.join("data").to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap()
    }

    fn codex_cfg() -> RootConfig {
        RootConfig::from_toml_str(
            r#"
default_tool = "codex"

[tools.codex]
kind = "codex"
"#,
        )
        .unwrap()
    }

    #[test]
    fn codex_home_prefers_env_override() {
        let env = |k: &str| match k {
            "CODEX_HOME" => Some("/custom/codex".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(codex_home(env), Some(PathBuf::from("/custom/codex")));
        let env = |k: &str| (k == "HOME").then(|| "/home/u".to_string());
        assert_eq!(codex_home(env), Some(PathBuf::from("/home/u/.codex")));
    }

    #[test]
    fn references_codex_checks_default_repo_and_workflow() {
        assert!(references_codex(&codex_cfg()));
        let repo = RootConfig::from_toml_str(
            r#"
[tools.cdx]
kind = "codex"

[[repositories]]
name = "r"
path = "/tmp/r"
tool = "cdx"
"#,
        )
        .unwrap();
        assert!(references_codex(&repo));
        let claude_only = RootConfig::from_toml_str("default_tool = \"claude\"").unwrap();
        assert!(!references_codex(&claude_only));
    }

    #[test]
    fn sync_skips_without_codex_reference_or_home() {
        let base = unique_dir("skip");
        let paths = paths_under(&base);
        let claude_only = RootConfig::from_toml_str("").unwrap();
        assert_eq!(
            sync_registration(Some(&base.join(".codex")), &paths, &claude_only).unwrap(),
            SyncOutcome::NotReferenced
        );
        // Referenced, but no codex home dir on disk (codex not installed).
        assert_eq!(
            sync_registration(Some(&base.join(".codex")), &paths, &codex_cfg()).unwrap(),
            SyncOutcome::NoCodexHome
        );
        assert_eq!(
            sync_registration(None, &paths, &codex_cfg()).unwrap(),
            SyncOutcome::NoCodexHome
        );
    }

    #[test]
    fn sync_creates_entries_and_is_idempotent() {
        let base = unique_dir("create");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(
            sync_registration(Some(&home), &paths, &codex_cfg()).unwrap(),
            SyncOutcome::Updated
        );
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(hooks_json_path(&home)).unwrap())
                .unwrap();
        for (event, script, timeout) in MANAGED_HOOKS {
            let hook = &root["hooks"][event][0]["hooks"][0];
            assert_eq!(hook["type"], "command");
            assert!(
                hook["command"].as_str().unwrap().ends_with(script),
                "{event} wires {script}"
            );
            assert_eq!(hook["timeout"], *timeout);
        }
        // Second sync: byte-identical, no rewrite.
        assert_eq!(
            sync_registration(Some(&home), &paths, &codex_cfg()).unwrap(),
            SyncOutcome::Unchanged
        );
        assert!(verify_registration(&home, &paths).is_empty());
    }

    #[test]
    fn sync_replaces_in_place_keeping_later_user_entries_at_their_index() {
        // A user group AFTER the managed one: the stale managed entry must be
        // replaced at index 0 (not removed-and-appended), or the user group's
        // index — and with it their codex trust record — would shift.
        let base = unique_dir("inplace");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        let hooks_dir = crate::hooks::hooks_dir(&paths);
        let existing = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": hooks_dir.join("on-stop.sh").to_string_lossy(), "timeout": 99 }] },
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/after-totsuka" }] }
                ]
            }
        });
        std::fs::write(
            hooks_json_path(&home),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();
        assert_eq!(
            sync_registration(Some(&home), &paths, &codex_cfg()).unwrap(),
            SyncOutcome::Updated
        );
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(hooks_json_path(&home)).unwrap())
                .unwrap();
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(
            stop[0]["hooks"][0]["timeout"], 30,
            "managed entry refreshed at its original index 0"
        );
        assert_eq!(
            stop[1]["hooks"][0]["command"], "/usr/local/bin/after-totsuka",
            "the user entry after ours keeps index 1"
        );
    }

    #[test]
    fn sync_preserves_user_entries_and_replaces_stale_managed_ones() {
        let base = unique_dir("merge");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        let hooks_dir = crate::hooks::hooks_dir(&paths);
        // A user entry first, plus a stale managed entry (old timeout) and an
        // unknown event that must survive untouched.
        let existing = json!({
            "description": "user file",
            "hooks": {
                "Stop": [
                    { "matcher": "x", "hooks": [{ "type": "command", "command": "/usr/local/bin/my-hook" }] },
                    { "hooks": [{ "type": "command", "command": hooks_dir.join("on-stop.sh").to_string_lossy(), "timeout": 99 }] }
                ],
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/guard" }] }
                ]
            }
        });
        std::fs::write(
            hooks_json_path(&home),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();
        assert_eq!(
            sync_registration(Some(&home), &paths, &codex_cfg()).unwrap(),
            SyncOutcome::Updated
        );
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(hooks_json_path(&home)).unwrap())
                .unwrap();
        assert_eq!(root["description"], "user file", "top-level fields survive");
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "user entry + one managed entry");
        assert_eq!(
            stop[0]["hooks"][0]["command"], "/usr/local/bin/my-hook",
            "user entry keeps index 0 (its codex trust key survives)"
        );
        assert_eq!(stop[1]["hooks"][0]["timeout"], 30, "stale entry replaced");
        assert_eq!(
            root["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/guard"
        );
        assert!(verify_registration(&home, &paths).is_empty());
    }

    #[test]
    fn sync_refuses_to_clobber_invalid_json() {
        let base = unique_dir("invalid");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(hooks_json_path(&home), "{ not json").unwrap();
        let err = sync_registration(Some(&home), &paths, &codex_cfg()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(hooks_json_path(&home)).unwrap(),
            "{ not json",
            "the unparseable file is left untouched"
        );
        assert!(!verify_registration(&home, &paths).is_empty());
    }

    #[test]
    fn verify_flags_missing_and_drifted_entries() {
        let base = unique_dir("verify");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        let issues = verify_registration(&home, &paths);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].problem, "missing");

        sync_registration(Some(&home), &paths, &codex_cfg()).unwrap();
        // Tamper: drop the managed Stop entry.
        let mut root: Value =
            serde_json::from_str(&std::fs::read_to_string(hooks_json_path(&home)).unwrap())
                .unwrap();
        root["hooks"]["Stop"] = json!([]);
        std::fs::write(
            hooks_json_path(&home),
            serde_json::to_string(&root).unwrap(),
        )
        .unwrap();
        let issues = verify_registration(&home, &paths);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].problem.contains("Stop"), "{}", issues[0].problem);
    }

    #[test]
    fn untrusted_events_reads_codex_trust_state() {
        let base = unique_dir("trust");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        sync_registration(Some(&home), &paths, &codex_cfg()).unwrap();

        // Nothing trusted yet: every managed event is reported.
        let untrusted = untrusted_events(&home, &paths).unwrap();
        assert_eq!(untrusted.len(), MANAGED_HOOKS.len());

        // Trust all but one (Stop stays untrusted); disabled counts as
        // untrusted even with a hash.
        let path = hooks_json_path(&home);
        let mut config = String::new();
        for (event, _, _) in &MANAGED_HOOKS[1..] {
            config.push_str(&format!(
                "[hooks.state.\"{}:{}:0:0\"]\ntrusted_hash = \"sha256:abc\"\n\n",
                path.display(),
                event_key(event)
            ));
        }
        std::fs::write(home.join("config.toml"), config).unwrap();
        let untrusted = untrusted_events(&home, &paths).unwrap();
        assert_eq!(untrusted, vec!["Stop".to_string()]);
    }

    #[test]
    fn untrusted_events_uses_the_index_after_user_entries() {
        let base = unique_dir("trust-idx");
        let paths = paths_under(&base);
        let home = base.join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        // A user Stop entry occupies index 0; ours lands at index 1.
        std::fs::write(
            hooks_json_path(&home),
            serde_json::to_string(&json!({
                "hooks": { "Stop": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/my-hook" }] }
                ]}
            }))
            .unwrap(),
        )
        .unwrap();
        sync_registration(Some(&home), &paths, &codex_cfg()).unwrap();
        let path = hooks_json_path(&home);
        let mut config = format!(
            "[hooks.state.\"{}:stop:1:0\"]\ntrusted_hash = \"sha256:abc\"\n\n",
            path.display()
        );
        for (event, _, _) in &MANAGED_HOOKS[1..] {
            config.push_str(&format!(
                "[hooks.state.\"{}:{}:0:0\"]\ntrusted_hash = \"sha256:abc\"\n\n",
                path.display(),
                event_key(event)
            ));
        }
        std::fs::write(home.join("config.toml"), config).unwrap();
        assert!(untrusted_events(&home, &paths).unwrap().is_empty());
    }
}
