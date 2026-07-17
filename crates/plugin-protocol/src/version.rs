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
pub const PROTOCOL_VERSION: &str = "0.1.3";

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
        assert_eq!(protocol_version(), Version::new(0, 1, 3));
    }

    #[test]
    fn compatible_requirement_matches() {
        // A plugin supporting ^0.1 works with 0.1.x — the additive 0.1.1
        // (InitializeParams.repositories), 0.1.2 (InitializeParams.llm) and
        // 0.1.3 (hook/resume/diagnostics) must not strand `^0.1` manifests.
        let req = VersionReq::parse("^0.1").unwrap();
        assert!(is_compatible_with_current(&req));
        // A plugin that *requires* one of the additive supplies can say so.
        let req = VersionReq::parse(">=0.1.1, <0.2").unwrap();
        assert!(is_compatible_with_current(&req));
        let req = VersionReq::parse(">=0.1.2, <0.2").unwrap();
        assert!(is_compatible_with_current(&req));
        let req = VersionReq::parse(">=0.1.3, <0.2").unwrap();
        assert!(is_compatible_with_current(&req));
    }

    #[test]
    fn incompatible_requirement_is_rejected() {
        // A plugin requiring >=1.0 does not work with 0.1.0.
        let req = VersionReq::parse(">=1.0.0").unwrap();
        assert!(!is_compatible_with_current(&req));
    }
}
