//! AI-tool abstraction (#196): which agent CLI runs inside a pane.
//!
//! Two orthogonal axes (ADR-0014): the **agent plugin** (pane runner —
//! herdr/orca, `[[workflows]].agent`) and the **AI tool** (the CLI launched in
//! the pane — Claude Code / Codex / OpenCode, `[tools]` + `tool` fields). The
//! orchestrator resolves the tool here — command line, mode flags, resume
//! syntax — and hands the plugin a fully-resolved
//! [`ToolLaunchSpec`](plugin_protocol::methods::ToolLaunchSpec) (argv + env)
//! over the protocol, following the opaque-contract style of
//! `HookLaunchSpec`/H-01: tool knowledge lives on the orchestrator side, the
//! plugin just launches what it is given.
//!
//! Selection precedence (validated in `config::validate`, applied in
//! `run::dispatch_one`): `[[workflows]].tool` (explicit pin) >
//! `[[repositories]].tool` (repo default) > `default_tool` (global) >
//! built-in `"claude"`.
//!
//! Phase 1 (#196) ships the Claude adapter only: [`ToolKind::Codex`] /
//! [`ToolKind::Opencode`] parse in config but have no completion-detection
//! adapter yet, so validation rejects configs that reference them and
//! `launch_spec` returns `None` ([`ToolKind::has_adapter`]).

use std::collections::BTreeMap;

use plugin_protocol::methods::ToolLaunchSpec;
use serde::Deserialize;

use crate::config::ToolConfig;

/// The adapter family a `[tools.<name>]` entry belongs to. Determines argv
/// assembly, capabilities, and (Phase 2/3) the completion-detection assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Claude Code (`claude`) — the reference adapter: `--settings` hooks,
    /// `--permission-mode plan`, `--resume <id>`.
    Claude,
    /// OpenAI Codex CLI (Phase 2, hooks adapter).
    Codex,
    /// OpenCode CLI (Phase 3, JS-plugin adapter).
    Opencode,
}

impl ToolKind {
    /// The stable snake_case config string for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Claude => "claude",
            ToolKind::Codex => "codex",
            ToolKind::Opencode => "opencode",
        }
    }

    /// Whether a completion-detection adapter ships for this kind. A tool
    /// without one could never signal completion — every task would end in a
    /// timeout escalation — so validation refuses such configs upfront
    /// (#196 decision 8: no `kind = "custom"` escape hatch).
    pub fn has_adapter(self) -> bool {
        matches!(self, ToolKind::Claude)
    }
}

/// What a tool can do, driving graceful degradation in dispatch/engine
/// behavior (#196 縮退表). Static per [`ToolKind`]; Phase 2/3 real-machine
/// spikes may flip individual flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCapabilities {
    /// Invisible prompt-context injection (`UserPromptSubmit`
    /// `additionalContext` or equivalent). Without it, instructions ride the
    /// visible `extra_context` instead.
    pub invisible_injection: bool,
    /// A blank Stop can be blocked to re-ask for the status marker (R-03).
    /// Without it, an UNKNOWN stop escalates immediately (no D-02 retry loop).
    pub marker_block: bool,
    /// Prompt-type Stop hooks exist (`verification = "llm"`). Without it, llm
    /// verification falls back to `human` at dispatch.
    pub prompt_verification: bool,
    /// A past session can be resumed by native id.
    pub resume: bool,
    /// A read-oriented plan mode exists.
    pub plan_mode: bool,
    /// Intermediate liveness signals arrive while the agent works.
    pub heartbeat: bool,
    /// The tool's native session id is captured (SessionStart or equivalent),
    /// enabling thread continuity (#140).
    pub session_id_capture: bool,
}

/// A resolved `[tools.<name>]` entry (or a built-in default): everything
/// needed to assemble the launch argv for one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolProfile {
    /// Registry key (`[tools.<name>]`), e.g. `"claude"` / `"claude-fast"`.
    pub name: String,
    /// Adapter family.
    pub kind: ToolKind,
    /// Whitespace-split command line: first token = program, rest = base args.
    pub command: String,
    /// Extra args appended in implement mode (overrides the kind default).
    pub mode_args: Option<Vec<String>>,
    /// Extra args appended in plan mode (overrides the kind default).
    pub plan_args: Option<Vec<String>>,
}

/// The per-dispatch inputs [`ToolProfile::launch_spec`] combines with the
/// profile: everything that varies task-to-task.
#[derive(Debug, Clone)]
pub struct LaunchInputs<'a> {
    /// Plan (design) mode instead of implement.
    pub plan: bool,
    /// The workflow's rendered hook-settings path (Claude only; `--settings`).
    pub settings_path: Option<&'a str>,
    /// A prior session's native id to resume (#140), if any.
    pub resume_session_id: Option<&'a str>,
    /// Environment to inject into the launched process (`TOTSUKA_*`).
    pub env: BTreeMap<String, String>,
}

impl ToolProfile {
    /// The built-in profile for `name`, if one exists. Phase 1: `"claude"`
    /// only; `[tools.<name>]` entries override/extend these.
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self {
                name: "claude".to_string(),
                kind: ToolKind::Claude,
                command: "claude".to_string(),
                mode_args: None,
                plan_args: None,
            }),
            _ => None,
        }
    }

    /// Interpret a parsed `[tools.<name>]` config entry.
    pub fn from_config(name: &str, config: &ToolConfig) -> Self {
        Self {
            name: name.to_string(),
            kind: config.kind,
            command: config
                .command
                .clone()
                .unwrap_or_else(|| config.kind.as_str().to_string()),
            mode_args: config.mode_args.clone(),
            plan_args: config.plan_args.clone(),
        }
    }

    /// The static capability table for this profile's kind (#196 縮退表).
    pub fn capabilities(&self) -> ToolCapabilities {
        match self.kind {
            ToolKind::Claude => ToolCapabilities {
                invisible_injection: true,
                marker_block: true,
                prompt_verification: true,
                resume: true,
                plan_mode: true,
                heartbeat: true,
                session_id_capture: true,
            },
            // Provisional (#196 縮退表); confirmed/adjusted by the Phase 2/3
            // real-machine spikes ([V1]–[V3], [U]) before these kinds gain an
            // adapter.
            ToolKind::Codex => ToolCapabilities {
                invisible_injection: true,
                marker_block: true,
                prompt_verification: false,
                resume: true,
                plan_mode: true,
                heartbeat: false,
                session_id_capture: true,
            },
            ToolKind::Opencode => ToolCapabilities {
                invisible_injection: false,
                marker_block: false,
                prompt_verification: false,
                resume: true,
                plan_mode: true,
                heartbeat: false,
                session_id_capture: true,
            },
        }
    }

    /// Assemble the fully-resolved launch spec for one dispatch, or `None`
    /// when the kind has no adapter yet (callers refuse such dispatches
    /// upfront via [`ToolKind::has_adapter`]).
    ///
    /// Claude argv (mirrors the herdr `launch_command` this replaces —
    /// #196 Phase 1 is behavior-invariant): base command, plan args in plan
    /// mode (default `--permission-mode plan`), `--settings <path>` whenever a
    /// hook settings path is supplied (H-03: `--resume` never inherits hooks,
    /// so the settings ride every launch), and `--resume <id>` when resuming.
    pub fn launch_spec(&self, inp: &LaunchInputs<'_>) -> Option<ToolLaunchSpec> {
        match self.kind {
            ToolKind::Claude => {
                let mut parts = self.command.split_whitespace().map(str::to_string);
                let program = parts.next().unwrap_or_else(|| "claude".to_string());
                let mut args: Vec<String> = parts.collect();
                if inp.plan {
                    match &self.plan_args {
                        Some(extra) => args.extend(extra.iter().cloned()),
                        None => args.extend(["--permission-mode".to_string(), "plan".to_string()]),
                    }
                } else if let Some(extra) = &self.mode_args {
                    args.extend(extra.iter().cloned());
                }
                if let Some(settings) = inp.settings_path {
                    args.push("--settings".to_string());
                    args.push(settings.to_string());
                }
                if let Some(id) = inp.resume_session_id {
                    args.push("--resume".to_string());
                    args.push(id.to_string());
                }
                Some(ToolLaunchSpec {
                    program,
                    args,
                    env: inp.env.clone(),
                })
            }
            // No adapter yet (Phase 2/3).
            ToolKind::Codex | ToolKind::Opencode => None,
        }
    }
}

/// The resolved tool registry: built-ins overlaid with `[tools]` entries
/// (an entry named like a built-in overrides it).
pub fn registry_from_config(
    tools: &BTreeMap<String, ToolConfig>,
) -> std::collections::HashMap<String, ToolProfile> {
    let mut registry = std::collections::HashMap::new();
    // Built-ins first (Phase 1: claude only); [tools] entries overlay them.
    let claude = ToolProfile::builtin("claude").expect("built-in profile exists");
    registry.insert(claude.name.clone(), claude);
    for (name, config) in tools {
        registry.insert(name.clone(), ToolProfile::from_config(name, config));
    }
    registry
}

/// The built-in-only registry (no `[tools]` entries): what a config without
/// a `[tools]` section resolves against. Also the baseline for tests.
pub fn builtin_registry() -> std::collections::HashMap<String, ToolProfile> {
    registry_from_config(&BTreeMap::new())
}

/// The tool name a task resolves to (#196 precedence): the workflow's
/// explicit pin, else the repository default, else the global default.
pub fn resolve_tool_name(
    workflow_tool: Option<&str>,
    repo_tool: Option<&str>,
    default_tool: &str,
) -> String {
    workflow_tool
        .or(repo_tool)
        .unwrap_or(default_tool)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> ToolProfile {
        ToolProfile::builtin("claude").unwrap()
    }

    fn inputs<'a>(
        plan: bool,
        settings_path: Option<&'a str>,
        resume_session_id: Option<&'a str>,
    ) -> LaunchInputs<'a> {
        LaunchInputs {
            plan,
            settings_path,
            resume_session_id,
            env: BTreeMap::new(),
        }
    }

    fn argv(profile: &ToolProfile, inp: &LaunchInputs<'_>) -> (String, Vec<String>) {
        let spec = profile.launch_spec(inp).unwrap();
        (spec.program, spec.args)
    }

    // Golden tests ported from the herdr `launch_command` this module
    // replaces (plugins/agent-ide-herdr/src/config.rs) — the argv contract
    // must not drift (#196 Phase 1 behavior invariance).

    #[test]
    fn claude_adds_plan_args_only_in_plan_mode() {
        let profile = ToolProfile {
            command: "claude --verbose".to_string(),
            ..claude()
        };
        assert_eq!(
            argv(&profile, &inputs(false, None, None)),
            ("claude".to_string(), vec!["--verbose".to_string()])
        );
        assert_eq!(
            argv(&profile, &inputs(true, None, None)),
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
    fn claude_appends_settings_and_resume() {
        let profile = claude();
        // The hook settings path rides after any plan args; `--resume` coexists
        // with `--settings` (H-03: resume must re-pass the hook settings).
        assert_eq!(
            argv(
                &profile,
                &inputs(false, Some("/data/hooks/orchestrator-implement.json"), None)
            ),
            (
                "claude".to_string(),
                vec![
                    "--settings".to_string(),
                    "/data/hooks/orchestrator-implement.json".to_string(),
                ]
            )
        );
        assert_eq!(
            argv(
                &profile,
                &inputs(
                    true,
                    Some("/data/hooks/orchestrator-plan.json"),
                    Some("claude-sess-abc")
                )
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
            argv(&profile, &inputs(false, None, Some("sess-1"))),
            (
                "claude".to_string(),
                vec!["--resume".to_string(), "sess-1".to_string()]
            )
        );
    }

    #[test]
    fn custom_plan_and_mode_args_override_defaults() {
        let profile = ToolProfile {
            plan_args: Some(vec!["--plan".to_string()]),
            mode_args: Some(vec!["--yolo".to_string()]),
            ..claude()
        };
        assert_eq!(
            argv(&profile, &inputs(true, None, None)).1,
            vec!["--plan".to_string()]
        );
        assert_eq!(
            argv(&profile, &inputs(false, None, None)).1,
            vec!["--yolo".to_string()]
        );
    }

    #[test]
    fn launch_spec_carries_env_verbatim() {
        let mut env = BTreeMap::new();
        env.insert("TOTSUKA_JOB_ID".to_string(), "job-1-2".to_string());
        let spec = claude()
            .launch_spec(&LaunchInputs {
                plan: false,
                settings_path: None,
                resume_session_id: None,
                env: env.clone(),
            })
            .unwrap();
        assert_eq!(spec.env, env);
    }

    #[test]
    fn kinds_without_an_adapter_produce_no_spec() {
        for kind in [ToolKind::Codex, ToolKind::Opencode] {
            let profile = ToolProfile { kind, ..claude() };
            assert!(!kind.has_adapter());
            assert!(profile.launch_spec(&inputs(false, None, None)).is_none());
        }
    }

    #[test]
    fn registry_builtin_overridden_by_config_entry() {
        use crate::config::RootConfig;
        let cfg = RootConfig::from_toml_str(
            r#"
[tools.claude]
kind = "claude"
command = "claude --model opus"

[tools.claude-fast]
kind = "claude"
command = "claude --model haiku"
"#,
        )
        .unwrap();
        let registry = registry_from_config(&cfg.tools);
        assert_eq!(registry["claude"].command, "claude --model opus");
        assert_eq!(registry["claude-fast"].command, "claude --model haiku");
        // The built-in remains when not overridden.
        let registry = registry_from_config(&BTreeMap::new());
        assert_eq!(registry["claude"].command, "claude");
    }

    #[test]
    fn resolution_precedence_is_workflow_repo_default() {
        assert_eq!(resolve_tool_name(Some("wf"), Some("repo"), "default"), "wf");
        assert_eq!(resolve_tool_name(None, Some("repo"), "default"), "repo");
        assert_eq!(resolve_tool_name(None, None, "default"), "default");
    }
}
