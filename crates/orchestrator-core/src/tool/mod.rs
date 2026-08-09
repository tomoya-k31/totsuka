//! AI-tool abstraction (#196): which agent CLI runs inside a pane.
//!
//! Two orthogonal axes (ADR-0014): the **agent plugin** (pane runner —
//! herdr/orca, `[[workflows]].agent`) and the **AI tool** (the CLI launched in
//! the pane — Claude Code / Codex / OpenCode, `[tools]` + `tool` fields). The
//! orchestrator resolves the tool here — command line, mode flags, resume
//! syntax — and hands the plugin a fully-resolved
//! [`ToolLaunchSpec`] (argv + env)
//! over the protocol, following the opaque-contract style of
//! `HookLaunchSpec`/H-01: tool knowledge lives on the orchestrator side, the
//! plugin just launches what it is given.
//!
//! Selection precedence (validated in `config::validate`, applied in
//! `run::dispatch_one`): `[[workflows]].tool` (explicit pin) >
//! `[[repositories]].tool` (repo default) > `default_tool` (global) >
//! built-in `"claude"`.
//!
//! Phase 1 (#196) shipped the Claude adapter; Phase 2 added Codex (hooks
//! adapter — global `hooks.json` registration in [`crate::hooks::codex`]);
//! Phase 3 added OpenCode (JS-plugin adapter — asset installation in
//! [`crate::hooks::opencode`]). Every current kind has an adapter;
//! [`ToolKind::has_adapter`] stays as the gate a future adapterless kind
//! would trip (validation rejects references, `launch_spec` returns `None`).

use std::collections::BTreeMap;

use plugin_protocol::methods::ToolLaunchSpec;
use serde::Deserialize;

use crate::config::{Profile, ToolConfig};

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
        matches!(
            self,
            ToolKind::Claude | ToolKind::Codex | ToolKind::Opencode
        )
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
    /// Prompt-type Stop hooks exist (`verification = "llm"`). Without it, a
    /// workflow's `llm` verification degrades to `human` when the completion
    /// arrives: the task parks in `Verifying` awaiting `totsuka task verify`
    /// rather than publishing unverified output (read by
    /// `run::hooks::Engine::verification_for`, #301).
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
    /// The workflow's resolved [`Profile`], when it has one.
    ///
    /// Read only to answer "are this dispatch's write paths already closed by
    /// the rendered deny rules?" — see the Claude arm of
    /// [`launch_spec`](ToolProfile::launch_spec), which drops
    /// `--permission-mode plan` in that case (#410).
    pub profile: Option<Profile>,
    /// The workflow's rendered hook-settings path (Claude only; `--settings`).
    pub settings_path: Option<&'a str>,
    /// A prior session's native id to resume (#140), if any.
    pub resume_session_id: Option<&'a str>,
    /// Environment to inject into the launched process (`TOTSUKA_*`).
    pub env: BTreeMap<String, String>,
}

impl ToolProfile {
    /// The built-in profile for `name`, if one exists (`"claude"`, `"codex"`,
    /// `"opencode"` — each usable via `tool = "<name>"` without a `[tools]`
    /// entry); `[tools.<name>]` entries override/extend these.
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self {
                name: "claude".to_string(),
                kind: ToolKind::Claude,
                command: "claude".to_string(),
                mode_args: None,
                plan_args: None,
            }),
            "codex" => Some(Self {
                name: "codex".to_string(),
                kind: ToolKind::Codex,
                command: "codex".to_string(),
                mode_args: None,
                plan_args: None,
            }),
            "opencode" => Some(Self {
                name: "opencode".to_string(),
                kind: ToolKind::Opencode,
                command: "opencode".to_string(),
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
            // Confirmed by the Phase 2 real-machine spike (2026-07-24,
            // codex-cli 0.145.0): Stop block via exit 2 / decision JSON,
            // `last_assistant_message` on Stop stdin, UserPromptSubmit
            // `additionalContext` injection, `codex resume <id>`, SessionStart
            // session-id capture. No prompt-type hooks (command only), no
            // background-task heartbeat. `plan_mode` is the `--sandbox
            // read-only` degradation — codex has no plan *permission* mode.
            ToolKind::Codex => ToolCapabilities {
                invisible_injection: true,
                marker_block: true,
                prompt_verification: false,
                resume: true,
                plan_mode: true,
                heartbeat: false,
                session_id_capture: true,
            },
            // Confirmed by the Phase 3 real-machine spike (2026-07-24,
            // opencode 1.14.39): `-s <id>` resume with retained context,
            // `session.created` id capture, last-message fetch via the SDK.
            // No invisible injection (instructions ride the visible
            // extra_context), no stop block (UNKNOWN streak escalation
            // instead), no prompt-type hooks, no heartbeat. `plan_mode` is
            // the `--agent totsuka-plan` full-deny agent (a partial
            // permission deny leaks via subagent delegation — spike finding).
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
    /// Claude argv: base command, plan args in plan mode (default
    /// `--permission-mode plan`), `--settings <path>` whenever a hook settings
    /// path is supplied (H-03: `--resume` never inherits hooks, so the settings
    /// ride every launch), and `--resume <id>` when resuming.
    ///
    /// **The plan args are conditional since #410.** They are skipped when the
    /// dispatch both carries a settings file and names a [`Profile`] whose deny
    /// rules remove every write tool — `answer` today. See the Claude arm for
    /// why both halves are required. An explicit `plan_args` is never skipped.
    ///
    /// Codex argv (#196 Phase 2): base command, then the `resume <id>`
    /// subcommand when resuming (codex resumes via a subcommand, not a flag),
    /// then the mode flags — implement default `--sandbox workspace-write
    /// --ask-for-approval on-request`, plan default `--sandbox read-only`
    /// (codex has no plan permission mode — spike \[V3\]); `codex resume`
    /// accepts the same flags. `settings_path` is ignored: codex hooks are
    /// registered globally ([`crate::hooks::codex`]) and gated per pane via
    /// the `TOTSUKA_*` env this spec carries.
    ///
    /// OpenCode argv (#196 Phase 3): base command, plan default
    /// `--agent totsuka-plan` (the full-deny plan agent installed by
    /// [`crate::hooks::opencode`]), implement default = no extra flags, and
    /// `-s <id>` when resuming. `settings_path` is ignored — completion
    /// detection is the globally installed JS plugin, env-gated like codex.
    pub fn launch_spec(&self, inp: &LaunchInputs<'_>) -> Option<ToolLaunchSpec> {
        let fallback_program = self.kind.as_str().to_string();
        let mut parts = self.command.split_whitespace().map(str::to_string);
        let program = parts.next().unwrap_or(fallback_program);
        let mut args: Vec<String> = parts.collect();
        match self.kind {
            ToolKind::Claude => {
                // `--permission-mode plan` is skipped when the profile's deny
                // rules already remove every write tool (#410). Claude's plan
                // mode did not stop a `Bash` file write in a live session, so
                // against a shell-less agent what it still contributes is
                // `ExitPlanMode`, a human approval gate that an unattended pane
                // resolves unpredictably: it hangs when Claude Code wrote its
                // plan file and auto-passes when it could not, and `Write`
                // (which authors that file) is one of the tools we removed. An
                // explicit `plan_args` override is still honoured — an operator
                // who wrote one meant it.
                //
                // **`settings_path` is part of the condition, not decoration.**
                // The deny rules reach Claude only through `--settings`, and
                // `run::dispatch_one` resolves that path only for hook-capable
                // agents (`resume_session` / `diagnostics_snapshot`). An
                // agent_ide that declares neither — orca, mock, any plugin
                // shaped like them — gets no settings file, so asking the
                // profile alone would drop the plan flag from a dispatch that
                // never received a deny list: strictly looser than before this
                // change. Both halves must be true for the trade to hold.
                //
                // Only claude is treated this way. Codex's plan flag is
                // `--sandbox read-only`, a real sandbox, and opencode's is an
                // all-deny agent (ADR-0023); dropping those would be a loss.
                let plan_mode_is_redundant = self.plan_args.is_none()
                    && inp.settings_path.is_some()
                    && crate::hooks::permissions::plan_mode_only_adds_the_gate(inp.profile);
                if inp.plan && !plan_mode_is_redundant {
                    match &self.plan_args {
                        Some(extra) => args.extend(extra.iter().cloned()),
                        None => args.extend(["--permission-mode".to_string(), "plan".to_string()]),
                    }
                } else if !inp.plan
                    && let Some(extra) = &self.mode_args
                {
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
            }
            ToolKind::Codex => {
                if let Some(id) = inp.resume_session_id {
                    args.push("resume".to_string());
                    args.push(id.to_string());
                }
                if inp.plan {
                    match &self.plan_args {
                        Some(extra) => args.extend(extra.iter().cloned()),
                        None => args.extend(["--sandbox".to_string(), "read-only".to_string()]),
                    }
                } else {
                    match &self.mode_args {
                        Some(extra) => args.extend(extra.iter().cloned()),
                        None => args.extend([
                            "--sandbox".to_string(),
                            "workspace-write".to_string(),
                            "--ask-for-approval".to_string(),
                            "on-request".to_string(),
                        ]),
                    }
                }
            }
            ToolKind::Opencode => {
                if inp.plan {
                    match &self.plan_args {
                        Some(extra) => args.extend(extra.iter().cloned()),
                        None => {
                            args.extend(["--agent".to_string(), "totsuka-plan".to_string()]);
                        }
                    }
                } else if let Some(extra) = &self.mode_args {
                    args.extend(extra.iter().cloned());
                }
                if let Some(id) = inp.resume_session_id {
                    args.push("-s".to_string());
                    args.push(id.to_string());
                }
            }
        }
        Some(ToolLaunchSpec {
            program,
            args,
            env: inp.env.clone(),
        })
    }
}

/// The resolved tool registry: built-ins overlaid with `[tools]` entries
/// (an entry named like a built-in overrides it).
pub fn registry_from_config(
    tools: &BTreeMap<String, ToolConfig>,
) -> std::collections::HashMap<String, ToolProfile> {
    let mut registry = std::collections::HashMap::new();
    // Built-ins first; [tools] entries overlay them.
    for builtin in ["claude", "codex", "opencode"] {
        let profile = ToolProfile::builtin(builtin).expect("built-in profile exists");
        registry.insert(profile.name.clone(), profile);
    }
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
            // No profile: the legacy `mode = "plan"` shape, which gets no deny
            // injection and therefore keeps `--permission-mode plan`.
            profile: None,
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

    /// #410/#409: every read-only profile drops `--permission-mode plan`.
    /// Plan mode did not stop a `Bash` file write in a live session, so what it
    /// still contributes is an approval gate that an unattended pane resolves
    /// unpredictably — hanging for 14 minutes in one measured `design` run.
    #[test]
    fn claude_drops_plan_mode_for_every_read_only_profile() {
        let with = |profile| LaunchInputs {
            plan: true,
            profile,
            // A settings file is passed: this is the hook-capable shape, the
            // only one where the deny rules actually reach Claude.
            settings_path: Some("/hooks/orchestrator-wf.json"),
            resume_session_id: None,
            env: BTreeMap::new(),
        };
        let args = |profile| argv(&claude(), &with(profile)).1;

        let settings = [
            "--settings".to_string(),
            "/hooks/orchestrator-wf.json".to_string(),
        ];

        assert_eq!(
            args(Some(Profile::Answer)),
            settings.to_vec(),
            "answer denies `Bash` and the edit tools, so plan mode adds only the gate"
        );
        // These keep `Bash` — the patterns and the read-only violation check
        // are their protection — but the gate would hang them, so it goes too.
        for profile in [Profile::Triage, Profile::Design] {
            assert_eq!(
                args(Some(profile)),
                settings.to_vec(),
                "{profile:?} drops the gate as well (#409): it hangs an unattended pane"
            );
        }
        // `implement` is not read-only, so it never had the flag to drop.
        assert_eq!(
            args(Some(Profile::Implement)),
            [
                vec!["--permission-mode".to_string(), "plan".to_string()],
                settings.to_vec()
            ]
            .concat(),
            "an implement workflow forced into plan mode keeps the flag"
        );
        // A workflow with no profile gets no deny injection at all; dropping
        // plan mode there would leave it unrestricted.
        assert_eq!(
            args(None),
            [
                vec!["--permission-mode".to_string(), "plan".to_string()],
                settings.to_vec()
            ]
            .concat()
        );
    }

    /// The deny rules only reach Claude through `--settings`, and
    /// `run::dispatch_one` resolves that path only for hook-capable agents. An
    /// `answer` dispatch to an agent_ide that declares neither `resume_session`
    /// nor `diagnostics_snapshot` (orca, mock) therefore carries **no deny
    /// list**, and dropping the plan flag as well would leave it with nothing
    /// at all — looser than before #410 touched this.
    #[test]
    fn the_plan_flag_survives_when_no_settings_file_carries_the_deny_rules() {
        let args = argv(
            &claude(),
            &LaunchInputs {
                plan: true,
                profile: Some(Profile::Answer),
                settings_path: None,
                resume_session_id: None,
                env: BTreeMap::new(),
            },
        )
        .1;
        assert_eq!(
            args,
            vec!["--permission-mode".to_string(), "plan".to_string()],
            "without a settings file there is no deny list, so plan mode is all there is"
        );
    }

    /// The drop is **claude-only**. Codex's plan flag is a real OS sandbox and
    /// opencode's is an all-deny agent (ADR-0023) — those enforce something,
    /// so removing them would be a straight loss.
    #[test]
    fn the_other_tools_keep_their_plan_flags_under_the_same_profile() {
        let inp = LaunchInputs {
            plan: true,
            profile: Some(Profile::Answer),
            settings_path: Some("/hooks/orchestrator-wf.json"),
            resume_session_id: None,
            env: BTreeMap::new(),
        };
        assert_eq!(
            argv(&ToolProfile::builtin("codex").unwrap(), &inp).1,
            vec!["--sandbox".to_string(), "read-only".to_string()]
        );
        assert_eq!(
            argv(&ToolProfile::builtin("opencode").unwrap(), &inp).1,
            vec!["--agent".to_string(), "totsuka-plan".to_string()]
        );
    }

    /// An operator who wrote `plan_args` meant it, so the drop does not apply.
    #[test]
    fn an_explicit_plan_args_override_survives_the_drop() {
        let mut tool = claude();
        tool.plan_args = Some(vec!["--permission-mode".into(), "plan".into()]);
        let args = argv(
            &tool,
            &LaunchInputs {
                plan: true,
                profile: Some(Profile::Answer),
                settings_path: Some("/hooks/orchestrator-wf.json"),
                resume_session_id: None,
                env: BTreeMap::new(),
            },
        )
        .1;
        assert_eq!(
            args,
            vec![
                "--permission-mode".to_string(),
                "plan".to_string(),
                "--settings".to_string(),
                "/hooks/orchestrator-wf.json".to_string()
            ]
        );
    }

    #[test]
    fn launch_spec_carries_env_verbatim() {
        let mut env = BTreeMap::new();
        env.insert("TOTSUKA_JOB_ID".to_string(), "job-1-2".to_string());
        let spec = claude()
            .launch_spec(&LaunchInputs {
                plan: false,
                profile: None,
                settings_path: None,
                resume_session_id: None,
                env: env.clone(),
            })
            .unwrap();
        assert_eq!(spec.env, env);
    }

    #[test]
    fn all_kinds_have_adapters_since_phase_3() {
        // `has_adapter` stays as the gate a future adapterless kind would
        // trip; today every kind is dispatchable.
        for kind in [ToolKind::Claude, ToolKind::Codex, ToolKind::Opencode] {
            assert!(kind.has_adapter());
        }
    }

    fn opencode() -> ToolProfile {
        ToolProfile::builtin("opencode").unwrap()
    }

    // OpenCode argv contract (#196 Phase 3) — flags verified on the real CLI
    // (opencode 1.14.39 spike, 2026-07-24).

    #[test]
    fn opencode_plan_uses_totsuka_plan_agent_and_resume_is_a_flag() {
        assert_eq!(
            argv(&opencode(), &inputs(false, None, None)),
            ("opencode".to_string(), vec![]),
            "implement mode launches the plain TUI"
        );
        assert_eq!(
            argv(&opencode(), &inputs(true, None, None)),
            (
                "opencode".to_string(),
                vec!["--agent".to_string(), "totsuka-plan".to_string()]
            )
        );
        assert_eq!(
            argv(&opencode(), &inputs(true, None, Some("ses_abc"))),
            (
                "opencode".to_string(),
                vec![
                    "--agent".to_string(),
                    "totsuka-plan".to_string(),
                    "-s".to_string(),
                    "ses_abc".to_string()
                ]
            )
        );
    }

    #[test]
    fn opencode_ignores_settings_path_and_honors_custom_args() {
        let spec = opencode()
            .launch_spec(&inputs(
                false,
                Some("/data/hooks/orchestrator-x.json"),
                None,
            ))
            .unwrap();
        assert!(!spec.args.iter().any(|a| a.contains("orchestrator-x")));
        let profile = ToolProfile {
            command: "opencode --mini".to_string(),
            mode_args: Some(vec!["--auto".to_string()]),
            ..opencode()
        };
        assert_eq!(
            argv(&profile, &inputs(false, None, Some("ses_1"))),
            (
                "opencode".to_string(),
                vec![
                    "--mini".to_string(),
                    "--auto".to_string(),
                    "-s".to_string(),
                    "ses_1".to_string()
                ]
            )
        );
    }

    fn codex() -> ToolProfile {
        ToolProfile::builtin("codex").unwrap()
    }

    // Codex argv contract (#196 Phase 2) — flags verified on the real CLI
    // (codex-cli 0.145.0 spike, 2026-07-24).

    #[test]
    fn codex_implement_and_plan_use_sandbox_defaults() {
        assert_eq!(
            argv(&codex(), &inputs(false, None, None)),
            (
                "codex".to_string(),
                vec![
                    "--sandbox".to_string(),
                    "workspace-write".to_string(),
                    "--ask-for-approval".to_string(),
                    "on-request".to_string(),
                ]
            )
        );
        // No plan permission mode exists ([V3]); plan degrades to the
        // read-only sandbox.
        assert_eq!(
            argv(&codex(), &inputs(true, None, None)),
            (
                "codex".to_string(),
                vec!["--sandbox".to_string(), "read-only".to_string()]
            )
        );
    }

    #[test]
    fn codex_resume_is_a_subcommand_before_mode_flags() {
        assert_eq!(
            argv(&codex(), &inputs(false, None, Some("019f8fc5-abc"))),
            (
                "codex".to_string(),
                vec![
                    "resume".to_string(),
                    "019f8fc5-abc".to_string(),
                    "--sandbox".to_string(),
                    "workspace-write".to_string(),
                    "--ask-for-approval".to_string(),
                    "on-request".to_string(),
                ]
            )
        );
    }

    #[test]
    fn codex_ignores_settings_path_and_honors_custom_args() {
        // No `--settings` equivalent: hooks are registered globally and
        // env-gated, so the settings path must leave no trace in the argv.
        let spec = codex()
            .launch_spec(&inputs(
                false,
                Some("/data/hooks/orchestrator-x.json"),
                None,
            ))
            .unwrap();
        assert!(!spec.args.iter().any(|a| a.contains("orchestrator-x")));
        // Custom base command / mode args / plan args override the defaults.
        let profile = ToolProfile {
            command: "codex --model gpt-5.6-sol".to_string(),
            mode_args: Some(vec!["--full-auto".to_string()]),
            plan_args: Some(vec!["--sandbox".to_string(), "read-only".to_string()]),
            ..codex()
        };
        assert_eq!(
            argv(&profile, &inputs(false, None, Some("sess-1"))),
            (
                "codex".to_string(),
                vec![
                    "--model".to_string(),
                    "gpt-5.6-sol".to_string(),
                    "resume".to_string(),
                    "sess-1".to_string(),
                    "--full-auto".to_string(),
                ]
            )
        );
    }

    #[test]
    fn codex_is_a_builtin_in_the_registry() {
        let registry = builtin_registry();
        assert_eq!(registry["codex"].kind, ToolKind::Codex);
        assert_eq!(registry["codex"].command, "codex");
        assert!(ToolKind::Codex.has_adapter());
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
