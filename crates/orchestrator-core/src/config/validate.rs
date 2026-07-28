//! Static configuration validation (F-63 `--offline`, F-58).
//!
//! These checks need no plugin process: schema version, repository path
//! existence, unique names, and workflow/repository references to plugins
//! (including the rule that a **disabled** plugin must not be referenced,
//! F-58). Checks that require launching plugins (F-59) are wired in #51.

use std::collections::HashSet;

use plugin_protocol::manifest::OutputCapability;

use super::resolve::expand_path;
use super::schema::{CURRENT_SCHEMA_VERSION, PluginKind, RootConfig, VerificationMode};
use crate::domain::workflow::{self, Severity, Workflow};
use crate::tool::{ToolKind, ToolProfile};

/// A single static-validation failure. `Display` gives "cause + next action".
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    /// The config declares a schema this binary predates — the operator is
    /// running an old totsuka against a config written for a newer one (#276).
    #[error(
        "config schema version {found} is newer than this totsuka supports (v{expected}) → upgrade totsuka to a version that supports config schema v{found}"
    )]
    SchemaTooNew { found: u32, expected: u32 },

    /// The config predates this binary. There is no automatic migration and
    /// deliberately no `config migrate` command — see the versioning policy in
    /// `docs/development/config-reference.md` (#276).
    #[error(
        "config schema version {found} is older than this totsuka requires (v{expected}) → update config.toml to schema v{expected} (see docs/development/config-reference.md) and set `version = {expected}`"
    )]
    SchemaOutdated { found: u32, expected: u32 },

    /// A repository path does not exist on disk.
    #[error(
        "repository `{name}` path does not exist: {path} → fix `path` in config.toml or clone the repo"
    )]
    RepoPathMissing { name: String, path: String },

    /// A repository path could not be expanded (e.g. missing env var).
    #[error(
        "repository `{name}` path is unresolvable: {reason} → set the referenced variable, or fix `path` in config.toml"
    )]
    RepoPathUnresolvable { name: String, reason: String },

    /// A worktree location template uses an unknown `{placeholder}` (F-22).
    #[error(
        "{referrer} uses unknown placeholder `{{{placeholder}}}` → allowed: {{repo}}, {{repo_name}}, {{branch}}, {{task_id}}, {{source}}"
    )]
    UnknownWorktreePlaceholder {
        referrer: String,
        placeholder: String,
    },

    /// Two repositories share a name.
    #[error("duplicate repository name `{0}` → repository names must be unique")]
    DuplicateRepo(String),

    /// Two workflows share a name.
    #[error("duplicate workflow name `{0}` → workflow names must be unique")]
    DuplicateWorkflow(String),

    /// A workflow/repo references a plugin that is not defined.
    #[error(
        "{referrer} references unknown plugin `{plugin}` → add `[plugins.{plugin}]` or fix the name"
    )]
    UnknownPluginRef { referrer: String, plugin: String },

    /// A workflow/repo references a disabled plugin (F-58).
    #[error(
        "{referrer} references disabled plugin `{plugin}` → set `[plugins.{plugin}] enabled = true` or point it elsewhere"
    )]
    DisabledPluginRef { referrer: String, plugin: String },

    /// A referenced plugin has the wrong kind for the role.
    #[error(
        "{referrer} plugin `{plugin}` has the wrong kind (expected {expected:?}) → use a {expected:?} plugin"
    )]
    WrongPluginKind {
        referrer: String,
        plugin: String,
        expected: PluginKind,
    },

    /// A `tool` reference names neither a built-in nor a `[tools]` entry
    /// (#196).
    #[error(
        "{referrer} references unknown tool `{tool}` → add `[tools.{tool}]` or use the built-in `claude`"
    )]
    UnknownToolRef { referrer: String, tool: String },

    /// A referenced tool's kind has no completion-detection adapter yet
    /// (#196; unreachable since Phase 3 gave every kind an adapter — kept as
    /// the gate a future adapterless kind would trip).
    #[error(
        "{referrer} tool `{tool}` has kind `{kind}` which has no adapter yet → this kind is not dispatchable in this version"
    )]
    UnsupportedToolKind {
        referrer: String,
        tool: String,
        kind: String,
    },
}

/// Placeholders permitted in worktree location templates (F-22 addendum).
const ALLOWED_WORKTREE_PLACEHOLDERS: &[&str] =
    &["repo", "repo_name", "branch", "task_id", "source"];

/// Run all static checks, returning every problem found (empty = valid).
pub fn validate_static<E>(cfg: &RootConfig, env: &E) -> Vec<ValidationError>
where
    E: Fn(&str) -> Option<String>,
{
    let mut errors = Vec::new();

    // Both directions are a hard stop, but the action they call for is the
    // opposite one — upgrade the binary vs. edit the config — so they are
    // separate variants (#276), mirroring `StateError::SchemaTooNew` /
    // `SchemaOutdated` on the state.db side (#275).
    match cfg.version.cmp(&CURRENT_SCHEMA_VERSION) {
        std::cmp::Ordering::Greater => errors.push(ValidationError::SchemaTooNew {
            found: cfg.version,
            expected: CURRENT_SCHEMA_VERSION,
        }),
        std::cmp::Ordering::Less => errors.push(ValidationError::SchemaOutdated {
            found: cfg.version,
            expected: CURRENT_SCHEMA_VERSION,
        }),
        std::cmp::Ordering::Equal => {}
    }

    // Global worktree template (F-22).
    if let Some(location) = &cfg.worktree.location {
        check_worktree_placeholders("[worktree].location", location, &mut errors);
    }

    // Repositories: unique names + path existence.
    let mut seen_repos = HashSet::new();
    for repo in &cfg.repositories {
        if !seen_repos.insert(repo.name.as_str()) {
            errors.push(ValidationError::DuplicateRepo(repo.name.clone()));
        }
        match expand_path(&repo.path.to_string_lossy(), env) {
            Ok(path) if path.exists() => {}
            Ok(path) => errors.push(ValidationError::RepoPathMissing {
                name: repo.name.clone(),
                path: path.display().to_string(),
            }),
            Err(e) => errors.push(ValidationError::RepoPathUnresolvable {
                name: repo.name.clone(),
                reason: e.to_string(),
            }),
        }
        if let Some(tool) = &repo.tool {
            check_tool_ref(
                cfg,
                &format!("repository `{}` tool", repo.name),
                tool,
                &mut errors,
            );
        }
        if let Some(location) = &repo.worktree_location {
            check_worktree_placeholders(
                &format!("repository `{}` worktree_location", repo.name),
                location,
                &mut errors,
            );
        }
    }

    // Workflows: unique names + source/agent references.
    let mut seen_workflows = HashSet::new();
    for wf in &cfg.workflows {
        if !seen_workflows.insert(wf.name.as_str()) {
            errors.push(ValidationError::DuplicateWorkflow(wf.name.clone()));
        }
        check_plugin_ref(
            cfg,
            &format!("workflow `{}` source", wf.name),
            &wf.source,
            PluginKind::TaskSource,
            &mut errors,
        );
        check_plugin_ref(
            cfg,
            &format!("workflow `{}` agent", wf.name),
            &wf.agent,
            PluginKind::AgentIde,
            &mut errors,
        );
        if let Some(tool) = &wf.tool {
            check_tool_ref(
                cfg,
                &format!("workflow `{}` tool", wf.name),
                tool,
                &mut errors,
            );
        }
    }

    // Global default tool (#196). Checked even when unset: the implicit
    // default is the built-in `claude`, whose kind a `[tools.claude]` entry
    // can override — an adapterless override must fail here, not at dispatch.
    check_tool_ref(
        cfg,
        "default_tool",
        cfg.default_tool.as_deref().unwrap_or("claude"),
        &mut errors,
    );

    errors
}

/// Severity of a validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingSeverity {
    /// Blocks: `config validate` fails.
    Error,
    /// Advisory only.
    Warning,
}

/// A unified validation finding (static check or workflow check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Severity.
    pub severity: FindingSeverity,
    /// Human-readable message ("cause + next action").
    pub message: String,
}

/// Run the full config validation: static checks (F-58/63) **plus** workflow
/// validation (F-81/82/83), returning errors and warnings in one list. This is
/// the entry point the `config validate` command (#64) drives.
///
/// `source_outputs` returns a task-source plugin's declared output
/// capabilities (from its manifest offline, or `None` when unknown).
/// `agent_hook_capable` returns whether an agent plugin signals completion via
/// Claude Code hooks (#131), or `None` when unknown — then the
/// `[hooks].auth_token_ref` advisory is skipped.
pub fn validate<E, F, H>(
    cfg: &RootConfig,
    env: &E,
    source_outputs: F,
    agent_hook_capable: H,
) -> Vec<Finding>
where
    E: Fn(&str) -> Option<String>,
    F: Fn(&str) -> Option<Vec<OutputCapability>>,
    H: Fn(&str) -> Option<bool>,
{
    let mut findings: Vec<Finding> = validate_static(cfg, env)
        .into_iter()
        .map(|e| Finding {
            severity: FindingSeverity::Error,
            message: e.to_string(),
        })
        .collect();

    let workflows = Workflow::from_configs(&cfg.workflows);
    for issue in workflow::validate_workflows(&workflows, source_outputs) {
        findings.push(Finding {
            severity: match issue.severity {
                Severity::Error => FindingSeverity::Error,
                Severity::Warning => FindingSeverity::Warning,
            },
            message: issue.message,
        });
    }
    hook_findings(cfg, &agent_hook_capable, &mut findings);
    findings
}

/// Hook/verification advisory checks (#135) — warnings only, in the
/// "cause + next action" style.
fn hook_findings<H>(cfg: &RootConfig, agent_hook_capable: &H, findings: &mut Vec<Finding>)
where
    H: Fn(&str) -> Option<bool>,
{
    let has_notifier = cfg
        .plugins
        .values()
        .any(|p| p.enabled && p.kind == PluginKind::Notifier);

    for wf in &cfg.workflows {
        // verification = human needs a notifier, or nobody notices the wait.
        if wf.verification == VerificationMode::Human && !has_notifier {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message: format!(
                    "workflow `{}` uses verification = human but no enabled notifier plugin is configured → add an enabled `[plugins.*]` with kind = \"notifier\" so verification requests are noticed",
                    wf.name
                ),
            });
        }

        // Hook-capable agents need the Bearer token to authenticate (E-03).
        if cfg.hooks.auth_token_ref.is_none() && agent_hook_capable(&wf.agent) == Some(true) {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message: format!(
                    "workflow `{}` uses hook-capable agent `{}` but `[hooks].auth_token_ref` is unset → set it (e.g. \"keychain:totsuka/hook-token\") so hook events can be authenticated",
                    wf.name, wf.agent
                ),
            });
        }

        // verification = llm needs Claude's prompt-type Stop hook (#196):
        // a workflow pinned to a non-claude tool degrades to human
        // verification when the completion arrives (`Engine::verification_for`,
        // #301); an unpinned workflow whose repo/global default could resolve
        // to a non-claude tool is fragile — suggest the explicit pin so the
        // constraint is statically guaranteed.
        if wf.verification == VerificationMode::Llm {
            match &wf.tool {
                Some(tool) => {
                    if matches!(
                        resolve_tool_kind(cfg, tool),
                        Some(kind) if kind != ToolKind::Claude
                    ) {
                        findings.push(Finding {
                            severity: FindingSeverity::Warning,
                            message: format!(
                                "workflow `{}` uses verification = llm but pins tool `{}` (non-claude kind) → llm verification needs Claude's prompt-type Stop hook, so completions will fall back to human verification (the task parks in Verifying awaiting `totsuka task verify`); pin a claude-kind tool or set verification = \"human\"",
                                wf.name, tool
                            ),
                        });
                    }
                }
                None => {
                    let non_claude_default = cfg
                        .default_tool
                        .as_deref()
                        .into_iter()
                        .chain(cfg.repositories.iter().filter_map(|r| r.tool.as_deref()))
                        .find(|t| {
                            matches!(
                                resolve_tool_kind(cfg, t),
                                Some(kind) if kind != ToolKind::Claude
                            )
                        });
                    if let Some(tool) = non_claude_default {
                        findings.push(Finding {
                            severity: FindingSeverity::Warning,
                            message: format!(
                                "workflow `{}` uses verification = llm without a tool pin, but `{}` (non-claude kind) is a reachable repo/global default → add `tool = \"claude\"` to the workflow so llm verification is statically guaranteed",
                                wf.name, tool
                            ),
                        });
                    }
                }
            }
        }

        // rubric only feeds the llm-verification prompt hook.
        if wf.rubric.is_some() && wf.verification != VerificationMode::Llm {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message: format!(
                    "workflow `{}` sets rubric but verification = {} → rubric only applies to llm verification; set verification = \"llm\" or remove rubric",
                    wf.name,
                    wf.verification.as_str()
                ),
            });
        }
    }
}

/// Whether any finding is an error (used for the `config validate` exit code).
pub fn has_errors(findings: &[Finding]) -> bool {
    findings
        .iter()
        .any(|f| f.severity == FindingSeverity::Error)
}

/// Validate one tool reference (#196): the name must resolve (built-in or
/// `[tools]` entry) and its kind must have an adapter (Phase 1: `claude`).
fn check_tool_ref(cfg: &RootConfig, referrer: &str, tool: &str, errors: &mut Vec<ValidationError>) {
    match resolve_tool_kind(cfg, tool) {
        None => errors.push(ValidationError::UnknownToolRef {
            referrer: referrer.to_string(),
            tool: tool.to_string(),
        }),
        Some(kind) if !kind.has_adapter() => errors.push(ValidationError::UnsupportedToolKind {
            referrer: referrer.to_string(),
            tool: tool.to_string(),
            kind: kind.as_str().to_string(),
        }),
        Some(_) => {}
    }
}

/// The kind a tool name resolves to: the `[tools]` entry wins over the
/// built-in of the same name; `None` when the name matches neither.
fn resolve_tool_kind(cfg: &RootConfig, tool: &str) -> Option<ToolKind> {
    cfg.tool(tool)
        .map(|t| t.kind)
        .or_else(|| ToolProfile::builtin(tool).map(|p| p.kind))
}

/// Validate one plugin reference: it must exist, be enabled (F-58), and be of
/// the expected kind.
fn check_plugin_ref(
    cfg: &RootConfig,
    referrer: &str,
    plugin: &str,
    expected: PluginKind,
    errors: &mut Vec<ValidationError>,
) {
    match cfg.plugin(plugin) {
        None => errors.push(ValidationError::UnknownPluginRef {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
        }),
        Some(p) if !p.enabled => errors.push(ValidationError::DisabledPluginRef {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
        }),
        Some(p) if p.kind != expected => errors.push(ValidationError::WrongPluginKind {
            referrer: referrer.to_string(),
            plugin: plugin.to_string(),
            expected,
        }),
        Some(_) => {}
    }
}

/// Report any `{placeholder}` in `template` outside the allowed set.
///
/// `${ENV}` env references are skipped: a `{` immediately preceded by `$` is
/// part of `${...}` expansion (handled at resolve time), not a worktree
/// placeholder.
fn check_worktree_placeholders(referrer: &str, template: &str, errors: &mut Vec<ValidationError>) {
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && (i == 0 || bytes[i - 1] != b'$')
            && let Some(rel) = template[i + 1..].find('}')
        {
            let name = &template[i + 1..i + 1 + rel];
            if !ALLOWED_WORKTREE_PLACEHOLDERS.contains(&name) {
                errors.push(ValidationError::UnknownWorktreePlaceholder {
                    referrer: referrer.to_string(),
                    placeholder: name.to_string(),
                });
            }
            i = i + 1 + rel + 1;
            continue;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn valid_config_has_no_errors() {
        // Use the repo root itself as an existing path.
        let dir = env!("CARGO_MANIFEST_DIR");
        let toml = format!(
            r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[repositories]]
name = "totsuka"
path = "{dir}"
tool = "claude"

[[workflows]]
name = "impl"
source = "github"
trigger = {{ project_status = "todo" }}
mode = "implement"
agent = "herdr"
output = "pull_request"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn disabled_plugin_reference_is_rejected() {
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = false
kind = "agent_ide"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::DisabledPluginRef { plugin, .. } if plugin == "herdr"
        )));
    }

    #[test]
    fn unknown_and_wrong_kind_and_missing_path_are_reported() {
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[[repositories]]
name = "missing"
path = "/nonexistent/totsuka/repo"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "github"
output = "none"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        // agent points at a task_source plugin -> wrong kind.
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::WrongPluginKind { plugin, .. } if plugin == "github"
        )));
        // repo path does not exist.
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::RepoPathMissing { name, .. } if name == "missing"
        )));
    }

    #[test]
    fn worktree_templates_reject_unknown_placeholders() {
        // `${XDG_STATE_HOME}` env ref + valid `{repo_name}`/`{branch}` are OK;
        // `{bogus}` in the per-repo override is rejected.
        let dir = env!("CARGO_MANIFEST_DIR");
        let toml = format!(
            r#"
[worktree]
location = "${{XDG_STATE_HOME}}/totsuka/worktrees/{{repo_name}}/{{branch}}"

[[repositories]]
name = "totsuka"
path = "{dir}"
worktree_location = "{{repo}}/../.worktrees/{{bogus}}"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::UnknownWorktreePlaceholder { placeholder, .. } if placeholder == "bogus"
            )),
            "expected unknown-placeholder error, got: {errors:?}"
        );
        // The valid global template must NOT produce an error.
        assert!(!errors.iter().any(|e| matches!(
            e,
            ValidationError::UnknownWorktreePlaceholder { referrer, .. } if referrer == "[worktree].location"
        )));
    }

    /// A config from the future and a config from the past both stop the run,
    /// but the fix is the opposite one each way, so the messages must point in
    /// opposite directions (#276) — and neither may name `config migrate`,
    /// which does not exist.
    #[test]
    fn a_version_mismatch_points_at_the_side_that_has_to_change() {
        let too_new = RootConfig::from_toml_str("version = 999").unwrap();
        let errors = validate_static(&too_new, &env_from(&[]));
        let err = errors
            .iter()
            .find(|e| matches!(e, ValidationError::SchemaTooNew { found: 999, .. }))
            .unwrap_or_else(|| panic!("no SchemaTooNew in {errors:?}"));
        let text = err.to_string();
        assert!(text.contains("upgrade totsuka"), "{text}");
        assert!(!text.contains("config migrate"), "{text}");

        let outdated = RootConfig::from_toml_str("version = 0").unwrap();
        let errors = validate_static(&outdated, &env_from(&[]));
        let err = errors
            .iter()
            .find(|e| matches!(e, ValidationError::SchemaOutdated { found: 0, .. }))
            .unwrap_or_else(|| panic!("no SchemaOutdated in {errors:?}"));
        let text = err.to_string();
        assert!(text.contains("update config.toml"), "{text}");
        assert!(!text.contains("config migrate"), "{text}");
    }

    #[test]
    fn unified_validate_surfaces_workflow_errors_and_warnings() {
        // Two enabled plugins so plugin-ref checks pass; a plan×pull_request
        // workflow (error) and an overlapping pair (warning).
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "bad_plan"
source = "github"
trigger = { label = "x" }
mode = "plan"
agent = "herdr"
output = "pull_request"

[[workflows]]
name = "overlap_a"
source = "github"
trigger = { label = "y" }
mode = "implement"
agent = "herdr"
output = "none"

[[workflows]]
name = "overlap_b"
source = "github"
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);

        assert!(has_errors(&findings), "plan×pull_request must be an error");
        assert!(findings.iter().any(|f| f.severity == FindingSeverity::Error
            && f.message.contains("plan with output = pull_request")));
        assert!(
            findings.iter().any(
                |f| f.severity == FindingSeverity::Warning && f.message.contains("overlapping")
            )
        );
    }

    /// A minimal valid plugin pair used by the hook/verification warning tests.
    const PLUGIN_PAIR: &str = r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"
"#;

    fn warnings_of(findings: &[Finding]) -> Vec<&Finding> {
        findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Warning)
            .collect()
    }

    #[test]
    fn human_verification_without_notifier_warns() {
        let toml = format!(
            r#"{PLUGIN_PAIR}
[[workflows]]
name = "review"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "human"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("no enabled notifier plugin")),
            "expected notifier warning: {findings:?}"
        );

        // An enabled notifier silences the warning.
        let toml = format!(
            r#"{PLUGIN_PAIR}
[plugins.macos]
enabled = true
kind = "notifier"

[[workflows]]
name = "review"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "human"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            !findings.iter().any(|f| f.message.contains("notifier")),
            "unexpected notifier warning: {findings:?}"
        );
    }

    #[test]
    fn missing_auth_token_ref_with_hook_capable_agent_warns() {
        let toml = format!(
            r#"{PLUGIN_PAIR}
[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();

        // Hook-capable agent + no [hooks].auth_token_ref -> warning.
        let findings = validate(&cfg, &env_from(&[]), |_| None, |name| Some(name == "herdr"));
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("[hooks].auth_token_ref")),
            "expected auth_token_ref warning: {findings:?}"
        );

        // Capability unknown (None) -> the advisory is skipped.
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            !findings
                .iter()
                .any(|f| f.message.contains("auth_token_ref")),
            "unknown capability must not warn: {findings:?}"
        );

        // Token configured -> no warning.
        let toml = format!(
            r#"{PLUGIN_PAIR}
[hooks]
auth_token_ref = "keychain:totsuka/hook-token"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |name| Some(name == "herdr"));
        assert!(
            !findings
                .iter()
                .any(|f| f.message.contains("auth_token_ref")),
            "configured token must not warn: {findings:?}"
        );
    }

    #[test]
    fn tool_references_are_validated() {
        let dir = env!("CARGO_MANIFEST_DIR");
        // Unknown names at every level; a [tools] entry with an adapterless
        // kind (#196: opencode parses but cannot dispatch until Phase 3).
        let toml = format!(
            r#"
default_tool = "nope"

[tools.opencode]
kind = "opencode"

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[repositories]]
name = "totsuka"
path = "{dir}"
tool = "opencode"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
tool = "typo"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::UnknownToolRef { referrer, tool } if referrer == "default_tool" && tool == "nope"
        )));
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::UnknownToolRef { tool, .. } if tool == "typo"
        )));
        // Since Phase 3 every kind has an adapter, so the opencode reference
        // above is valid — only the unknown names error.
        assert_eq!(errors.len(), 2, "unexpected extra errors: {errors:?}");

        // The built-in `claude` and a claude-kind [tools] profile are fine.
        let toml = format!(
            r#"
default_tool = "claude"

[tools.claude-fast]
kind = "claude"
command = "claude --model haiku"

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[repositories]]
name = "totsuka"
path = "{dir}"
tool = "claude-fast"

[[workflows]]
name = "impl"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
tool = "claude"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn implicit_default_tool_is_checked_against_overridden_builtin() {
        // /code-review finding on #223: a [tools.claude] entry can override
        // the built-in's kind; with default_tool and every `tool` field
        // omitted, the *implicit* default "claude" must still pass through
        // `check_tool_ref` statically. Since Phase 3 every kind has an
        // adapter, an overridden built-in is simply valid — the check now
        // only guards a future adapterless kind (and unknown names).
        let cfg = RootConfig::from_toml_str("[tools.claude]\nkind = \"opencode\"").unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "unexpected: {errors:?}");
        // An untouched built-in stays silent too.
        let cfg = RootConfig::from_toml_str("").unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn llm_verification_with_non_claude_tool_warns() {
        // A workflow pinned to a non-claude tool with verification = llm
        // degrades at dispatch -> warning (the adapterless-kind hard error
        // also fires in Phase 1, separately).
        let toml = format!(
            r#"{PLUGIN_PAIR}
[tools.codex]
kind = "codex"

[[workflows]]
name = "pinned"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "llm"
tool = "codex"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("`pinned`")
                    && f.message.contains("verification = llm")
                    && f.message.contains("pins tool `codex`")),
            "expected pinned-mismatch warning: {findings:?}"
        );

        // Unpinned llm workflow + a non-claude default in reach -> suggest
        // the explicit pin.
        let toml = format!(
            r#"default_tool = "codex"
{PLUGIN_PAIR}
[tools.codex]
kind = "codex"

[[workflows]]
name = "unpinned"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "llm"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            warnings_of(&findings).iter().any(
                |f| f.message.contains("`unpinned`") && f.message.contains("tool = \"claude\"")
            ),
            "expected pin suggestion: {findings:?}"
        );

        // All-claude resolution -> no tool warning.
        let toml = format!(
            r#"{PLUGIN_PAIR}
[[workflows]]
name = "fine"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "llm"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            !findings.iter().any(|f| f.message.contains("tool")),
            "claude-only must not warn: {findings:?}"
        );
    }

    #[test]
    fn rubric_without_llm_verification_warns() {
        let toml = format!(
            r#"{PLUGIN_PAIR}
[[workflows]]
name = "no_verify"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "none"
rubric = "実調査に基づくこと"

[[workflows]]
name = "llm_verify"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "llm"
rubric = "実調査に基づくこと"
"#
        );
        let cfg = RootConfig::from_toml_str(&toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        // verification = none + rubric -> warning naming the workflow.
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("rubric")
                    && f.message.contains("`no_verify`")
                    && f.message.contains("verification = none")),
            "expected rubric warning: {findings:?}"
        );
        // verification = llm + rubric is the intended combination -> no warning.
        assert!(
            !findings
                .iter()
                .any(|f| f.message.contains("`llm_verify`") && f.message.contains("rubric")),
            "llm + rubric must not warn: {findings:?}"
        );
    }
}
