//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/notion.toml` as JSON with secrets already expanded (F-64/F-65).
//!
//! The [`PropertyMap`] is what lets one plugin serve *any* database layout
//! (F-03): it names which Notion property carries each field of the shared
//! [`plugin_protocol::Task`] schema (F-01).

use std::collections::HashMap;

use std::sync::LazyLock;

use plugin_protocol::methods::ClaimedRepo;
use serde::Deserialize;

/// The embedded instruction defaults, parsed once on first use.
///
/// A malformed `defaults.toml` is an authoring error in a file that ships
/// inside the binary — no input can change it — so this panics rather than
/// degrading. `embedded_defaults_parse` forces it in CI instead of at
/// `initialize`.
static DEFAULTS: LazyLock<EmbeddedPrompts> = LazyLock::new(|| {
    toml::from_str::<Defaults>(include_str!("defaults.toml"))
        .expect("embedded defaults.toml must parse")
        .prompts
});

/// Top level of `defaults.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    prompts: EmbeddedPrompts,
}

/// The embedded defaults, with **every field required**.
///
/// A separate type from [`NotionPrompts`], and the duplication is the point —
/// the same trap the Slack and GitHub plugins document. `NotionPrompts` fills omitted keys
/// from `DEFAULTS`; if `DEFAULTS` were also a `NotionPrompts`, deleting a key
/// from `defaults.toml` would make its `#[serde(default)]` read `DEFAULTS`
/// **while `DEFAULTS` is still initialising** — a re-entrant `LazyLock`, which
/// **deadlocks rather than panicking**. The symptom would be a CI job hanging
/// to its timeout instead of a test failing with a readable message.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedPrompts {
    triage_instructions: String,
    design_instructions: String,
    implement_instructions: String,
}

/// Instruction text this plugin sends with each task (#398, epic #311).
///
/// Built-in values live in the embedded `defaults.toml`, not in Rust string
/// literals, so rewording is a data edit. Field names are the config keys under
/// `[prompts]` in `plugins/notion.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotionPrompts {
    /// Sent when the workflow's profile is `triage`.
    #[serde(default = "default_triage_instructions")]
    pub triage_instructions: String,
    /// Sent when the workflow's profile is `design`.
    #[serde(default = "default_design_instructions")]
    pub design_instructions: String,
    /// Sent when the workflow's profile is `implement`.
    #[serde(default = "default_implement_instructions")]
    pub implement_instructions: String,
}

fn default_triage_instructions() -> String {
    DEFAULTS.triage_instructions.clone()
}
fn default_design_instructions() -> String {
    DEFAULTS.design_instructions.clone()
}
fn default_implement_instructions() -> String {
    DEFAULTS.implement_instructions.clone()
}

impl Default for NotionPrompts {
    fn default() -> Self {
        Self {
            triage_instructions: default_triage_instructions(),
            design_instructions: default_design_instructions(),
            implement_instructions: default_implement_instructions(),
        }
    }
}

impl NotionPrompts {
    /// The template for `kind`, or `None` when the Orchestrator sent no
    /// `instructions_kind` — or sent one this plugin has no text for.
    ///
    /// An unknown kind returns `None` rather than falling back to a default:
    /// guessing which instruction a future profile wants would put the agent to
    /// work on the wrong deliverable, which is worse than dispatching it with
    /// the instructions it had before (#398 predates any such profile, so this
    /// arm is reachable only from a newer core).
    pub fn for_kind(&self, kind: &str) -> Option<&str> {
        match kind {
            "triage" => Some(&self.triage_instructions),
            "design" => Some(&self.design_instructions),
            "implement" => Some(&self.implement_instructions),
            _ => None,
        }
    }
}

/// Whether the project owner is a user or an organization (GraphQL requires
/// choosing the right root field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OwnerType {
    /// A user account (`user(login:)`).
    #[default]
    User,
    /// An organization (`organization(login:)`).
    Organization,
}

impl OwnerType {
    /// The GraphQL root field selecting this owner's `projectV2`.
    pub fn graphql_root(self) -> &'static str {
        match self {
            OwnerType::User => "user",
            OwnerType::Organization => "organization",
        }
    }
}

/// The Notion property type backing the status column. The write-back body
/// (F-84) and option lookup differ between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// A `status` property (the dedicated Notion status type).
    #[default]
    Status,
    /// A `select` property used as a status.
    Select,
}

impl StatusKind {
    /// The property-object key holding the option (`status` or `select`).
    pub fn key(self) -> &'static str {
        match self {
            StatusKind::Status => "status",
            StatusKind::Select => "select",
        }
    }
}

/// Where a task's body text comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BodySource {
    /// No body is ingested.
    #[default]
    None,
    /// A `rich_text` property named by [`PropertyMap::body`].
    Property,
    /// The page's block content, fetched and converted to Markdown
    /// (`v1`: major block types only, F-03).
    Page,
}

/// Maps the shared [`Task`](plugin_protocol::Task) fields onto this database's
/// Notion property names (F-03). Only [`title`](Self::title) is mandatory;
/// unset optional fields are simply not extracted.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertyMap {
    /// The `title` property (Notion's default is `Name`).
    #[serde(default = "default_title_prop")]
    pub title: String,
    /// The status property (`status` or `select`, see [`status_kind`]).
    ///
    /// [`status_kind`]: Self::status_kind
    #[serde(default)]
    pub status: Option<String>,
    /// The Notion property type backing [`status`](Self::status).
    #[serde(default)]
    pub status_kind: StatusKind,
    /// A `people` property holding assignees (F-08).
    #[serde(default)]
    pub assignee: Option<String>,
    /// A `number`/`select`/`status` property holding priority.
    #[serde(default)]
    pub priority: Option<String>,
    /// A property carrying a repository hint (`rich_text`/`select`/`url`, F-10).
    #[serde(default)]
    pub repo_hint: Option<String>,
    /// A `rich_text` property carrying the body, when
    /// [`body_source`](NotionConfig::body_source) is `property`.
    #[serde(default)]
    pub body: Option<String>,
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self {
            title: default_title_prop(),
            status: None,
            status_kind: StatusKind::default(),
            assignee: None,
            priority: None,
            repo_hint: None,
            body: None,
        }
    }
}

/// One Notion database this plugin polls, and the repositories it is the
/// tracker for (`[[databases]]`, #542).
///
/// A list rather than one-plugin-instance-per-database, for the reason
/// [ADR-0056](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md)
/// gives: instances would need `name ≠ bin name`, which ADR-0027 refused.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// The database queried for tasks.
    pub database_id: String,
    /// The repositories this database is the tracker for.
    ///
    /// Required and non-empty because it is the forward mapping repository →
    /// database that [`NotionConfig::claimed_repos`] publishes; a database
    /// cannot say which repositories it will hold in future, so an omitted
    /// list has nothing to publish.
    ///
    /// **As an ingest filter it is conditional, unlike the GitHub plugin's.**
    /// There, every issue carries a repository. Here the repository comes from
    /// the optional `repo_hint` property, so a page that has none is ingested
    /// and the Orchestrator resolves its repository as it did before (F-11) —
    /// filtering those out would silently ingest nothing at all for anyone who
    /// has not mapped `repo_hint`.
    pub repos: Vec<String>,
}

impl DatabaseConfig {
    /// Whether a page whose `repo_hint` is `repo` belongs to this database's
    /// tracked set. `None` (no hint on the page) always passes — see
    /// [`repos`](Self::repos).
    pub fn repo_allowed(&self, repo: Option<&str>) -> bool {
        match repo {
            Some(repo) => self.repos.iter().any(|r| r == repo),
            None => true,
        }
    }
}

/// Notion task-source settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotionConfig {
    /// Integration token (resolved by the orchestrator, F-65). Never touched by
    /// us beyond sending it as a bearer token.
    pub token: String,
    /// The databases this plugin polls (`[[databases]]`, #542). Required and
    /// non-empty; `static_config_errors` rejects an empty list.
    ///
    /// This replaced the flat `database_id` outright. `deny_unknown_fields`
    /// makes a pre-#542 config a hard `initialize` failure, which is the
    /// intended outcome (ADR-0056).
    pub databases: Vec<DatabaseConfig>,
    /// The operator's own Notion user id, used to detect self-assigned tasks
    /// (F-08). When unset, self-detection is disabled: only *unassigned* tasks
    /// are ingestable (any assigned task is treated as someone else's).
    #[serde(default)]
    pub notion_user_id: Option<String>,
    /// Property-name mapping onto the common schema (F-03).
    #[serde(default)]
    pub property_map: PropertyMap,
    /// Where a task body comes from (F-03).
    #[serde(default)]
    pub body_source: BodySource,
    /// Status option names treated as "in progress" and therefore excluded from
    /// ingest (F-08).
    #[serde(default)]
    pub in_progress_statuses: Vec<String>,
    /// Maps an orchestrator-side status name to the database's option name for
    /// `task/update_status` (F-84). Identity when absent.
    #[serde(default)]
    pub status_map: HashMap<String, String>,
    /// Maps a priority option name (for `select`/`status` priority properties)
    /// to a numeric priority. Higher runs first. A `number` priority property is
    /// used directly and ignores this map.
    #[serde(default)]
    pub priority_map: HashMap<String, i64>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// REST base URL (overridable for tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Pinned Notion API version header (`Notion-Version`).
    #[serde(default = "default_api_version")]
    pub api_version: String,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Client-side request rate cap (requests/second) for the built-in
    /// throttle. Notion's public limit is ~3 rps.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_rps: u32,
    /// Instruction text overrides (#398). Every key falls back to the embedded
    /// default when omitted.
    #[serde(default)]
    pub prompts: NotionPrompts,
}

impl NotionConfig {
    /// Resolve the Notion option name for an orchestrator status via
    /// [`status_map`](Self::status_map), falling back to the name itself.
    pub fn map_status<'a>(&'a self, status: &'a str) -> &'a str {
        self.status_map.get(status).map_or(status, String::as_str)
    }

    /// The repositories this plugin is the tracker for, and where an item for
    /// each goes (`InitializeResult.claimed_repos`, protocol 0.5.1, #542).
    ///
    /// The destination names the required properties as well as the database:
    /// an agent creating the page has to know which columns to fill, and the
    /// names are the operator's, not Notion's.
    pub fn claimed_repos(&self) -> Vec<ClaimedRepo> {
        self.databases
            .iter()
            .flat_map(|database| {
                let destination = self.destination_for(database);
                database.repos.iter().map(move |repo| ClaimedRepo {
                    repo: repo.clone(),
                    destination: destination.clone(),
                })
            })
            .collect()
    }

    /// Prose telling an agent how to file into `database` (#542).
    ///
    /// Lives on `NotionConfig` rather than `DatabaseConfig` because the
    /// property names are shared across databases — the agent needs both
    /// halves to create a usable page.
    fn destination_for(&self, database: &DatabaseConfig) -> String {
        let map = &self.property_map;
        let mut columns = vec![format!("`{}` (title)", map.title)];
        if let Some(status) = &map.status {
            columns.push(format!("`{status}` (status)"));
        }
        if let Some(repo_hint) = &map.repo_hint {
            columns.push(format!("`{repo_hint}` (the repository name)"));
        }
        format!(
            "Notion database `{}`. Create a page there and fill {}.              Totsuka does not create Notion pages itself, so use whatever Notion              tooling you have available (an MCP server, the API with your own token).",
            database.database_id,
            columns.join(", "),
        )
    }

    /// Whether a task with these assignee user ids may be ingested by this
    /// operator (F-08): unassigned, or [`notion_user_id`](Self::notion_user_id)
    /// is among the assignees. A task assigned only to other people is skipped.
    /// Matching is over the whole list, so ingest never depends on ordering.
    pub fn assignable_to_me(&self, assignee_ids: &[&str]) -> bool {
        if assignee_ids.is_empty() {
            return true;
        }
        match &self.notion_user_id {
            Some(me) => assignee_ids.iter().any(|id| id == me),
            None => false,
        }
    }

    /// Whether `status` is an "in progress" column excluded from ingest (F-08).
    pub fn is_in_progress(&self, status: &str) -> bool {
        self.in_progress_statuses.iter().any(|s| s == status)
    }

    /// Numeric priority for a named option via
    /// [`priority_map`](Self::priority_map); `0` when unmapped.
    pub fn priority_value(&self, option: &str) -> i64 {
        self.priority_map.get(option).copied().unwrap_or(0)
    }
}

fn default_title_prop() -> String {
    "Name".to_string()
}
fn default_source_name() -> String {
    "notion".to_string()
}
fn default_api_url() -> String {
    "https://api.notion.com/v1".to_string()
}
fn default_api_version() -> String {
    "2022-06-28".to_string()
}
fn default_max_retries() -> u32 {
    3
}
fn default_rate_limit() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> NotionConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(
            serde_json::json!({ "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }] }),
        );
        assert_eq!(cfg.source_name, "notion");
        assert_eq!(cfg.api_url, "https://api.notion.com/v1");
        assert_eq!(cfg.api_version, "2022-06-28");
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.rate_limit_rps, 3);
        assert_eq!(cfg.property_map.title, "Name");
        assert_eq!(cfg.property_map.status_kind, StatusKind::Status);
        assert_eq!(cfg.body_source, BodySource::None);
        assert!(cfg.notion_user_id.is_none());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = serde_json::from_value::<NotionConfig>(serde_json::json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }], "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn ingest_gating_helpers() {
        let cfg = parse(serde_json::json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }], "notion_user_id": "u_me",
            "in_progress_statuses": ["実装中"]
        }));
        // Unassigned is ingestable; I count as assigned regardless of position;
        // others-only is excluded.
        assert!(cfg.assignable_to_me(&[]));
        assert!(cfg.assignable_to_me(&["u_me"]));
        assert!(cfg.assignable_to_me(&["u_other", "u_me"]));
        assert!(!cfg.assignable_to_me(&["u_other"]));
        assert!(cfg.is_in_progress("実装中"));
        assert!(!cfg.is_in_progress("実装待ち"));
    }

    #[test]
    fn without_user_id_only_unassigned_is_mine() {
        let cfg = parse(
            serde_json::json!({ "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }] }),
        );
        assert!(cfg.assignable_to_me(&[]));
        assert!(!cfg.assignable_to_me(&["u_me"]));
    }

    #[test]
    fn status_and_priority_maps() {
        let cfg = parse(serde_json::json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }],
            "status_map": { "レビュー待ち": "In Review" },
            "priority_map": { "High": 10, "Low": 1 }
        }));
        assert_eq!(cfg.map_status("レビュー待ち"), "In Review");
        assert_eq!(cfg.map_status("実装待ち"), "実装待ち");
        assert_eq!(cfg.priority_value("High"), 10);
        assert_eq!(cfg.priority_value("Unknown"), 0);
    }
}
