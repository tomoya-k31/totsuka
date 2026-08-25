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
use crate::template;
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
    /// `ai-docs/development/config-reference.md` (#276).
    #[error(
        "config schema version {found} is older than this totsuka requires (v{expected}) → update config.toml to schema v{expected} (see docs/config-reference.md) and set `version = {expected}`"
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
        "{referrer} uses unknown placeholder `{{{placeholder}}}` → allowed: {{repo}}, {{repo_name}}, {{worktree_name}}, {{task_id}}, {{source}}"
    )]
    UnknownWorktreePlaceholder {
        referrer: String,
        placeholder: String,
    },

    /// A worktree location template still uses the retired `{branch}`
    /// placeholder (F-22 addendum).
    ///
    /// Its own variant rather than an `UnknownWorktreePlaceholder`: the name
    /// was valid until the branch stopped being something the orchestrator
    /// knows at creation time, so "unknown placeholder" would read as a typo
    /// and send the operator looking for one.
    #[error(
        "{referrer} uses `{{branch}}`, which is no longer available: the branch is chosen by the agent \
         after the worktree exists, so it cannot name the directory the worktree is created at → \
         use `{{worktree_name}}` (rendered as `{{source}}-{{task_id}}`) instead"
    )]
    RetiredWorktreeBranchPlaceholder { referrer: String },

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

    /// A prompt override uses a `{placeholder}` the key does not define
    /// (#315). An **error**, not a passthrough: the typo silently deletes the
    /// text it was meant to insert, and when that text is the marker
    /// convention the only symptom is tasks escalating on a timeout.
    #[error(
        "{referrer} prompt `{key}` uses unknown placeholder `{{{placeholder}}}` → allowed for this key: {allowed} (unknown placeholders are emitted verbatim at render time, so this would ship as literal text)"
    )]
    UnknownPromptPlaceholder {
        referrer: String,
        key: String,
        placeholder: String,
        allowed: String,
    },

    /// A key written under a `prompts` table that #465 removed.
    ///
    /// The table is still parsed (opaquely) purely so this can be raised.
    /// Deleting the field instead would leave serde reporting `prompts` as an
    /// unknown field, which is true but answers the wrong question: the
    /// operator is going to ask what happened to text they wrote deliberately,
    /// and a schema error does not tell them.
    #[error(
        "{referrer} sets `{key}`, which was removed in favour of built-in prompt text → {replacement}"
    )]
    RemovedPromptKey {
        referrer: String,
        key: String,
        replacement: &'static str,
    },

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

    /// A workflow writes out a key its `profile` already supplies (#394).
    ///
    /// Rejected rather than silently overridden either way: whichever side lost
    /// would be text that reads as live config and is not. The same reasoning
    /// removed `output = "pull_request"` instead of accepting-and-ignoring it.
    /// `output` is exempt — it selects a wiring destination, not a permission,
    /// so an override there is a documented feature.
    #[error(
        "workflow `{workflow}` sets both profile = \"{profile}\" and {key} → the profile already decides {key} (and mode/verification are what make it a permission boundary, so an override is not accepted); remove `{key}` to use the profile, or drop `profile` and spell out mode/output/verification"
    )]
    ProfileConflict {
        workflow: String,
        profile: String,
        key: &'static str,
    },

    /// A workflow has neither a `profile` nor the keys one would have supplied
    /// (#394). `mode`/`output` were required before profiles existed; this
    /// keeps them required whenever no profile stands in for them.
    #[error(
        "workflow `{workflow}` has no profile and no `{key}` → set `profile` to one of answer/triage/design/implement, or write `{key}` out"
    )]
    WorkflowMissingKey { workflow: String, key: &'static str },

    /// A top-level table matches no plugin in the `[plugins.*]` roster (#554).
    ///
    /// This is what replaces `deny_unknown_fields` on
    /// [`RootConfig`], and it catches strictly more:
    /// a mistyped core key (`[worktre]`) and a mistyped plugin name (`[slak]`)
    /// both land here, where serde only ever saw the first.
    #[error(
        "unknown top-level table `{name}` in config.toml → no plugin named `{name}` is declared in [plugins.*]; add `[plugins.{name}]` if this is a plugin's settings, or fix the spelling of a core key"
    )]
    UnknownTopLevelTable { name: String },

    /// A leftover top-level key holds something other than a table (#554).
    ///
    /// Split from [`UnknownTopLevelTable`](Self::UnknownTopLevelTable) because
    /// the fix is different: a scalar is never a plugin's settings, so naming
    /// the roster would send the operator down the wrong path.
    #[error(
        "top-level key `{name}` in config.toml is a {found}, not a table → only plugin settings tables may sit at the top level next to the Orchestrator's own keys"
    )]
    TopLevelKeyNotATable { name: String, found: &'static str },

    /// Two `[[projects]]` entries share a `name` (#554), so a repository
    /// pointing at it would file into whichever the code happened to reach
    /// first.
    #[error(
        "two [[projects]] entries are both named `{name}` → a repository's `project` names one of them; rename one"
    )]
    DuplicateProject { name: String },

    /// A `[[repositories]].project` names no `[[projects]]` entry (#554).
    #[error(
        "repository `{repo}` has project = `{project}`, which no [[projects]] entry declares → add that entry, or fix the name"
    )]
    UnknownProjectRef { repo: String, project: String },

    /// A `[[projects]].source` names no enabled task_source (#554).
    ///
    /// The field is `plugin`, not `source`: thiserror reads a field literally
    /// named `source` as the error's *cause*, and would try to make a `String`
    /// implement `Error`.
    #[error(
        "project `{name}` has source = `{plugin}`, which is not an enabled task_source → enable `[plugins.{plugin}]` with kind = \"task_source\", or fix the name"
    )]
    ProjectSourceNotASource { name: String, plugin: String },

    /// A roster entry uses a name that is already a `config.toml` top-level
    /// key (#554).
    ///
    /// Its `[<name>]` table would be parsed as the Orchestrator's own key of
    /// that name, so the plugin would start with an empty config and nothing
    /// would say so. Plugin names are binary names and cannot be renamed
    /// (ADR-0027), so the roster entry itself has to go.
    #[error(
        "plugin `{name}` cannot be used: `{name}` is already a top-level key of config.toml, so its `[{name}]` settings table would be read as that key instead → the plugin needs a different binary name"
    )]
    PluginNameIsReserved { name: String },
}

/// Placeholders permitted in worktree location templates (F-22 addendum).
const ALLOWED_WORKTREE_PLACEHOLDERS: &[&str] =
    &["repo", "repo_name", "worktree_name", "task_id", "source"];

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

    // Top-level tables that are not the Orchestrator's own keys are plugin
    // settings (#554). serde can no longer reject an unknown key — the
    // flattened catch-all swallows every one of them — so the roster is what
    // decides which are legitimate.
    for (name, value) in &cfg.plugin_settings {
        match value {
            toml::Value::Table(_) if cfg.plugins.contains_key(name) => {}
            toml::Value::Table(_) => {
                errors.push(ValidationError::UnknownTopLevelTable { name: name.clone() })
            }
            other => errors.push(ValidationError::TopLevelKeyNotATable {
                name: name.clone(),
                found: other.type_str(),
            }),
        }
    }

    // A roster name that collides with one of the Orchestrator's own top-level
    // keys silently loses its settings table, so it is refused outright (#554).
    for name in cfg.plugins.keys() {
        if crate::config::is_reserved_top_level_key(name) {
            errors.push(ValidationError::PluginNameIsReserved { name: name.clone() });
        }
    }

    // Global worktree template (F-22).
    if let Some(location) = &cfg.worktree.location {
        check_worktree_placeholders("[worktree].location", location, &mut errors);
    }

    // `[prompts]` and `[[workflows]].prompts` were removed in #465. Both are
    // still *parsed* (as opaque tables) so this can name every key the operator
    // wrote and say what became of it — serde's bare unknown-field error would
    // report a table that is "not allowed" without ever mentioning that it used
    // to be supported.
    check_removed_prompt_table("[prompts]", &cfg.prompts, &mut errors);
    for wf in &cfg.workflows {
        check_removed_prompt_table(
            &format!("workflow `{}` prompts", wf.name),
            &wf.prompts,
            &mut errors,
        );
    }

    // The one prompt knob left (#465). Placeholder typos are errors here rather
    // than render-time passthroughs — see `UnknownPromptPlaceholder`.
    for wf in &cfg.workflows {
        if let Some(rubric) = wf.rubric.as_deref() {
            check_rubric_placeholders(&wf.name, rubric, &mut errors);
        }
    }

    // `[[projects]]` and the references into it (#554). The whole chain
    // — repository → project → plugin — resolves without launching anything,
    // which is the point of writing `source` out rather than inferring it.
    let mut seen_projects = HashSet::new();
    for project in &cfg.projects {
        if !seen_projects.insert(project.name.as_str()) {
            errors.push(ValidationError::DuplicateProject {
                name: project.name.clone(),
            });
        }
        let is_source = cfg
            .plugin(&project.source)
            .is_some_and(|p| p.enabled && p.kind == crate::config::PluginKind::TaskSource);
        if !is_source {
            errors.push(ValidationError::ProjectSourceNotASource {
                name: project.name.clone(),
                plugin: project.source.clone(),
            });
        }
    }
    for repo in &cfg.repositories {
        if let Some(project) = &repo.project
            && !cfg.projects.iter().any(|p| &p.name == project)
        {
            errors.push(ValidationError::UnknownProjectRef {
                repo: repo.name.clone(),
                project: project.clone(),
            });
        }
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
        check_profile_keys(wf, &mut errors);
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

/// Whether a workflow spells out exactly one of "profile" or "the keys a
/// profile supplies" (#394).
///
/// `output` appears in neither list: it is required when no profile supplies
/// it, and permitted as an override when one does — the single exception, since
/// it chooses where a result is wired rather than what the agent may do.
fn check_profile_keys(wf: &crate::config::WorkflowConfig, errors: &mut Vec<ValidationError>) {
    match wf.profile {
        Some(profile) => {
            for (key, present) in [
                ("mode", wf.mode.is_some()),
                ("verification", wf.verification.is_some()),
            ] {
                if present {
                    errors.push(ValidationError::ProfileConflict {
                        workflow: wf.name.clone(),
                        profile: profile.as_str().to_string(),
                        key,
                    });
                }
            }
        }
        None => {
            for (key, present) in [("mode", wf.mode.is_some()), ("output", wf.output.is_some())] {
                if !present {
                    errors.push(ValidationError::WorkflowMissingKey {
                        workflow: wf.name.clone(),
                        key,
                    });
                }
            }
        }
    }
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
/// `agent_hook_capable` returns whether an agent plugin declares
/// `Capabilities::hook_completion` (#131, renamed from a `resume_session ||
/// diagnostics_snapshot` heuristic in 0.5.0 / #496), or `None` when unknown —
/// then the
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
        if wf.resolved_verification() == VerificationMode::Human && !has_notifier {
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
        if wf.resolved_verification() == VerificationMode::Llm {
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
        if wf.rubric.is_some() && wf.resolved_verification() != VerificationMode::Llm {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message: format!(
                    "workflow `{}` sets rubric but verification = {} → rubric only applies to llm verification; set verification = \"llm\" or remove rubric",
                    wf.name,
                    wf.resolved_verification().as_str()
                ),
            });
        }

        // An empty rubric reads as "leave it out", but it lands in the
        // verification prompt as nothing at all — the judge is then left with
        // only the exemptions and no criterion to check.
        if wf.rubric.as_deref().is_some_and(|r| r.trim().is_empty()) {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message: format!(
                    "workflow `{}` sets rubric to an empty string → this replaces the built-in criteria with nothing rather than falling back to them; remove the key to use the default",
                    wf.name
                ),
            });
        }

        for message in swallowed_brace_warnings(&wf.name, wf.rubric.as_deref()) {
            findings.push(Finding {
                severity: FindingSeverity::Warning,
                message,
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

/// Advisory message for a `rubric` whose braces are malformed in a way that
/// silently swallows a real placeholder (#328).
///
/// Separate from [`check_rubric_placeholders`] because it is a Warning: the
/// pattern also occurs in legitimate nested JSON shown to a model.
fn swallowed_brace_warnings(workflow: &str, rubric: Option<&str>) -> Vec<String> {
    rubric
        .filter(|value| template::has_swallowed_brace(value))
        .map(|_| {
            format!(
                "workflow `{workflow}` rubric has a `{{` inside another `{{…}}` span → the whole span is treated as one unknown name and emitted verbatim, so any real placeholder inside it never expands; if the braces are literal content this is harmless, otherwise balance them"
            )
        })
        .into_iter()
        .collect()
}

/// Report any `{placeholder}` a workflow's `rubric` uses (#315, #465).
///
/// The allowed set is empty, and comes from the built-in table rather than a
/// literal here so the two cannot drift: `rubric` fills the
/// `verification_rubric` leaf, and leaves render in a pass of their own before
/// the assembly substitutes `{rubric}`. A name in a leaf therefore has nothing
/// to resolve against and ships as literal text — which is why this is an
/// error rather than a passthrough.
///
/// `${ENV}` is **not** skipped: prompts get no env expansion, so a `${…}` in
/// one is a literal, and `{…}` inside it is still a placeholder.
fn check_rubric_placeholders(workflow: &str, rubric: &str, errors: &mut Vec<ValidationError>) {
    let allowed = crate::prompts::allowed_placeholders("verification_rubric")
        .expect("verification_rubric is a built-in prompt key");
    for name in template::scan(rubric, template::ScanMode::Rendered) {
        if !allowed.contains(&name) {
            errors.push(ValidationError::UnknownPromptPlaceholder {
                referrer: format!("workflow `{workflow}`"),
                key: "rubric".to_string(),
                placeholder: name.to_string(),
                allowed: if allowed.is_empty() {
                    "(none)".to_string()
                } else {
                    allowed
                        .iter()
                        .map(|a| format!("{{{a}}}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            });
        }
    }
}

/// Report every key still written under a `prompts` table removed by #465.
///
/// One finding per key, each naming what replaced it, because the operator
/// wrote that text on purpose and "unknown field" answers a different question
/// than the one they will be asking.
fn check_removed_prompt_table(
    referrer: &str,
    table: &toml::Table,
    errors: &mut Vec<ValidationError>,
) {
    for key in table.keys() {
        errors.push(ValidationError::RemovedPromptKey {
            referrer: referrer.to_string(),
            key: key.clone(),
            replacement: removed_prompt_replacement(key),
        });
    }
}

/// What to do instead, per removed key (#465).
fn removed_prompt_replacement(key: &str) -> &'static str {
    match key {
        "verification_rubric" => {
            "write the criteria as `rubric` on the workflow itself — the one prompt key that survived"
        }
        "marker_self_report" => {
            "nothing replaces it: the completion protocol is chosen by the workflow's `profile` (design / implement get the human-confirmation variant), which is exactly what an override here used to defeat"
        }
        "branch_convention" => {
            "nothing replaces it: the agent reads the branch convention out of the target repository (ADR-0026)"
        }
        "verification_prompt"
        | "verification_marker_convention"
        | "verification_background_exemption"
        | "verification_nonclaim_exemption" => {
            "nothing replaces it: how the judging prompt is assembled is built in, and `rubric` is the part of it that was ever meant to be yours"
        }
        "opencode_plan_agent" => {
            "nothing replaces it: the opencode plan agent's prose is built in (its permission deny map never was configurable)"
        }
        _ => "no key by this name was ever supported here",
    }
}

/// Report any `{placeholder}` in `template` outside the allowed set.
///
/// `${ENV}` env references are skipped: a `{` immediately preceded by `$` is
/// part of `${...}` expansion (handled at resolve time), not a worktree
/// placeholder.
fn check_worktree_placeholders(referrer: &str, template: &str, errors: &mut Vec<ValidationError>) {
    // Deduplicated: `scan` reports every occurrence, so a template naming the
    // same placeholder twice would otherwise produce two identical findings —
    // pure noise, since one remedy fixes both.
    let mut seen = HashSet::new();
    for name in template::scan(template, template::ScanMode::Replaced) {
        if !seen.insert(name) {
            continue;
        }
        if name == "branch" {
            errors.push(ValidationError::RetiredWorktreeBranchPlaceholder {
                referrer: referrer.to_string(),
            });
        } else if !ALLOWED_WORKTREE_PLACEHOLDERS.contains(&name) {
            errors.push(ValidationError::UnknownWorktreePlaceholder {
                referrer: referrer.to_string(),
                placeholder: name.to_string(),
            });
        }
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

    /// The check that replaced `deny_unknown_fields` on `RootConfig` (#554).
    /// A table the roster knows is settings; one it does not is a typo, and
    /// the typo can be in either half — a core key or a plugin name.
    #[test]
    fn a_top_level_table_is_legitimate_only_when_the_roster_knows_it() {
        let cfg = RootConfig::from_toml_str(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"

[slack]
app_token = "op://Dev/Slack/app_token"

[slak]
app_token = "typo"

[worktre]
cleanup = "keep_7d"
"#,
        )
        .unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        let named: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert!(
            named.iter().any(|e| e.contains("`slak`")),
            "a mistyped plugin name must be caught: {named:?}"
        );
        assert!(
            named.iter().any(|e| e.contains("`worktre`")),
            "a mistyped core key must still be caught: {named:?}"
        );
        assert!(
            !named.iter().any(|e| e.contains("`slack`")),
            "the roster knows slack, so its table is fine: {named:?}"
        );
    }

    /// A scalar at the top level is never a plugin's settings, so it gets its
    /// own message instead of being pointed at the roster.
    #[test]
    fn a_leftover_top_level_scalar_is_reported_as_not_a_table() {
        let cfg = RootConfig::from_toml_str("max_concurency = 8\n").unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        let named: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert!(
            named
                .iter()
                .any(|e| e.contains("`max_concurency`") && e.contains("not a table")),
            "{named:?}"
        );
    }

    /// A roster entry named after one of the Orchestrator's own top-level keys
    /// would lose its settings table to that key — silently, which is the whole
    /// hazard (#554).
    #[test]
    fn a_roster_name_that_collides_with_a_core_key_is_refused() {
        let cfg = RootConfig::from_toml_str(
            r#"
[plugins.log]
enabled = true
kind = "notifier"
"#,
        )
        .unwrap();
        let named: Vec<String> = validate_static(&cfg, &env_from(&[]))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            named
                .iter()
                .any(|e| e.contains("`log`") && e.contains("already a top-level key")),
            "{named:?}"
        );
    }

    /// The `[[projects]]` reference chain — repository → project → plugin —
    /// resolves offline (#554): a duplicate name, a dangling `project`, and a
    /// `source` that is not an enabled task_source are each their own error,
    /// while an intact chain says nothing. This is the check that replaced
    /// `ClaimConflict`: the broken states are refused before any plugin runs.
    #[test]
    fn the_projects_reference_chain_is_validated_without_launching_anything() {
        let cfg = RootConfig::from_toml_str(
            r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[projects]]
name = "board"
source = "github"
owner = "me"
project_number = 1

[[projects]]
name = "board"
source = "github"

[[projects]]
name = "notion-db"
source = "herdr"

[[repositories]]
name = "web-app"
path = "/tmp"
project = "board"

[[repositories]]
name = "cli"
path = "/tmp"
project = "no-such-board"
"#,
        )
        .unwrap();
        let named: Vec<String> = validate_static(&cfg, &env_from(&[]))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            named.iter().any(|e| e.contains("both named `board`")),
            "a duplicate project name must be caught: {named:?}"
        );
        assert!(
            named
                .iter()
                .any(|e| e.contains("`cli`") && e.contains("`no-such-board`")),
            "a dangling `project` reference must be caught: {named:?}"
        );
        assert!(
            named
                .iter()
                .any(|e| e.contains("`notion-db`") && e.contains("`herdr`")),
            "a source that is not an enabled task_source must be caught: {named:?}"
        );
        // The intact half of the chain raises nothing.
        assert!(
            !named.iter().any(|e| e.contains("`web-app`")),
            "a resolvable reference must not be reported: {named:?}"
        );
    }

    /// `project` is optional (#554): a repository with none is the normal
    /// state, never a finding.
    #[test]
    fn a_repository_without_a_project_is_not_a_finding() {
        let cfg = RootConfig::from_toml_str(
            r#"
[[repositories]]
name = "web-app"
path = "/tmp"
"#,
        )
        .unwrap();
        let named: Vec<String> = validate_static(&cfg, &env_from(&[]))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(!named.iter().any(|e| e.contains("project")), "{named:?}");
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
output = "source"
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
        // `${XDG_STATE_HOME}` env ref + valid `{repo_name}`/`{worktree_name}`
        // are OK; `{bogus}` in the per-repo override is rejected.
        let dir = env!("CARGO_MANIFEST_DIR");
        let toml = format!(
            r#"
[worktree]
location = "${{XDG_STATE_HOME}}/totsuka/worktrees/{{repo_name}}/{{worktree_name}}"

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

    /// `{branch}` was a valid placeholder until the branch stopped being known
    /// at worktree-creation time. Silently rendering it empty would put every
    /// worktree of a repository at the same path; reporting it as an unknown
    /// placeholder would read as a typo. It gets its own message naming the
    /// replacement.
    #[test]
    fn worktree_templates_reject_the_retired_branch_placeholder() {
        let toml = r#"
[worktree]
location = "/state/worktrees/{repo_name}/{branch}"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::RetiredWorktreeBranchPlaceholder { referrer } if referrer == "[worktree].location"
            )),
            "expected the retired-placeholder error, got: {errors:?}"
        );
        // Not also reported as a typo — one finding, one remedy.
        assert!(!errors.iter().any(|e| matches!(
            e,
            ValidationError::UnknownWorktreePlaceholder { placeholder, .. } if placeholder == "branch"
        )));
        let message = errors
            .iter()
            .find(|e| matches!(e, ValidationError::RetiredWorktreeBranchPlaceholder { .. }))
            .map(|e| e.to_string())
            .unwrap();
        assert!(
            message.contains("{worktree_name}"),
            "the message must name the replacement: {message}"
        );
    }

    /// One finding per distinct placeholder, however many times a template
    /// names it — the remedy is the same edit either way.
    #[test]
    fn a_repeated_bad_placeholder_is_reported_once() {
        let toml = r#"
[worktree]
location = "/state/{branch}/{branch}/{bogus}/{bogus}"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert_eq!(
            errors
                .iter()
                .filter(|e| matches!(e, ValidationError::RetiredWorktreeBranchPlaceholder { .. }))
                .count(),
            1,
            "{errors:?}"
        );
        assert_eq!(
            errors
                .iter()
                .filter(|e| matches!(
                    e,
                    ValidationError::UnknownWorktreePlaceholder { placeholder, .. } if placeholder == "bogus"
                ))
                .count(),
            1,
            "{errors:?}"
        );
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

    /// A workflow body with the plugin refs a validation pass needs, so these
    /// tests see only the profile findings they are about.
    fn profile_cfg(workflow_body: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "w"
source = "slack"
agent = "herdr"
{workflow_body}
"#
        ))
        .unwrap_or_else(|e| panic!("fixture does not parse: {e}"))
    }

    #[test]
    fn a_profile_alongside_the_keys_it_supplies_is_an_error() {
        // Silent-override in either direction leaves text that reads as live
        // config and is not — the reason `output = "pull_request"` was deleted
        // rather than ignored.
        for key in ["mode = \"implement\"", "verification = \"human\""] {
            let cfg = profile_cfg(&format!("profile = \"answer\"\n{key}"));
            let errors = validate_static(&cfg, &env_from(&[]));
            let err = errors
                .iter()
                .find(|e| matches!(e, ValidationError::ProfileConflict { .. }))
                .unwrap_or_else(|| panic!("no ProfileConflict for {key}: {errors:?}"));
            let text = err.to_string();
            assert!(text.contains("answer"), "{text}");
            // "cause + next action": both ways out have to be spelled out.
            assert!(text.contains("remove"), "{text}");
        }
    }

    #[test]
    fn output_is_the_one_key_a_profile_may_be_overridden_on() {
        let cfg = profile_cfg("profile = \"implement\"\noutput = \"source\"");
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::ProfileConflict { .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn no_profile_and_no_mode_or_output_is_an_error() {
        // `mode`/`output` stopped being structurally required when they became
        // `Option`; this is what keeps them required in practice.
        let cfg = profile_cfg("output = \"none\"");
        let errors = validate_static(&cfg, &env_from(&[]));
        let err = errors
            .iter()
            .find(|e| matches!(e, ValidationError::WorkflowMissingKey { key: "mode", .. }))
            .unwrap_or_else(|| panic!("no WorkflowMissingKey(mode): {errors:?}"));
        assert!(err.to_string().contains("profile"), "{err}");

        let cfg = profile_cfg("mode = \"plan\"");
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::WorkflowMissingKey { key: "output", .. })),
            "{errors:?}"
        );
    }

    #[test]
    fn a_profile_workflow_needs_neither_mode_nor_output() {
        let cfg = profile_cfg("profile = \"design\"");
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_unknown_profile_name_is_rejected_at_parse_time() {
        // serde's enum rejection is the whole mechanism — no hand-written list
        // of valid names to drift out of step with `Profile`.
        let err = RootConfig::from_toml_str(
            r#"
[[workflows]]
name = "w"
source = "s"
profile = "reviewer"
agent = "a"
"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::config::ConfigError::Parse(_)),
            "{err:?}"
        );
    }

    #[test]
    fn a_profile_reaches_the_llm_verification_advisories() {
        // #301's degradation and the tool-pin advisory both key off
        // `verification`, which a profile now supplies. If they read the raw
        // field they would see `None` and never fire — an `answer` task would
        // run unverified against a non-claude tool while the config claimed
        // otherwise.
        let cfg = RootConfig::from_toml_str(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[tools.codex-cli]
kind = "codex"

[[workflows]]
name = "w"
source = "slack"
profile = "answer"
agent = "herdr"
tool = "codex-cli"
"#,
        )
        .unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| Some(vec![]), |_| None);
        assert!(
            findings
                .iter()
                .any(|f| f.message.contains("verification = llm")),
            "{findings:?}"
        );
    }

    #[test]
    fn unified_validate_surfaces_workflow_errors_and_warnings() {
        // Two enabled plugins so plugin-ref checks pass; an `output = source`
        // workflow whose source declares no `source` output (error, F-83) and
        // a `rubric` on a workflow that does not verify with the llm judge
        // (warning). Both must appear in one pass — an error must not stop the
        // warnings from being reported.
        //
        // The warning used to be trigger overlap (F-81). That check needed the
        // Orchestrator to interpret triggers, which it stopped doing in #554;
        // any other warning exercises the same "errors and warnings coexist"
        // property this test is about.
        let toml = r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "cannot_publish"
source = "github"
trigger = { label = "x" }
mode = "implement"
agent = "herdr"
output = "source"

[[workflows]]
name = "rubric_without_llm"
source = "github"
trigger = { label = "y" }
mode = "implement"
agent = "herdr"
output = "none"
verification = "none"
rubric = "the PR is open"
"#;
        let cfg = RootConfig::from_toml_str(toml).unwrap();
        let findings = validate(&cfg, &env_from(&[]), |_| Some(vec![]), |_| None);

        assert!(has_errors(&findings), "{findings:?}");
        assert!(
            findings
                .iter()
                .any(|f| f.severity == FindingSeverity::Error
                    && f.message.contains("output = source"))
        );
        assert!(
            findings
                .iter()
                .any(|f| f.severity == FindingSeverity::Warning
                    && f.message.contains("rubric_without_llm")),
            "{findings:?}"
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

    /// A workflow plus whatever prompt keys the caller adds.
    fn prompts_cfg(extra: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"{PLUGIN_PAIR}
[[workflows]]
name = "reply"
source = "github"
mode = "implement"
agent = "herdr"
output = "none"
verification = "llm"
{extra}
"#
        ))
        .unwrap()
    }

    #[test]
    fn a_removed_prompt_key_says_what_became_of_it() {
        // #465 kept both tables parseable purely so this error can exist. A
        // bare `unknown field` would be true and useless: the operator wrote
        // that text on purpose and needs to know where it went.
        let cfg = prompts_cfg(
            "\n[prompts]\nmarker_self_report = \"完了時は印を付けてください\"\nopencode_plan_agent = \"設計だけ\"\n",
        );
        let errors = validate_static(&cfg, &env_from(&[]));
        let removed: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::RemovedPromptKey { .. }))
            .collect();
        assert_eq!(removed.len(), 2, "one per key, got {errors:?}");
        let msgs: Vec<String> = removed.iter().map(|e| e.to_string()).collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("marker_self_report") && m.contains("profile")),
            "the self-report message points at the profile that now decides it: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("opencode_plan_agent") && m.contains("built in")),
            "got {msgs:?}"
        );
    }

    #[test]
    fn a_removed_workflow_prompt_key_names_its_workflow() {
        // The workflow table used to be a distinct type whose
        // `deny_unknown_fields` rejected global-only keys. Both tables are now
        // opaque, so the workflow name has to come from the referrer.
        let cfg =
            prompts_cfg("  [workflows.prompts]\n  verification_rubric = \"実調査に基づくこと\"\n");
        let errors = validate_static(&cfg, &env_from(&[]));
        let msg = errors
            .iter()
            .find(|e| matches!(e, ValidationError::RemovedPromptKey { .. }))
            .unwrap_or_else(|| panic!("got {errors:?}"))
            .to_string();
        assert!(msg.contains("`reply`"), "got {msg}");
        // It points at the key that survived, by its surviving spelling.
        assert!(msg.contains("`rubric`"), "got {msg}");
    }

    #[test]
    fn an_unknown_placeholder_in_a_rubric_is_a_validation_error() {
        // The rubric is a leaf: leaves render in a pass of their own, before
        // the assembly substitutes `{rubric}`. A name written here has nothing
        // to resolve against and ships as literal text.
        let cfg = prompts_cfg("rubric = \"{rubric} を見てください\"\n");
        let errors = validate_static(&cfg, &env_from(&[]));
        let found = errors.iter().find(|e| {
            matches!(
                e,
                ValidationError::UnknownPromptPlaceholder { key, placeholder, .. }
                    if key == "rubric" && placeholder == "rubric"
            )
        });
        assert!(found.is_some(), "got {errors:?}");
        // The message names the config spelling, not the leaf it fills.
        let msg = found.unwrap().to_string();
        assert!(
            msg.contains("`reply`") && msg.contains("`rubric`"),
            "got {msg}"
        );
        assert!(msg.contains("(none)"), "no placeholder is allowed: {msg}");
    }

    #[test]
    fn a_json_shape_in_a_rubric_is_content_not_a_placeholder() {
        // #328: `render` emits `{"ok": true}` verbatim, so validation must not
        // reject it. Showing a model the shape it must answer with is an
        // ordinary thing to write, and before the fix it made `run` refuse to
        // start.
        let cfg = prompts_cfg("rubric = \"出力は {\\\"ok\\\": true} の形にしてください\"\n");
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, ValidationError::UnknownPromptPlaceholder { .. })),
            "got {errors:?}"
        );
    }

    #[test]
    fn a_stray_brace_in_a_rubric_warns() {
        // `"{ {rubric}"` renders as one unknown key emitted verbatim (#328).
        let cfg = prompts_cfg("rubric = \"{ {rubric}\"\n");
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("`reply`") && f.message.contains("inside another")),
            "got {findings:?}"
        );
        // The stock config is clean.
        let findings = validate(&prompts_cfg(""), &env_from(&[]), |_| None, |_| None);
        assert!(
            !warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("inside another")),
            "got {findings:?}"
        );
    }

    #[test]
    fn worktree_templates_keep_scanning_every_brace_span() {
        // #328's identifier restriction must NOT reach worktree templates:
        // they are substituted by chained `str::replace`, so `{branch}` inside
        // a larger span really is substituted and must stay validated.
        let cfg = RootConfig::from_toml_str(&format!(
            r#"{PLUGIN_PAIR}
[worktree]
location = "/tmp/{{a{{branch}}"
"#
        ))
        .unwrap();
        let errors = validate_static(&cfg, &env_from(&[]));
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::UnknownWorktreePlaceholder { .. })),
            "got {errors:?}"
        );

        // A hyphen typo in a worktree placeholder is an error too — otherwise
        // it silently creates a literally-named directory.
        let cfg = RootConfig::from_toml_str(&format!(
            r#"{PLUGIN_PAIR}
[worktree]
location = "/tmp/{{repo-name}}"
"#
        ))
        .unwrap();
        assert!(
            validate_static(&cfg, &env_from(&[]))
                .iter()
                .any(|e| matches!(
                    e,
                    ValidationError::UnknownWorktreePlaceholder { placeholder, .. }
                        if placeholder == "repo-name"
                ))
        );
    }

    #[test]
    fn an_empty_rubric_warns() {
        // An empty rubric reads as "leave it out" but lands as nothing at all,
        // leaving the judge with only the exemptions and no criterion.
        let cfg = prompts_cfg("rubric = \"\"\n");
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("`reply`")
                    && f.message.contains("rubric")
                    && f.message.contains("empty string")),
            "got {findings:?}"
        );
    }

    #[test]
    fn the_stock_config_raises_nothing_about_prompts() {
        // With no override surface left, a config that says nothing about
        // prompts must produce nothing about prompts — errors or warnings.
        let cfg = prompts_cfg("");
        assert!(
            !validate_static(&cfg, &env_from(&[]))
                .iter()
                .any(|e| matches!(
                    e,
                    ValidationError::RemovedPromptKey { .. }
                        | ValidationError::UnknownPromptPlaceholder { .. }
                ))
        );
        let findings = validate(&cfg, &env_from(&[]), |_| None, |_| None);
        assert!(
            !warnings_of(&findings)
                .iter()
                .any(|f| f.message.contains("rubric") || f.message.contains("prompt")),
            "got {findings:?}"
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
