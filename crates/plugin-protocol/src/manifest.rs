//! Plugin manifest (`plugin.toml`) and capability declaration (F-53, F-33).
//!
//! Every plugin ships a `plugin.toml` describing its identity, kind, own
//! version, the range of Orchestrator protocol versions it supports, and its
//! [`Capabilities`]. The Orchestrator reads this before launching the plugin to
//! check protocol compatibility (F-54) and to request only supported features.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// The kind of a plugin (F-50). New kinds may be added in a future minor
/// version; unknown kinds are rejected rather than silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// Provides tasks (GitHub, Notion, …).
    TaskSource,
    /// Drives an agent IDE (herdr, orca, …).
    AgentIde,
    /// Delivers notifications.
    Notifier,
}

/// Output policies a task source plugin can fulfil (F-83), declared as a
/// capability so the Orchestrator only routes supported outputs to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputCapability {
    /// Can write results back to the source (`result/publish`).
    Source,
}

/// Capabilities a plugin declares (F-33). The Orchestrator requests only the
/// features advertised here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    /// Supports plan (design) mode (F-36).
    pub plan_mode: bool,
    /// Can render a design preview in a side pane / screen (F-34).
    pub design_preview: bool,
    /// Supports pane control.
    pub pane_control: bool,
    /// Streams state/log fragments via `state/subscribe` (F-38).
    pub state_stream: bool,
    /// Can resume a past agent session via
    /// `task/dispatch(resume_session_id)` (0.1.3). Defaults to `false`, so
    /// plugins that predate it (orca, mock, …) simply never advertise it.
    pub resume_session: bool,
    /// Answers `diagnostics/snapshot` with a pane screen capture (0.1.3,
    /// R-10). Defaults to `false` like `resume_session`.
    pub diagnostics_snapshot: bool,
    /// This task_source pushes tasks via `task/submit` (0.1.6). Since
    /// protocol 0.2.0 `tasks/fetch` no longer exists, so every task_source
    /// that can launch at all effectively declares this `true`; a manifest
    /// requiring only `^0.1` is rejected before this field is even
    /// consulted (F-54). Defaults to `false` for the historical case of a
    /// pre-0.1.6 manifest parsed by an older orchestrator.
    pub task_submit: bool,
    /// Output policies this (task source) plugin can fulfil.
    pub outputs: Vec<OutputCapability>,
}

impl Capabilities {
    /// Whether this agent reports completion through Claude Code hooks (#131).
    /// There is no dedicated flag: the 0.1.3 `resume_session` /
    /// `diagnostics_snapshot` pair is the de-facto signal, and this is the
    /// single source of that rule — the runtime launch decision and the
    /// `[hooks].auth_token_ref` config advisory must agree on it.
    pub fn hook_capable(&self) -> bool {
        self.resume_session || self.diagnostics_snapshot
    }
}

/// A parsed `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Plugin instance/binary name.
    pub name: String,
    /// Plugin kind.
    pub kind: PluginKind,
    /// The plugin's own version.
    pub version: Version,
    /// Range of Orchestrator protocol versions this plugin supports (F-54).
    pub protocol_version: VersionReq,
    /// Declared capabilities (F-33).
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// Errors from parsing a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// `plugin.toml` failed to parse.
    #[error("failed to parse plugin.toml: {0}")]
    Parse(#[from] toml::de::Error),
}

impl Manifest {
    /// Parse a `plugin.toml` document.
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestError> {
        Ok(toml::from_str(s)?)
    }

    /// Whether this plugin is compatible with `orchestrator_protocol` (F-54).
    pub fn is_compatible_with(&self, orchestrator_protocol: &Version) -> bool {
        crate::version::is_compatible(&self.protocol_version, orchestrator_protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
name = "herdr"
kind = "agent_ide"
version = "1.2.0"
protocol_version = "^0.1"

[capabilities]
plan_mode = true
design_preview = true
state_stream = true
"#;

    #[test]
    fn parses_manifest() {
        let m = Manifest::from_toml_str(EXAMPLE).unwrap();
        assert_eq!(m.name, "herdr");
        assert_eq!(m.kind, PluginKind::AgentIde);
        assert_eq!(m.version, Version::new(1, 2, 0));
        assert!(m.capabilities.plan_mode);
        assert!(m.capabilities.state_stream);
        assert!(!m.capabilities.pane_control);
        // The 0.1.3 flags default to false when absent (additive).
        assert!(!m.capabilities.resume_session);
        assert!(!m.capabilities.diagnostics_snapshot);
    }

    #[test]
    fn parses_0_1_3_capability_flags() {
        let m = Manifest::from_toml_str(
            r#"
name = "herdr"
kind = "agent_ide"
version = "1.3.0"
protocol_version = "^0.1"

[capabilities]
resume_session = true
diagnostics_snapshot = true
"#,
        )
        .unwrap();
        assert!(m.capabilities.resume_session);
        assert!(m.capabilities.diagnostics_snapshot);
    }

    #[test]
    fn hook_capable_follows_the_0_1_3_flags() {
        let cap = |resume_session, diagnostics_snapshot| Capabilities {
            resume_session,
            diagnostics_snapshot,
            ..Default::default()
        };
        assert!(cap(true, false).hook_capable());
        assert!(cap(false, true).hook_capable());
        assert!(cap(true, true).hook_capable());
        // orca / mock declare neither.
        assert!(!cap(false, false).hook_capable());
        assert!(!Capabilities::default().hook_capable());
    }

    #[test]
    fn protocol_compatibility_is_checked() {
        let m = Manifest::from_toml_str(EXAMPLE).unwrap();
        assert!(m.is_compatible_with(&Version::new(0, 1, 5)));
        // ^0.1 excludes 0.2 and 1.x.
        assert!(!m.is_compatible_with(&Version::new(0, 2, 0)));
        assert!(!m.is_compatible_with(&Version::new(1, 0, 0)));
    }

    #[test]
    fn task_source_declares_outputs() {
        let m = Manifest::from_toml_str(
            r#"
name = "github"
kind = "task_source"
version = "0.1.0"
protocol_version = "^0.1"

[capabilities]
outputs = ["source"]
"#,
        )
        .unwrap();
        assert_eq!(m.kind, PluginKind::TaskSource);
        assert_eq!(m.capabilities.outputs, vec![OutputCapability::Source]);
        // 0.1.6: absent `task_submit` defaults to false (additive) — the
        // plugin keeps being polled.
        assert!(!m.capabilities.task_submit);
    }

    #[test]
    fn push_source_declares_task_submit() {
        let m = Manifest::from_toml_str(
            r#"
name = "slack"
kind = "task_source"
version = "0.2.0"
protocol_version = ">=0.1.6, <0.4"

[capabilities]
task_submit = true
outputs = ["source"]
"#,
        )
        .unwrap();
        assert!(m.capabilities.task_submit);
        assert!(m.is_compatible_with(&Version::new(0, 1, 6)));
        // A push-only plugin survives the 0.2.0 fetch removal and, with the
        // bound raised to `<0.4`, the 0.3.0 removal of `Task.thread_key`…
        assert!(m.is_compatible_with(&Version::new(0, 2, 0)));
        assert!(m.is_compatible_with(&Version::new(0, 3, 0)));
        // …while staying honest about a hypothetical 0.4.
        assert!(!m.is_compatible_with(&Version::new(0, 4, 0)));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let err = Manifest::from_toml_str(
            r#"
name = "x"
kind = "teleporter"
version = "0.1.0"
protocol_version = "^0.1"
"#,
        );
        assert!(err.is_err());
    }
}
