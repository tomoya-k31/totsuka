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
    /// Supports pane control.
    pub pane_control: bool,
    /// Streams state/log fragments via `state/subscribe` (F-38).
    pub state_stream: bool,
    /// Reports completion through the agent tool's hooks rather than through
    /// the state stream alone (#131, 0.5.0).
    ///
    /// Replaces the `resume_session` / `diagnostics_snapshot` pair that used
    /// to stand in for this (`hook_capable()`). That pair was a *de-facto*
    /// signal — neither flag says "hooks" — so a plugin author had to know the
    /// convention to opt in, and a plugin that could resume sessions without
    /// speaking hooks had no way to say so. This flag says what it means.
    pub hook_completion: bool,
    /// Answers `diagnostics/snapshot` with a pane screen capture (0.1.3,
    /// R-10).
    ///
    /// Deliberately **not** folded into
    /// [`hook_completion`](Self::hook_completion): it gates a real RPC, and
    /// merging the two would silently require every hook-completing agent to
    /// also answer snapshots.
    pub diagnostics_snapshot: bool,
    /// Output policies this (task source) plugin can fulfil.
    pub outputs: Vec<OutputCapability>,
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
pane_control = true
state_stream = true
"#;

    #[test]
    fn parses_manifest() {
        let m = Manifest::from_toml_str(EXAMPLE).unwrap();
        assert_eq!(m.name, "herdr");
        assert_eq!(m.kind, PluginKind::AgentIde);
        assert_eq!(m.version, Version::new(1, 2, 0));
        assert!(m.capabilities.pane_control);
        assert!(m.capabilities.state_stream);
        // Absent flags default to false, which is what lets a manifest
        // written against an older protocol keep parsing.
        assert!(!m.capabilities.hook_completion);
        assert!(!m.capabilities.diagnostics_snapshot);
    }

    #[test]
    fn parses_the_hook_and_snapshot_flags() {
        let m = Manifest::from_toml_str(
            r#"
name = "herdr"
kind = "agent_ide"
version = "1.3.0"
protocol_version = "^0.5"

[capabilities]
hook_completion = true
diagnostics_snapshot = true
"#,
        )
        .unwrap();
        assert!(m.capabilities.hook_completion);
        assert!(m.capabilities.diagnostics_snapshot);
    }

    /// 0.5.0 retired `plan_mode` / `task_submit` / `resume_session`. The
    /// manifest must keep **parsing** them — `Capabilities` has no
    /// `deny_unknown_fields`, so an older plugin's manifest is accepted and
    /// the keys ignored. What breaks is code that *reads* them, which is a
    /// type break and is why the protocol version moved.
    #[test]
    fn retired_capability_keys_are_tolerated_and_ignored() {
        let m = Manifest::from_toml_str(
            r#"
name = "legacy"
kind = "agent_ide"
version = "0.1.0"
protocol_version = "^0.5"

[capabilities]
plan_mode = true
task_submit = true
resume_session = true
state_stream = true
"#,
        )
        .unwrap();
        // The retired keys parsed without error…
        assert!(m.capabilities.state_stream);
        // …and none of them turned into the flag that replaced one of them.
        // A plugin wanting hook completion must say so with the new name,
        // which is the point of retiring a de-facto signal.
        assert!(!m.capabilities.hook_completion);
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
    }

    #[test]
    fn a_task_source_manifest_survives_every_boundary_so_far() {
        let m = Manifest::from_toml_str(
            r#"
name = "slack"
kind = "task_source"
version = "0.2.0"
protocol_version = ">=0.1.6, <0.6"

[capabilities]
outputs = ["source"]
"#,
        )
        .unwrap();
        assert!(m.is_compatible_with(&Version::new(0, 1, 6)));
        // A push-only plugin survives the 0.2.0 fetch removal, the 0.3.0
        // removal of `Task.thread_key`, the 0.4.0 removal of
        // `TaskDispatchParams.hook` (#411), and — with the bound raised to
        // `<0.6` (#496) — the 0.5.0 removal of `Capabilities::task_submit`.
        // A task_source read none of them…
        assert!(m.is_compatible_with(&Version::new(0, 2, 0)));
        assert!(m.is_compatible_with(&Version::new(0, 3, 0)));
        assert!(m.is_compatible_with(&Version::new(0, 4, 0)));
        assert!(m.is_compatible_with(&crate::version::protocol_version()));
        // …while staying honest about the next boundary, whatever it removes.
        assert!(!m.is_compatible_with(&Version::new(0, 6, 0)));
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
