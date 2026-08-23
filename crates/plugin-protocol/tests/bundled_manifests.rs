//! Every manifest this repository ships must be launchable by the
//! Orchestrator built from the same tree (F-54).
//!
//! # Why this is a test and not a review checklist
//!
//! `ai-docs/components/plugin-protocol.md` already carries the obligation:
//! "同梱プラグイン manifest の `protocol_version` 上限も同一 PR で見直す".
//! The rule was never wrong, and it is not reliably forgotten either — the
//! 0.4.0 bump (#413) moved all seven manifests, including the one outside
//! `plugins/`. The 0.5.0 bump (#501) moved six, and
//! `.claude/skills/live-e2e/assets/cfg/mock-agent.plugin.toml` was left at
//! `<0.5` while `PROTOCOL_VERSION` reached 0.5.0 (#526).
//!
//! That is the case for automating it rather than restating it: an obligation
//! discharged by counting by hand is not wrong on the bumps it happens to
//! catch, it is *unreliable*, and "同梱プラグイン" reads as "the plugins
//! directory" to whoever is counting. One miss is enough, because the miss is
//! silent — the manifest ships `enabled = false`, so the refusal only appears
//! the moment someone reaches for the mock.
//!
//! # Why here, and not in `scripts/arch-lint.sh`
//!
//! The invariant is exactly [`is_compatible_with_current`] — the same call
//! the launch gate makes. Restating it in awk would mean reimplementing
//! semver range matching, so the check would be a *different* predicate that
//! happens to agree on the two shapes currently in use (`>=a.b.c` and
//! `<d.e`) and diverge on `^`, `~`, `*` or a pre-release. A check weaker than
//! the thing it checks is worse than none, because it reads as coverage.
//!
//! Parsing through [`Manifest`] rather than grepping for the key is the same
//! argument one level down — but be precise about how far it reaches.
//! [`Manifest`] is `deny_unknown_fields`, so a typo in a **top-level** key
//! (`protocol_versoin`, `kindd`) fails here. [`Capabilities`] is **not**:
//! it carries only `#[serde(default)]`, deliberately, so that a retired
//! capability key on an old manifest is ignored rather than fatal (pinned by
//! `retired_capability_keys_are_tolerated_and_ignored`). A typo *inside*
//! `[capabilities]` therefore parses fine and silently leaves the flag false.
//! This check does not cover that, and nothing else does either.

use std::path::{Path, PathBuf};

use plugin_protocol::manifest::Manifest;
use plugin_protocol::version::{PROTOCOL_VERSION, is_compatible_with_current};

/// The workspace root (`crates/plugin-protocol/../..`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR is <root>/crates/plugin-protocol")
        .to_path_buf()
}

/// Whether `name` is a plugin manifest: `plugin.toml`, or the
/// `<something>.plugin.toml` form used where several manifests share a
/// directory (the live-e2e skill's `assets/cfg/`).
fn is_manifest(name: &str) -> bool {
    name == "plugin.toml" || name.ends_with(".plugin.toml")
}

/// Every manifest **this checkout** ships, depth-first.
///
/// Three prunes, and each one has to earn it — the point of the walk is that
/// a manifest cannot hide from it by living outside `plugins/`, so anything
/// skipped is a hole:
///
/// - `target/`: enormous, and holds only copies of what is already visible.
/// - `.git/`: holds no manifest.
/// - any subdirectory containing a `.git` entry: a nested checkout or a git
///   worktree, which is a *different* revision that happens to sit inside
///   this one. `.worktrees/` and `.claude/worktrees/` are both real paths
///   here (git-excluded, and the latter is auto-populated by worktree-isolated
///   subagents). Without this prune, a worktree parked on a pre-fix branch
///   fails the local test run with a message that reads as a manifest
///   violation in *this* tree. CI never sees it — a fresh clone has no
///   worktrees — which is exactly what makes it expensive to diagnose.
fn discover(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry under {}: {e}", dir.display()));
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|e| panic!("file_type {}: {e}", path.display()));
        if file_type.is_dir() {
            // `.git` is a file, not a directory, in a git worktree — so this
            // must test for existence, not for a directory.
            let is_nested_checkout = path.join(".git").exists();
            if name != "target" && name != ".git" && !is_nested_checkout {
                discover(&path, found);
            }
        } else if is_manifest(&name) {
            found.push(path);
        }
    }
}

/// Path relative to the repo root, for messages that can be pasted into an
/// editor.
fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn every_bundled_manifest_accepts_the_current_protocol_version() {
    let root = repo_root();
    let mut manifests = Vec::new();
    discover(&root, &mut manifests);
    manifests.sort();

    // Fail-close on the walk itself. A discovery bug that returns nothing —
    // a wrong root, a prune that ate too much — would otherwise report
    // "no violations", which is the one answer this test must never give by
    // accident. Every `plugins/*` directory must have contributed, and the
    // out-of-tree manifest that #526 was about must be among them.
    let mut expected: Vec<String> = std::fs::read_dir(root.join("plugins"))
        .expect("plugins/ exists")
        .map(|e| e.expect("plugins/ entry").path())
        .filter(|p| p.is_dir())
        .map(|p| {
            format!(
                "plugins/{}/plugin.toml",
                p.file_name().unwrap().to_string_lossy()
            )
        })
        .collect();
    expected.push(".claude/skills/live-e2e/assets/cfg/mock-agent.plugin.toml".to_string());
    expected.sort();

    let discovered: Vec<String> = manifests.iter().map(|p| rel(&root, p)).collect();
    for want in &expected {
        assert!(
            discovered.iter().any(|d| d == want),
            "manifest discovery missed `{want}` — the walk is broken, not the manifests. found: {discovered:?}"
        );
    }

    for path in &manifests {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Through the real deserializer: `deny_unknown_fields` makes a typo'd
        // key fail here rather than at the operator's next `plugin install`.
        let manifest = Manifest::from_toml_str(&text)
            .unwrap_or_else(|e| panic!("{} does not parse as a manifest: {e}", rel(&root, path)));
        assert!(
            is_compatible_with_current(&manifest.protocol_version),
            "{}: protocol_version = \"{}\" excludes PROTOCOL_VERSION {PROTOCOL_VERSION}, \
             so the Orchestrator built from this tree refuses to launch it (F-54) \
             → widen the bound (bundled manifests move together; see \
             ai-docs/components/plugin-protocol.md)",
            rel(&root, path),
            manifest.protocol_version,
        );
    }
}
