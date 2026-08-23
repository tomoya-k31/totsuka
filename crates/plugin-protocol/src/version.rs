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
/// `Task.thread_key` (removed in 0.3.0), the `diagnostics/snapshot` RPC, two `NotifierEvent`
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
///
/// 0.3.0: **breaking** — `Task.thread_key` is removed (#242/#264). It said
/// "these two tasks are one conversation"; since 0.2.4 that is what
/// [`Task.id`](crate::task::Task::id) itself means, so the field described a
/// relationship that no longer exists. Nothing breaks *on the wire* — `Task`
/// has no `deny_unknown_fields`, so a plugin that still sends it is accepted
/// and the value ignored — but a plugin that **reads** it no longer compiles,
/// and a plugin that relied on receiving it in `task/dispatch` now gets
/// nothing. That is a break in the type, so the version says so rather than
/// leaving third parties to discover it: a `<0.3` manifest is rejected at
/// launch by design (F-54), exactly as `^0.1` was at 0.2.0. The bundled
/// plugins move to `<0.4`.
///
/// 0.4.0: **breaking** — the three surfaces 0.3.0 was *supposed* to remove
/// (#411). 0.3.0 was a breaking bump, but it only deleted `Task.thread_key`;
/// everything else that said "removed at the next breaking bump" was left
/// behind, so the declarations and the code disagreed for a whole generation.
/// This bump makes them agree:
///
/// - `TaskDispatchParams.hook` and `HookLaunchSpec` (deprecated 0.2.3, #196)
///   are removed. [`ToolLaunchSpec`](crate::methods::ToolLaunchSpec) carries
///   the fully assembled argv, so `hook` only duplicated `--settings` and the
///   env in a form the plugin had to interpret.
/// - [`Capabilities`](crate::manifest::Capabilities)`::design_preview` is
///   removed (#356/#411). Nothing ever read it — neither the Orchestrator nor
///   any bundled plugin — so it was a declaration with no behaviour behind it
///   ([ADR-0030](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0030-herdr-pane-layout.md)).
///   The manifest key is tolerated on the wire (`Capabilities` has no
///   `deny_unknown_fields`) and ignored; a plugin that *reads* the field no
///   longer compiles.
///
/// The bundled plugins move to `<0.5`. **herdr additionally raises its lower
/// bound** to `>=0.2.3` — the version that introduced `tool_launch`. That is
/// what makes deleting its local argv assembly safe rather than merely
/// declared: an Orchestrator old enough to send no `tool_launch` can no longer
/// launch it at all (F-54), so the fallback is unreachable, not just
/// deprecated.
///
/// The floor follows the *dependency*, not the plugin kind. orca is an
/// `agent_ide` too and stays at `>=0.1.0`: it drives the `orca` CLI itself and
/// never reads `tool_launch`, so raising its bound would refuse orchestrators
/// it works with perfectly well. task_source and notifier stay put for the
/// same reason.
///
/// 0.4.1: [`TaskDispatchParams::repo_name`](crate::methods::TaskDispatchParams::repo_name)
/// (#417) — the repository the task was routed to, named as the operator named
/// it (`[[repositories]].name`), so an IDE plugin can show it. Additive and
/// optional: absent means the Orchestrator predates this version, and a plugin
/// must omit the name rather than refuse the dispatch.
///
/// **Patch, not minor.** In this 0.x scheme a minor bump strands every
/// manifest with a `<0.x` upper bound — the whole point of the 0.4.0
/// paragraph above — and nothing here requires a plugin to change. The
/// bundled manifests keep `<0.5` untouched.
///
/// 0.4.2: [`SessionReleaseResult::not_released`](crate::methods::SessionReleaseResult::not_released)
/// (#485) — why a release closed nothing. A bare `released: false` conflates
/// "the pane was already gone" with "the pane is alive and the identity guard
/// refused", which are opposites for a caller that is about to open a new pane
/// for the same task: only the second means the next dispatch collides with a
/// live pane. Additive and optional under the same contract as `repo_name` in
/// 0.4.1 — absent means the plugin predates this version, and the caller must
/// fall back to its pre-0.4.2 behaviour rather than assume either reason.
/// [`NotReleased`](crate::methods::NotReleased) carries a `#[serde(other)]`
/// catch-all so a reason added later does not make the whole response
/// undeserializable to this build. `<0.5` manifests keep matching.
/// 0.5.0: **breaking** — five declarations that nothing could ever act on are
/// removed, and the machine check that stops the next one is added (#496).
///
/// - [`Capabilities`](crate::manifest::Capabilities)`::plan_mode` and
///   `::task_submit` are removed. Neither was ever read by the Orchestrator.
///   `task_submit` additionally stopped carrying information at 0.2.0: with
///   `tasks/fetch` gone, every task_source that can launch at all is a push
///   source, so the flag could only ever be `true`.
/// - `Capabilities::resume_session` is replaced by
///   [`hook_completion`](crate::manifest::Capabilities::hook_completion).
///   Nothing read `resume_session` on its own — the resume decision is made
///   from the *tool's* capabilities, not the plugin's. Its only role was as
///   half of a `hook_capable()` heuristic that OR-ed it with
///   `diagnostics_snapshot`, so a plugin had to know an undocumented
///   convention to opt into hook completion. The new flag says what it means;
///   `diagnostics_snapshot` stays as its own flag because it gates a real RPC.
/// - `error_code::PROTOCOL_VERSION_MISMATCH` (-32001) and
///   `CAPABILITY_UNSUPPORTED` (-32002) are removed and their numbers retired.
///   Both were **unreachable by construction**: the compatibility check runs
///   host-side before the process is spawned, and the Orchestrator only calls
///   what a plugin declared.
///
/// **Nothing breaks on the wire.** `Capabilities` has no
/// `deny_unknown_fields`, so an old manifest's `plan_mode = true` is accepted
/// and ignored; an unknown error code was always just a number. What breaks is
/// code that *reads* the removed items, which is a type break — the same shape
/// as the 0.4.0 removal of `design_preview`. A `<0.5` manifest is rejected at
/// launch by design (F-54); the bundled plugins move to `<0.6`.
///
/// **The removals are not the point.** `scripts/arch-lint.sh` grew a
/// `declaration-consumed` check that fails when a `Capabilities` field is
/// never read by the Orchestrator, or an `error_code` constant is never
/// emitted or matched anywhere. Deleting these five without it would only
/// have reset a counter: `design_preview` sat unread for a whole generation
/// (0.4.0, #411), and `TaskDispatchParams.hook` took a full generation to go
/// after being marked for removal (0.2.3 → 0.4.0). An intentional
/// declaration-ahead-of-implementation is still possible — it goes in the
/// script's `DECLARATION_EXEMPT` list with a reason, which is what makes
/// "deliberate" distinguishable from "forgotten".
///
/// 0.5.1: [`InitializeResult::claimed_repos`](crate::methods::InitializeResult::claimed_repos)
/// (#542) — the repositories a task_source is the tracker for, and where an
/// item for each goes. This is the *forward* mapping repository → tracker; the
/// protocol only ever carried the reverse one (a task's `repo_hint`), so a
/// Slack-borne request could be routed to a repository but not to the board
/// that repository files into. Additive and optional under the same contract
/// as `repo_name` in 0.4.1: absent means the plugin predates this version and
/// claims nothing, which is never the same statement as "this repository has
/// no tracker". `<0.6` manifests keep matching.
///
/// **Patch, not minor**, for the reason spelled out at 0.4.1: a minor strands
/// every `<0.5`-bounded manifest, and nothing here requires a plugin to
/// change. The bundled manifests keep `<0.6` untouched.
pub const PROTOCOL_VERSION: &str = "0.5.1";

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
        assert_eq!(protocol_version(), Version::new(0, 5, 1));
    }

    #[test]
    fn compatible_requirement_matches() {
        // What the bundled plugins declare after the 0.4.0 boundary (#411):
        // task_source/notifier keep a wide lower bound, agent_ide plugins
        // require `tool_launch` (0.2.3) because their local argv fallback is
        // gone.
        for req in [">=0.1.6, <0.6", ">=0.2.3, <0.6"] {
            let parsed = VersionReq::parse(req).unwrap();
            assert!(
                is_compatible_with_current(&parsed),
                "{req} must be accepted by protocol 0.5.0"
            );
        }
    }

    #[test]
    fn incompatible_requirement_is_rejected() {
        // A plugin requiring >=1.0 does not work with a 0.x orchestrator.
        let req = VersionReq::parse(">=1.0.0").unwrap();
        assert!(!is_compatible_with_current(&req));
    }

    #[test]
    fn zero_three_manifests_are_stranded_by_the_zero_four_boundary() {
        // F-54 again (#411): 0.4.0 removes `TaskDispatchParams.hook` and
        // `Capabilities.design_preview`, so every `<0.4`-bounded manifest —
        // what the bundled plugins declared across the whole 0.3 generation —
        // is rejected at launch rather than left to read a field that is no
        // longer sent.
        for req in ["^0.2", ">=0.1.6, <0.4", ">=0.1.0, <0.4", ">=0.3.0, <0.4"] {
            let parsed = VersionReq::parse(req).unwrap();
            assert!(
                !is_compatible_with_current(&parsed),
                "{req} must be rejected by protocol 0.4.2"
            );
        }
    }

    #[test]
    fn herdrs_lower_bound_is_what_makes_the_fallback_unreachable() {
        // #411: deleting herdr's `agent_command`/`plan_args` argv assembly is
        // only safe because no Orchestrator that would have used it can launch
        // the plugin any more. That is a claim about the *manifest range*, not
        // about the code, so it is asserted here: `>=0.2.3` excludes every
        // release that predates `tool_launch`.
        let herdr = VersionReq::parse(">=0.2.3, <0.6").unwrap();
        for pre_tool_launch in ["0.1.0", "0.1.6", "0.2.0", "0.2.2"] {
            let v = Version::parse(pre_tool_launch).unwrap();
            assert!(
                !is_compatible(&herdr, &v),
                "herdr must refuse orchestrator {pre_tool_launch}, which sends no tool_launch"
            );
        }
        assert!(is_compatible(&herdr, &Version::new(0, 2, 3)));

        // The floor tracks the dependency, not the kind: orca is an agent_ide
        // too, reads no `tool_launch`, and keeps working with all of them.
        let orca = VersionReq::parse(">=0.1.0, <0.6").unwrap();
        assert!(is_compatible(&orca, &Version::new(0, 1, 0)));
        assert!(is_compatible_with_current(&orca));
    }

    #[test]
    fn an_additive_patch_strands_nobody_a_minor_would_have() {
        // #417 chose 0.4.1 over 0.5.0 for `repo_name`. The difference is not
        // stylistic: in this 0.x scheme the bundled manifests carry an upper
        // bound, so a minor bump refuses every one of them — which is
        // acceptable for a removal (#496) and pure cost for an added field no
        // plugin is required to read.
        let old_bound = VersionReq::parse(">=0.2.3, <0.5").unwrap();
        assert!(
            !is_compatible_with_current(&old_bound),
            "0.5.0 strands the previous generation's bound, by design"
        );
        let bundled = VersionReq::parse(">=0.2.3, <0.6").unwrap();
        assert!(is_compatible_with_current(&bundled), "0.5.0 is inside <0.6");
        assert!(
            !is_compatible(&bundled, &Version::new(0, 6, 0)),
            "and the same boundary is waiting for the next removal"
        );
    }

    /// The 0.5.0 boundary (#496), stated the way F-54 means it: every manifest
    /// from the 0.4 generation is refused at launch rather than left to read
    /// `plan_mode` / `task_submit` / `resume_session`, which no longer exist.
    #[test]
    fn zero_four_manifests_are_stranded_by_the_zero_five_boundary() {
        for req in ["^0.4", ">=0.1.6, <0.5", ">=0.2.3, <0.5", ">=0.4.0, <0.5"] {
            let parsed = VersionReq::parse(req).unwrap();
            assert!(
                !is_compatible_with_current(&parsed),
                "{req} must be rejected by protocol 0.5.0"
            );
        }
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
