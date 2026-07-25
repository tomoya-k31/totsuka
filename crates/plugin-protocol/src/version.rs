//! Protocol versioning (§10.2, F-54).
//!
//! The plugin protocol has its **own** SemVer, independent of the totsuka
//! application version. A plugin's [`Manifest`](crate::manifest::Manifest)
//! declares the range of Orchestrator protocol versions it supports as a SemVer
//! requirement; the Orchestrator advertises [`PROTOCOL_VERSION`] and refuses to
//! talk to a plugin whose range excludes it (breaking changes bump the major
//! and keep one prior generation supported).

use semver::{Version, VersionReq};

/// The protocol version this crate implements.
///
/// 0.1.1: `InitializeParams.repositories` (#109) — additive and optional,
/// so `^0.1` manifests keep matching. In this 0.x scheme the patch level
/// marks backward-compatible additions (a 0.2 would break every `^0.1`
/// manifest for a change no plugin is required to adopt); breaking changes
/// still bump the major/minor per the caret semantics below.
///
/// 0.1.2: `InitializeParams.llm` (#119) — the orchestrator's `[llm]`
/// supplied to task_source plugins as a classification default; additive
/// and optional under the same contract as `repositories`.
///
/// 0.1.3: `TaskDispatchParams.job_id`/`resume_session_id`/`hook`,
/// `Task.thread_key`, the `diagnostics/snapshot` RPC, two `NotifierEvent`
/// variants (`escalated`, `verification_pending`), and two `Capabilities`
/// flags (`resume_session`, `diagnostics_snapshot`) (#132) — all additive,
/// `^0.1`-compatible.
///
/// 0.1.4: the `session/focus` RPC (F-94 click-to-focus, #155) — additive,
/// gated on the existing `pane_control` capability (no new flag), so plugins
/// that never declare it are simply never called.
///
/// 0.1.5: `Task.instructions` — task-source-owned agent instructions,
/// separated from the human-visible `body` so hosts can deliver them
/// out-of-band (e.g. invisible prompt-context injection); additive and
/// optional under the same contract as the fields above.
///
/// 0.1.6: push ingestion — the `task/submit` RPC (P→O), the
/// `Capabilities.task_submit` flag, and `InitializeParams.triggers` /
/// `poll_interval_secs` (#183). All additive and `^0.1`-compatible.
/// `tasks/fetch` is deprecated from this version and scheduled for removal
/// in 0.2.0 (which will strand `^0.1` manifests by design, F-54).
///
/// 0.2.0: **breaking** — `tasks/fetch` (and its `TasksFetchParams`/
/// `TasksFetchResult` types) is removed, ending ADR-0008 Phase C. Every
/// task_source is now push-only (`task/submit`); the Orchestrator no longer
/// polls at all. A `^0.1` manifest is rejected at launch by design (F-54);
/// a push-only plugin declaring `>=0.1.6, <0.3` keeps working across this
/// boundary.
///
/// 0.2.1: the `session/release` RPC (#210) — close a finished session's pane
/// when the worktree cleanup decided to remove its worktree. Additive, gated
/// on the existing `pane_control` capability (no new flag, same contract as
/// `session/focus` in 0.1.4): plugins that never declare it are simply never
/// called, and every `<0.3`-bounded manifest keeps matching.
///
/// 0.2.2: the `session/list` RPC (#211) — enumerate the plugin's own live
/// panes so `doctor` can detect orphans (#210's cleanup linkage can break:
/// manual `git worktree remove`, refused releases, crashes). Additive, gated
/// on the same `pane_control` capability; `<0.3` manifests keep matching.
///
/// 0.2.3: `TaskDispatchParams.tool_launch` (`ToolLaunchSpec`, #196) — the
/// Orchestrator-resolved agent-CLI argv/env, replacing plugin-side command
/// assembly so the AI tool (Claude/Codex/OpenCode) is selected per
/// repo/workflow in core. Additive; `TaskDispatchParams.hook` is deprecated
/// from this version (still sent) and removed at the next breaking bump.
///
/// 0.2.4: task identity becomes conversation identity (#242). `Task.id` is a
/// *conversation*, `Task.message_key` identifies one delivery within it, the
/// `task/lookup` RPC lets a source ask whether a conversation is already known
/// before submitting to it, and
/// [`SESSION_UNRESUMABLE`](crate::error_code::SESSION_UNRESUMABLE) lets an
/// agent report an unusable session so the Orchestrator can retry without
/// resuming. All additive: `message_key` absent means "one message = one
/// task", `task/lookup` is optional for both sides, and the new error code is
/// only ever returned, never required. `<0.3` manifests keep matching.
pub const PROTOCOL_VERSION: &str = "0.2.4";

/// [`PROTOCOL_VERSION`] parsed into a [`Version`].
pub fn protocol_version() -> Version {
    Version::parse(PROTOCOL_VERSION).expect("PROTOCOL_VERSION is a valid semver literal")
}

/// Whether `orchestrator` satisfies the plugin's declared requirement.
pub fn is_compatible(plugin_requirement: &VersionReq, orchestrator: &Version) -> bool {
    plugin_requirement.matches(orchestrator)
}

/// Whether a plugin declaring `requirement` is compatible with this crate's
/// [`PROTOCOL_VERSION`].
pub fn is_compatible_with_current(requirement: &VersionReq) -> bool {
    is_compatible(requirement, &protocol_version())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_parses() {
        assert_eq!(protocol_version(), Version::new(0, 2, 4));
    }

    #[test]
    fn compatible_requirement_matches() {
        // Only a plugin that declared a range reaching past the 0.2.0
        // boundary survives it — the push-only requirement introduced
        // alongside `task/submit` (0.1.6) for exactly this purpose.
        let req = VersionReq::parse(">=0.1.6, <0.3").unwrap();
        assert!(is_compatible_with_current(&req));
    }

    #[test]
    fn incompatible_requirement_is_rejected() {
        // A plugin requiring >=1.0 does not work with 0.2.0.
        let req = VersionReq::parse(">=1.0.0").unwrap();
        assert!(!is_compatible_with_current(&req));
    }

    #[test]
    fn zero_one_manifests_are_stranded_by_the_zero_two_boundary() {
        // F-54, by design (ADR-0008 Phase C): every `^0.1`-family
        // requirement — including one bounded past an individual 0.1.x
        // feature — excludes 0.2.0 and is now rejected at launch. A plugin
        // that never adopted `task/submit` must upgrade, not silently keep
        // running against a fetch path that no longer exists.
        for req in [
            "^0.1",
            ">=0.1.1, <0.2",
            ">=0.1.2, <0.2",
            ">=0.1.3, <0.2",
            ">=0.1.4, <0.2",
            ">=0.1.5, <0.2",
            ">=0.1.6, <0.2",
        ] {
            let req = VersionReq::parse(req).unwrap();
            assert!(
                !is_compatible_with_current(&req),
                "{req} must be rejected by protocol 0.2.0"
            );
        }
    }
}
