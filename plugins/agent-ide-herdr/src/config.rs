//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/herdr.toml` as JSON (F-64/F-65).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// herdr agent_ide settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrConfig {
    /// Explicit socket path. Highest precedence when set.
    #[serde(default)]
    pub socket_path: Option<String>,
    /// Named herdr session (resolves to
    /// `~/.config/herdr/sessions/<name>/herdr.sock`). Used when `socket_path`
    /// is unset.
    #[serde(default)]
    pub session: Option<String>,
    /// How the dispatched task's panes are arranged (#356).
    #[serde(default)]
    pub layout: LayoutConfig,
    /// Overrides for the program-basename → herdr `kind` mapping
    /// ([ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-1).
    ///
    /// herdr protocol 17 picks the executable itself from `agent.start`'s
    /// `kind`, so the plugin translates
    /// [`ToolLaunchSpec::program`](plugin_protocol::methods::ToolLaunchSpec) into that
    /// vocabulary by its file name. A wrapper script (`my-claude`) has a name
    /// herdr does not know, and this table is how it is told:
    ///
    /// ```toml
    /// [kind_map]
    /// my-claude = "claude"
    /// ```
    ///
    /// Keys are compared against the program's **file name**, not its path.
    /// Nothing is validated here — herdr rejects an unknown `kind` at
    /// `agent.start`, and duplicating its 21-value enum in this crate would
    /// only give the two a chance to disagree.
    #[serde(default)]
    pub kind_map: HashMap<String, String>,
    /// Per-request timeout (seconds) for herdr socket calls.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

/// How `task/dispatch` arranges the panes of the workspace it creates (#356).
///
/// Before this existed the plugin specified nothing, so herdr's own default
/// leaked through: the agent got half the screen and the workspace's initial
/// shell — which nobody asked for and which carried the hook environment — got
/// the other half. These three knobs replace that accident with a choice.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    /// Whether a companion shell pane sits beside the agent. `false` gives the
    /// agent the whole workspace, and makes
    /// [`direction`](Self::direction)/[`ratio`](Self::ratio) irrelevant.
    #[serde(default = "default_layout_shell")]
    pub shell: bool,
    /// Which way the workspace is split.
    #[serde(default = "default_layout_direction")]
    pub direction: SplitDirection,
    /// The **agent** side's share of the split (the shell gets the rest).
    ///
    /// Deliberately unvalidated: herdr owns what a ratio means, so a value it
    /// rejects is reported by herdr rather than second-guessed here (a clamp
    /// would silently draw something the operator did not ask for). A ratio
    /// herdr refuses costs the shell pane, not the task — see
    /// [`HerdrAgent::dispatch`](crate::agent::HerdrAgent::dispatch).
    #[serde(default = "default_layout_ratio")]
    pub ratio: f64,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            shell: default_layout_shell(),
            direction: default_layout_direction(),
            ratio: default_layout_ratio(),
        }
    }
}

/// herdr's `SplitDirection`, mirrored verbatim — it has exactly these two
/// values (there is no `up`/`left`; the split always grows down or right).
///
/// Unlike [`ratio`](LayoutConfig::ratio) this **is** validated here, because it
/// can be: a closed two-value enum lets a typo fail loudly at `initialize`
/// with "unknown variant `up`" instead of degrading a pane at dispatch time,
/// hours later, into a warning nobody is watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// Split below: the agent keeps the top `ratio`.
    Down,
    /// Split to the right: the agent keeps the left `ratio`.
    Right,
}

impl SplitDirection {
    /// The wire value herdr's `SplitDirection` enum expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Right => "right",
        }
    }
}

impl HerdrConfig {
    /// Resolve the herdr socket path following the documented precedence
    /// (§transport): explicit `socket_path` > `session` name >
    /// `HERDR_SOCKET_PATH` env > `HERDR_SESSION` env > default
    /// `~/.config/herdr/herdr.sock`.
    pub fn resolve_socket_path(&self) -> PathBuf {
        if let Some(path) = &self.socket_path {
            return PathBuf::from(path);
        }
        if let Some(name) = &self.session {
            return session_socket(name);
        }
        if let Ok(path) = std::env::var("HERDR_SOCKET_PATH") {
            return PathBuf::from(path);
        }
        if let Ok(name) = std::env::var("HERDR_SESSION") {
            return session_socket(&name);
        }
        herdr_config_dir().join("herdr.sock")
    }
}

/// Config keys this plugin removed in protocol 0.4.0 (#411), paired with what
/// to do instead.
///
/// [`HerdrConfig`] is `deny_unknown_fields`, so simply deleting the fields
/// would turn a `herdr.toml` that worked yesterday into
/// `unknown field 'agent_command', expected one of ...` — loud, but it does not
/// say the key was *removed*, when it was removed, or what replaced it. These
/// pairs exist to answer that, and [`removed_keys_in`] is what reports them.
const REMOVED_KEYS: &[(&str, &str)] = &[
    (
        "agent_command",
        "the Orchestrator resolves the full argv itself since protocol 0.2.3 \
         (#196); set `[tools]`/`default_tool` in the orchestrator config instead",
    ),
    (
        "plan_args",
        "same as `agent_command` — plan flags come from the orchestrator's tool \
         registry (`[tools.<name>].plan_args`), not from this file",
    ),
    (
        "design_preview",
        "it never did anything (#356): nothing read it, so no drawing ever \
         changed. Pane arrangement is `[layout]`",
    ),
];

/// The removed keys (#411) present in a raw plugin-config object, rendered as
/// operator-facing lines. Empty when the config is clean — including when it is
/// not an object at all, which is a different error and reported by serde.
pub fn removed_keys_in(config: &serde_json::Value) -> Vec<String> {
    let Some(map) = config.as_object() else {
        return Vec::new();
    };
    REMOVED_KEYS
        .iter()
        .filter(|(key, _)| map.contains_key(*key))
        .map(|(key, advice)| {
            format!("`{key}` was removed in protocol 0.4.0 (#411): {advice}. Delete the key.")
        })
        .collect()
}

/// The herdr config directory: `$XDG_CONFIG_HOME/herdr` or `~/.config/herdr`.
fn herdr_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("herdr");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("herdr")
}

/// The socket path for a named session.
fn session_socket(name: &str) -> PathBuf {
    herdr_config_dir()
        .join("sessions")
        .join(name)
        .join("herdr.sock")
}

fn default_layout_shell() -> bool {
    true
}
/// Agent above, shell below. Vertical stacking gives the agent the full
/// terminal width — the measured 123 columns of a 50/50 side-by-side split is
/// where a TUI starts wrapping its own chrome.
fn default_layout_direction() -> SplitDirection {
    SplitDirection::Down
}
fn default_layout_ratio() -> f64 {
    0.8
}
fn default_request_timeout() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> HerdrConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(serde_json::json!({}));
        assert_eq!(cfg.request_timeout_secs, 30);
        // #356: an operator who writes no `[layout]` gets the agent stacked
        // above a small shell — NOT herdr's own 50/50 side-by-side default,
        // which is what leaked through before this table existed.
        assert_eq!(cfg.layout, LayoutConfig::default());
        assert!(cfg.layout.shell);
        assert_eq!(cfg.layout.direction, SplitDirection::Down);
        assert_eq!(cfg.layout.ratio, 0.8);
    }

    #[test]
    fn layout_keys_are_independently_defaulted() {
        // Writing one knob must not reset the others: `[layout]` with only a
        // ratio keeps the default direction and still asks for a shell.
        let cfg = parse(serde_json::json!({ "layout": { "ratio": 0.5 } }));
        assert_eq!(cfg.layout.ratio, 0.5);
        assert_eq!(cfg.layout.direction, SplitDirection::Down);
        assert!(cfg.layout.shell);

        let cfg = parse(serde_json::json!({ "layout": { "shell": false } }));
        assert!(!cfg.layout.shell);
        // The other two keep their defaults even though they are now moot.
        assert_eq!(cfg.layout.direction, SplitDirection::Down);
        assert_eq!(cfg.layout.ratio, 0.8);
    }

    #[test]
    fn split_direction_accepts_only_herdrs_two_values() {
        assert_eq!(
            parse(serde_json::json!({ "layout": { "direction": "right" } }))
                .layout
                .direction,
            SplitDirection::Right
        );
        // `up`/`left` do not exist in herdr's SplitDirection. Rejecting them at
        // `initialize` is the whole point of typing this key: the alternative
        // is a dispatch-time warning that costs the shell pane silently.
        let err = serde_json::from_value::<HerdrConfig>(serde_json::json!({
            "layout": { "direction": "up" }
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "got {err}");
    }

    #[test]
    fn unknown_layout_field_is_rejected() {
        // `deny_unknown_fields` has to hold inside the nested table too, or a
        // typo'd knob would be accepted and silently do nothing.
        let err = serde_json::from_value::<HerdrConfig>(serde_json::json!({
            "layout": { "raito": 0.8 }
        }))
        .unwrap_err();
        assert!(err.to_string().contains("raito"), "got {err}");
    }

    #[test]
    fn an_out_of_range_ratio_is_passed_through_untouched() {
        // Deliberately unvalidated (#356): herdr owns the semantics of a ratio,
        // so this parses and is sent as-is rather than clamped into something
        // the operator never wrote.
        assert_eq!(
            parse(serde_json::json!({ "layout": { "ratio": 1.5 } }))
                .layout
                .ratio,
            1.5
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = serde_json::from_value::<HerdrConfig>(serde_json::json!({
            "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn explicit_socket_path_wins() {
        let cfg =
            parse(serde_json::json!({ "socket_path": "/tmp/custom.sock", "session": "work" }));
        assert_eq!(cfg.resolve_socket_path(), PathBuf::from("/tmp/custom.sock"));
    }

    #[test]
    fn session_name_resolves_under_config_dir() {
        let cfg = parse(serde_json::json!({ "session": "work" }));
        let path = cfg.resolve_socket_path();
        assert!(
            path.ends_with("herdr/sessions/work/herdr.sock"),
            "got {path:?}"
        );
    }

    #[test]
    fn the_removed_keys_are_reported_by_name_not_as_unknown_fields() {
        // #411: `HerdrConfig` is `deny_unknown_fields`, so a herdr.toml that
        // still sets these would otherwise fail with serde's
        // `unknown field ...`, which does not say the key was removed, when,
        // or what replaced it.
        for key in ["agent_command", "plan_args", "design_preview"] {
            let found = removed_keys_in(&serde_json::json!({ key: "whatever" }));
            assert_eq!(found.len(), 1, "{key}");
            assert!(found[0].contains(key), "{}", found[0]);
            assert!(found[0].contains("0.4.0"), "{}", found[0]);
        }
        // All three at once are reported together, so one round trip tells the
        // operator everything to delete.
        let all = removed_keys_in(&serde_json::json!({
            "agent_command": "claude",
            "plan_args": ["--permission-mode", "plan"],
            "design_preview": "side_pane",
        }));
        assert_eq!(all.len(), 3);
        // A clean config, and a non-object, both report nothing.
        assert!(removed_keys_in(&serde_json::json!({ "session": "work" })).is_empty());
        assert!(removed_keys_in(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn a_config_that_still_sets_a_removed_key_does_not_parse() {
        // The tombstone above is the *message*; this is the behaviour. Both
        // matter: a key silently accepted and ignored is how `design_preview`
        // survived four minor versions doing nothing (#356).
        let err =
            serde_json::from_value::<HerdrConfig>(serde_json::json!({ "agent_command": "claude" }))
                .unwrap_err();
        assert!(err.to_string().contains("agent_command"), "got {err}");
    }
}
