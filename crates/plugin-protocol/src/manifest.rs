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
