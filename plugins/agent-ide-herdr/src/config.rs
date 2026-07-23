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
    /// How a design preview is surfaced (F-34), e.g. `side_pane`. Advisory; the
    /// plugin declares the `design_preview` capability regardless.
    #[serde(default = "default_design_preview")]
    pub design_preview: String,
    /// Per-request timeout (seconds) for herdr socket calls.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
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
