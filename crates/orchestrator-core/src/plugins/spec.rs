//! Launch-spec assembly for enabled plugins (F-58/64/65, moved from the CLI
//! in #217).
//!
//! [`plugin_spec`] combines the on-disk store (manifest, binary path), the
//! plugin's secret-resolved `[<name>]` table from `config.toml`, and the
//! `config.toml` material a task_source needs at `initialize` (repositories
//! #109, `[llm]` defaults #119, workflow triggers + poll cadence 0.1.6) into
//! one [`PluginSpec`] for [`plugin_host`](crate::adapters::plugin_host).

use std::collections::HashMap;
use std::time::Duration;

use plugin_protocol::manifest::PluginKind;
use plugin_protocol::methods::{LlmInfo, ProjectInfo, RepoInfo, WorkflowInfo};
use serde_json::Value;

use crate::adapters::plugin_host::PluginSpec;
use crate::config::{
    self, ConfigError, ResolveError, RootConfig, resolve_strings, secret_resolver,
};
use crate::plugins::{PluginStore, StoreError};

/// Default per-call plugin RPC timeout when `timeout_secs` is omitted.
pub const DEFAULT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Errors from assembling a plugin's launch spec.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    /// The plugin is declared enabled in config but has no installed binary.
    #[error("plugin `{0}` is enabled but not installed → `totsuka plugin install <dir>`")]
    NotInstalled(String),
    /// The plugin store failed (unreadable manifest, invalid name, ...).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The plugin's `[<name>]` table could not be converted to JSON.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A secret reference inside the plugin's `[<name>]` table did not
    /// resolve.
    #[error("in [{name}] of config.toml: {source}")]
    Resolve {
        /// The plugin whose table holds the offending reference.
        name: String,
        /// What failed to resolve.
        source: ResolveError,
    },
}

/// Build the [`PluginSpec`] for one enabled plugin from the store and its
/// secret-resolved `[<name>]` table in `config.toml` (F-58/65, #554).
pub fn plugin_spec(
    store: &PluginStore,
    cfg: &RootConfig,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<PluginSpec, SpecError> {
    let manifest = store
        .manifest_of(name)?
        .ok_or_else(|| SpecError::NotInstalled(name.to_string()))?;
    let init_config = plugin_init_config(cfg, name, env)?;
    let timeout = cfg
        .plugin(name)
        .and_then(|p| p.timeout_secs)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_PLUGIN_TIMEOUT);
    let is_source = manifest.kind == PluginKind::TaskSource;
    // Every plugin a workflow names — `source` or `agent` — is told about that
    // workflow (0.6.0, #554), because a plugin-owned option written on it may
    // belong to either. `trigger` stays a source's alone: it selects tasks,
    // and an agent has no say in that.
    let workflows = workflow_infos(cfg, name, is_source);
    // task_source plugins additionally get the orchestrator's repository list
    // (#109), `[llm]` settings (#119), and `poll_interval_secs` (0.1.6), so a
    // push source knows its watch cadence without a call carrying it.
    let (repositories, projects, llm, poll_interval_secs) = if is_source {
        let poll = cfg.plugin(name).and_then(|p| p.poll_interval_secs);
        (
            repo_infos(cfg, env),
            project_infos(cfg, name),
            llm_info(cfg, env),
            poll,
        )
    } else {
        (vec![], vec![], None, None)
    };
    Ok(PluginSpec {
        name: name.to_string(),
        program: store.plugin_dir(name).join(&manifest.name),
        args: vec![],
        manifest,
        init_config,
        repositories,
        projects,
        llm,
        workflows,
        poll_interval_secs,
        timeout,
    })
}

/// The `[[projects]]` entries `name` owns, with their opaque options (#554).
///
/// Filtered here rather than sent whole and filtered plugin-side: `source` is
/// the Orchestrator's key on the entry, so deciding whose it is *is* its job —
/// and a plugin that received other plugins' trackers would have to be trusted
/// to ignore them.
fn project_infos(cfg: &RootConfig, name: &str) -> Vec<ProjectInfo> {
    cfg.projects
        .iter()
        .filter(|p| p.source == name)
        .map(|p| ProjectInfo {
            name: p.name.clone(),
            options: match serde_json::to_value(&p.options) {
                Ok(Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            },
        })
        .collect()
}

/// The workflows naming `name`, in `[[workflows]]` definition order.
///
/// Definition order is load-bearing for a source: it reproduces the
/// Orchestrator's first-match rule (F-81) on its own side. For an agent the
/// order carries nothing, but keeping one list keeps one contract.
pub fn workflow_infos(cfg: &RootConfig, name: &str, is_source: bool) -> Vec<WorkflowInfo> {
    cfg.workflows
        .iter()
        .filter(|w| {
            if is_source {
                w.source == name
            } else {
                w.agent == name
            }
        })
        .map(|w| WorkflowInfo {
            workflow: w.name.clone(),
            // An agent is sent an empty object rather than `null`: a plugin
            // reading `.get("…")` off `null` mis-branches, which is the same
            // reason the catch-all trigger is `{}` (#396).
            trigger: if is_source {
                trigger_value(w)
            } else {
                Value::Object(serde_json::Map::new())
            },
            options: workflow_options(w),
        })
        .collect()
}

/// A workflow's plugin-owned keys as a JSON object (#554).
///
/// Empty when the workflow writes none, which is what every config written
/// before this existed says.
pub fn workflow_options(wf: &crate::config::WorkflowConfig) -> serde_json::Map<String, Value> {
    match serde_json::to_value(&wf.options) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

/// `config.toml` `[[repositories]]` mapped to the protocol's [`RepoInfo`],
/// with paths `~`/`${ENV}`-expanded (best effort: an unresolvable path is
/// passed through raw — the plugin treats paths as optional material).
fn repo_infos(cfg: &RootConfig, env: &HashMap<String, String>) -> Vec<RepoInfo> {
    let env_fn = |k: &str| env.get(k).cloned();
    cfg.repositories
        .iter()
        .map(|repo| {
            let raw = repo.path.to_string_lossy();
            let path = config::expand_path(&raw, &env_fn)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| raw.into_owned());
            RepoInfo {
                name: repo.name.clone(),
                summary: repo.summary.clone(),
                path: Some(path),
                project: repo.project.clone(),
            }
        })
        .collect()
}

/// `config.toml` `[llm]` mapped to the protocol's [`LlmInfo`] with its
/// `api_key_ref` resolved (F-65), supplied to task_source plugins as a
/// source-side classification default (#119). Best effort: an unresolvable
/// key reference yields `None` (nothing supplied) rather than an error —
/// `doctor`'s dedicated `llm` check reports the broken reference, and
/// `totsuka run` fails when building the orchestrator's own router from the
/// same reference, so the problem surfaces where it can be acted on without
/// also failing every plugin launch here.
fn llm_info(cfg: &RootConfig, env: &HashMap<String, String>) -> Option<LlmInfo> {
    let llm = cfg.llm.as_ref()?;
    let api_key = match &llm.api_key_ref {
        Some(reference) => match secret_resolver(env).resolve(reference) {
            Ok(secret) => Some(secret.expose().to_string()),
            Err(_) => return None,
        },
        None => None,
    };
    Some(LlmInfo {
        base_url: llm.base_url.clone(),
        model: llm.model.clone(),
        api_key,
    })
}

/// Take the plugin's `[<name>]` table from `config.toml` (empty object when
/// it wrote none) and resolve the secret references in its string values
/// (F-65, #554).
///
/// The table is never interpreted: it round-trips TOML → JSON with only the
/// string leaves rewritten, which is the same contract `plugins/{name}.toml`
/// had before the two files became one. Resolution is scoped to this subtree
/// on purpose — the Orchestrator's own fields resolve their references by
/// name, where each one is used.
pub fn plugin_init_config(
    cfg: &RootConfig,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<Value, SpecError> {
    let mut value = match cfg.plugin_settings(name) {
        Some(table) => serde_json::to_value(table).map_err(ConfigError::from)?,
        None => Value::Object(serde_json::Map::new()),
    };
    let resolver = secret_resolver(env);
    resolve_strings(&mut value, &resolver).map_err(|source| SpecError::Resolve {
        name: name.to_string(),
        source,
    })?;
    Ok(value)
}

/// A workflow's trigger as the plugin receives it, with the profile-derived
/// keys the Orchestrator adds (#398).
///
/// # Why the trigger table carries this
///
/// A source plugin has to write different instructions for a `design` task than
/// for an `implement` one — where to put the design comment, what URL to report
/// back. It cannot read the profile: `[[workflows]]` is the Orchestrator's
/// schema, and teaching every plugin about profiles would make each one depend
/// on a core concept that keeps changing.
///
/// So the Orchestrator translates. The trigger is already a plugin-defined
/// `Value` that plugins parse loosely, so an extra key rides along with **no
/// protocol change and no version bump**: an older plugin ignores what it does
/// not recognise and behaves exactly as before.
///
/// The **cost is that the degradation is silent**. A new Orchestrator against
/// an old plugin sends `instructions_kind`, gets no instructions back in the
/// task, and dispatches an agent that was never told where to write. Nothing
/// errors. There is no capability flag to probe for, so this cannot be checked
/// at runtime — release core and the source plugins together.
fn trigger_value(wf: &crate::config::WorkflowConfig) -> serde_json::Value {
    let mut value = serde_json::to_value(&wf.trigger).unwrap_or(serde_json::Value::Null);
    let Some(table) = value.as_object_mut() else {
        return value;
    };
    for (key, derived) in [
        ("instructions_kind", wf.profile.and_then(instructions_kind)),
        ("task_id_prefix", wf.profile.and_then(task_id_prefix)),
    ] {
        if let Some(v) = derived {
            table.insert(key.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    value
}

/// The task-id prefix a profile's tasks carry, or `None` when they keep the
/// plain conversation id (#397, #393 D7).
///
/// A Slack thread can already have an `answer` task at `{channel}:{thread_ts}`
/// ([ADR-0015](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0015-conversation-task-identity.md)),
/// and `UNIQUE(source, source_task_id)` means a second task on the same thread
/// needs a different id. Prefixing is what lets "answer this" and "now
/// implement it" coexist as separate tasks with separate worktrees, instead of
/// one task whose permissions widen mid-run.
///
/// `answer` has no prefix on purpose: it *is* the conversation, and taking the
/// plain id is what makes a follow-up mention continue it rather than open a
/// second one.
fn task_id_prefix(profile: crate::config::Profile) -> Option<&'static str> {
    use crate::config::Profile;
    match profile {
        Profile::Implement => Some("impl"),
        // `books:` is #324's existing design for the Slack triage flow; this
        // keeps that spelling rather than inventing a second one.
        Profile::Triage => Some("books"),
        // `design` is GitHub/Notion-sourced, where the task id is the issue's
        // own — there is no sibling task to collide with.
        Profile::Design | Profile::Answer => None,
    }
}

/// Which instruction set a profile asks its source plugin for, or `None` when
/// the plugin should keep its existing behaviour.
///
/// `answer` is absent on purpose: its reply goes back through the plugin's own
/// publish path, so the plugin already knows what to say and has always said
/// it. Sending a kind it has no text for would be a key that reads as
/// configured and does nothing.
fn instructions_kind(profile: crate::config::Profile) -> Option<&'static str> {
    use crate::config::Profile;
    match profile {
        Profile::Triage => Some("triage"),
        Profile::Design => Some("design"),
        Profile::Implement => Some("implement"),
        Profile::Answer => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(toml: &str) -> RootConfig {
        RootConfig::from_toml_str(toml).unwrap()
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The `initialize.triggers` contract has existed since protocol 0.1.6,
    /// and #396 made a plugin depend on it for the first time: the Slack
    /// plugin reads `trigger.reaction` from here to know which emoji start a
    /// task. Silently dropping the key — or reordering the list — turns the
    /// emoji into a no-op with no error anywhere.
    #[test]
    fn a_task_sources_triggers_arrive_whole_and_in_definition_order() {
        let cfg = root(
            r#"
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"

[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"
agent = "herdr"
"#,
        );
        let triggers = workflow_infos(&cfg, "slack", true);

        // Only this source's workflows, in definition order — the plugin
        // reproduces first-match from this list.
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].workflow, "slack-implement");
        assert_eq!(triggers[1].workflow, "slack-reply");
        assert_eq!(
            triggers[0].trigger.get("reaction").and_then(|v| v.as_str()),
            Some("hammer")
        );
        // The catch-all is an empty object, never `null`: a plugin reading
        // `.get("reaction")` on `null` would panic or mis-branch.
        assert!(triggers[1].trigger.is_object());
    }

    /// The plugin-owned `[[workflows]]` keys reach the plugins the workflow
    /// names — **both** of them (#554). The Orchestrator cannot tell whose a
    /// key is, so sending it to only one would decide that question by
    /// omission.
    #[test]
    fn workflow_options_reach_the_source_and_the_agent() {
        let cfg = root(
            r#"
[[workflows]]
name = "slack-books"
source = "slack"
agent = "herdr"
profile = "triage"
publish = "direct"

[[workflows]]
name = "gh-design"
source = "github"
agent = "herdr"
profile = "design"
"#,
        );
        let slack = workflow_infos(&cfg, "slack", true);
        assert_eq!(slack.len(), 1);
        assert_eq!(slack[0].options["publish"], serde_json::json!("direct"));

        // The agent sees the same key — and every workflow naming it, not just
        // the ones with options.
        let herdr = workflow_infos(&cfg, "herdr", false);
        assert_eq!(herdr.len(), 2);
        let books = herdr.iter().find(|w| w.workflow == "slack-books").unwrap();
        assert_eq!(books.options["publish"], serde_json::json!("direct"));
        // …but not the trigger: selecting tasks is the source's business.
        assert_eq!(books.trigger, serde_json::json!({}));

        // A workflow with no plugin keys carries an empty map, never `null`.
        let design = herdr.iter().find(|w| w.workflow == "gh-design").unwrap();
        assert!(design.options.is_empty());
    }

    /// The profile → `instructions_kind` translation the source plugins read
    /// (#398), and the two silences that are deliberate.
    #[test]
    fn a_profile_bakes_its_instructions_kind_into_the_trigger() {
        let cfg = root(
            r#"
[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"
agent = "herdr"

[[workflows]]
name = "gh-implement"
source = "github"
trigger = { project_status = "実装待ち" }
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"

[[workflows]]
name = "spelled-out"
source = "github"
trigger = { project_status = "その他" }
mode = "plan"
output = "source"
agent = "herdr"
"#,
        );
        let kind_of = |name: &str| {
            let wf = cfg.workflows.iter().find(|w| w.name == name).unwrap();
            trigger_value(wf)
                .get("instructions_kind")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        let prefix_of = |name: &str| {
            let wf = cfg.workflows.iter().find(|w| w.name == name).unwrap();
            trigger_value(wf)
                .get("task_id_prefix")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        assert_eq!(kind_of("gh-design").as_deref(), Some("design"));
        assert_eq!(kind_of("gh-implement").as_deref(), Some("implement"));
        // `answer` publishes through the plugin's own path, which already knows
        // what to say. A kind it has no text for would read as configured and
        // do nothing.
        assert_eq!(kind_of("slack-reply"), None);
        // The spelled-out notation has no profile to translate.
        assert_eq!(kind_of("spelled-out"), None);

        // #397: the id prefix that keeps an `impl:` task from colliding with
        // the `answer` task on the same Slack thread.
        assert_eq!(prefix_of("gh-implement").as_deref(), Some("impl"));
        // `answer` is the conversation itself, so it takes the plain id — that
        // is what makes a follow-up mention continue it (ADR-0015).
        assert_eq!(prefix_of("slack-reply"), None);
        // `design` is issue-sourced: its task id is the issue's own, with no
        // sibling to collide with.
        assert_eq!(prefix_of("gh-design"), None);
        assert_eq!(prefix_of("spelled-out"), None);

        // The existing trigger keys survive — the plugin still filters on them.
        let design = cfg
            .workflows
            .iter()
            .find(|w| w.name == "gh-design")
            .unwrap();
        assert_eq!(
            trigger_value(design)
                .get("project_status")
                .and_then(|v| v.as_str()),
            Some("設計待ち")
        );
    }

    #[test]
    fn llm_info_is_none_without_an_llm_table() {
        assert!(llm_info(&root(""), &env(&[])).is_none());
    }

    #[test]
    fn llm_info_resolves_an_env_key_reference() {
        let cfg = root(
            r#"
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4.5"
api_key_ref = "${OPENROUTER_API_KEY}"
"#,
        );
        let info = llm_info(&cfg, &env(&[("OPENROUTER_API_KEY", "sk-or-test")])).unwrap();
        assert_eq!(info.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(info.model, "anthropic/claude-haiku-4.5");
        assert_eq!(info.api_key.as_deref(), Some("sk-or-test"));
    }

    #[test]
    fn llm_info_without_a_key_reference_has_no_key() {
        let cfg = root(
            r#"
[llm]
base_url = "http://localhost:11434/v1"
model = "local"
"#,
        );
        let info = llm_info(&cfg, &env(&[])).unwrap();
        assert!(info.api_key.is_none());
    }

    #[test]
    fn llm_info_is_best_effort_on_an_unresolvable_reference() {
        // Nothing is supplied rather than failing every plugin launch —
        // doctor's `llm` check and `totsuka run`'s own router construction
        // surface the broken reference.
        let cfg = root(
            r#"
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "m"
api_key_ref = "${UNSET_VAR_FOR_TEST}"
"#,
        );
        assert!(llm_info(&cfg, &env(&[])).is_none());
    }
}
