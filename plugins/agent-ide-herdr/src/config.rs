//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/herdr.toml` as JSON (F-64/F-65).

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
    /// The agent CLI launched in each pane (F-31). Split on whitespace; the
    /// first token is the program, the rest are base arguments.
    #[serde(default = "default_agent_command")]
    pub agent_command: String,
    /// Extra argument appended for plan/design mode (F-36). For Claude Code the
    /// default puts the CLI in its read-oriented plan permission mode.
    #[serde(default = "default_plan_args")]
    pub plan_args: Vec<String>,
    /// How a design preview is surfaced (F-34), e.g. `side_pane`.
    ///
    /// **Deprecated and inert** (#356): nothing reads it — neither this plugin
    /// nor the Orchestrator — so setting it has never changed what is drawn.
    /// The pane a dispatched task gets is decided by [`layout`](Self::layout);
    /// `side_pane` here does **not** mean "put the agent beside something".
    /// Scheduled for removal at the next breaking bump, together with
    /// `agent_command`/`plan_args`.
    #[serde(default = "default_design_preview")]
    pub design_preview: String,
    /// How the dispatched task's panes are arranged (#356).
    #[serde(default)]
    pub layout: LayoutConfig,
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

    /// The agent launch command line for `mode` (F-31): the base
    /// [`agent_command`](Self::agent_command) plus [`plan_args`](Self::plan_args)
    /// in plan mode. Returns `(program, args)`.
    ///
    /// **Deprecated fallback** (#196): since protocol 0.2.3 the Orchestrator
    /// resolves the full argv itself (`TaskDispatchParams.tool_launch`) and
    /// this is only used when dispatching from an older orchestrator that
    /// sends no `tool_launch`. Scheduled for removal at the next breaking
    /// protocol bump, together with `agent_command`/`plan_args`.
    ///
    /// When the Orchestrator supplies a hook launch spec (0.1.3), `hook_settings`
    /// is its settings path and `--settings <path>` is appended so Claude Code
    /// loads the workflow's hooks (H-03: `--resume` never inherits hooks, so the
    /// settings must be re-passed on every launch, resume included). When
    /// resuming a past session, `resume` is its agent-native id and
    /// `--resume <id>` is appended (both flags coexist).
    pub fn launch_command(
        &self,
        plan: bool,
        hook_settings: Option<&str>,
        resume: Option<&str>,
    ) -> (String, Vec<String>) {
        let mut parts = self.agent_command.split_whitespace().map(str::to_string);
        let program = parts.next().unwrap_or_else(|| "claude".to_string());
        let mut args: Vec<String> = parts.collect();
        if plan {
            args.extend(self.plan_args.iter().cloned());
        }
        if let Some(settings) = hook_settings {
            args.push("--settings".to_string());
            args.push(settings.to_string());
        }
        if let Some(id) = resume {
            args.push("--resume".to_string());
            args.push(id.to_string());
        }
        (program, args)
    }
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

fn default_agent_command() -> String {
    "claude".to_string()
}
fn default_plan_args() -> Vec<String> {
    vec!["--permission-mode".to_string(), "plan".to_string()]
}
fn default_design_preview() -> String {
    "side_pane".to_string()
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
        assert_eq!(cfg.agent_command, "claude");
        assert_eq!(cfg.plan_args, vec!["--permission-mode", "plan"]);
        assert_eq!(cfg.design_preview, "side_pane");
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
    fn launch_command_adds_plan_args_only_in_plan_mode() {
        let cfg = parse(serde_json::json!({ "agent_command": "claude --verbose" }));
        assert_eq!(
            cfg.launch_command(false, None, None),
            ("claude".to_string(), vec!["--verbose".to_string()])
        );
        assert_eq!(
            cfg.launch_command(true, None, None),
            (
                "claude".to_string(),
                vec![
                    "--verbose".to_string(),
                    "--permission-mode".to_string(),
                    "plan".to_string()
                ]
            )
        );
    }

    #[test]
    fn launch_command_appends_settings_and_resume() {
        let cfg = parse(serde_json::json!({}));
        // The hook settings path rides after any plan args; `--resume` coexists
        // with `--settings` (H-03: resume must re-pass the hook settings).
        assert_eq!(
            cfg.launch_command(false, Some("/data/hooks/orchestrator-implement.json"), None),
            (
                "claude".to_string(),
                vec![
                    "--settings".to_string(),
                    "/data/hooks/orchestrator-implement.json".to_string(),
                ]
            )
        );
        assert_eq!(
            cfg.launch_command(
                true,
                Some("/data/hooks/orchestrator-plan.json"),
                Some("claude-sess-abc"),
            ),
            (
                "claude".to_string(),
                vec![
                    "--permission-mode".to_string(),
                    "plan".to_string(),
                    "--settings".to_string(),
                    "/data/hooks/orchestrator-plan.json".to_string(),
                    "--resume".to_string(),
                    "claude-sess-abc".to_string(),
                ]
            )
        );
        // Resume without a hook spec still passes `--resume` alone.
        assert_eq!(
            cfg.launch_command(false, None, Some("sess-1")),
            (
                "claude".to_string(),
                vec!["--resume".to_string(), "sess-1".to_string()]
            )
        );
    }
}
