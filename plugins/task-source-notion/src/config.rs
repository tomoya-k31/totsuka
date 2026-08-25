//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `[notion]` table of `config.toml` as JSON with secrets already
//! expanded (F-65, #554).
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
/// `[notion.prompts]` in config.toml.
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

/// The options on one `[[projects]]` entry, as this plugin reads them.
///
/// `name` and `source` are the Orchestrator's keys and never reach here;
/// `deny_unknown_fields` turns a typo in the rest into an `initialize`
/// failure rather than a setting that quietly does nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseOptions {
    /// The database queried for tasks.
    pub database_id: String,
    /// The status option a triage-filed page should be created with (#548
    /// follow-up).
    ///
    /// Absent means the page is created without a status, leaving a
    /// human-triage gate. **Setting this to a value some workflow trigger
    /// polls removes that gate** — filing then flows straight into an
    /// unattended run. Requires `property_map.status` to be mapped:
    /// `static_config_errors` rejects the combination — but that check runs
    /// only via `config/validate`, **not at `initialize`**. A config that was
    /// never validated starts fine and silently omits the status instruction
    /// from the destination, because there is no column to name.
    #[serde(default)]
    pub triage_status: Option<String>,
}

/// One database this plugin polls: its `[[projects]]` entry plus the
/// repositories bound to it.
///
/// The repositories are **derived** (#554) from `[[repositories]].project`,
/// which the Orchestrator supplies on `RepoInfo`. They used to be written
/// here as `repos = [...]` ([ADR-0056](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md)).
///
/// **As an ingest filter this list is conditional, unlike the GitHub
/// plugin's.** There, every issue carries a repository. Here the repository
/// comes from the optional `repo_hint` property, so a page that has none is
/// ingested and the Orchestrator resolves its repository as it did before
/// (F-11) — filtering those out would silently ingest nothing at all for
/// anyone who has not mapped `repo_hint`.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// The entry's `name` — what `[[repositories]].project` points at.
    pub name: String,
    /// The database queried for tasks.
    pub database_id: String,
    /// The status option a triage-filed page is created with.
    pub triage_status: Option<String>,
    /// The repositories bound to this database, in `[[repositories]]` order.
    pub repos: Vec<String>,
}

impl DatabaseConfig {
    /// A database built directly, for tests and for callers that already have
    /// the pieces. Production builds these through [`resolve`](Self::resolve).
    pub fn new(name: &str, database_id: &str, repos: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            database_id: database_id.to_string(),
            triage_status: None,
            repos: repos.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    /// Build the databases from what `initialize` supplied (#554).
    pub fn resolve(
        projects: &[plugin_protocol::methods::ProjectInfo],
        repositories: &[plugin_protocol::methods::RepoInfo],
    ) -> Result<Vec<Self>, Vec<String>> {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for info in projects {
            let options: DatabaseOptions =
                match serde_json::from_value(serde_json::Value::Object(info.options.clone())) {
                    Ok(o) => o,
                    Err(e) => {
                        errors.push(format!("project `{}`: {e}", info.name));
                        continue;
                    }
                };
            out.push(Self {
                name: info.name.clone(),
                database_id: options.database_id,
                triage_status: options.triage_status,
                repos: repositories
                    .iter()
                    .filter(|r| r.project.as_deref() == Some(info.name.as_str()))
                    .map(|r| r.name.clone())
                    .collect(),
            });
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }
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
    /// The databases this plugin polls.
    ///
    /// **Not read from `[notion]`** (#554): the entries live in the
    /// Orchestrator's `[[projects]]`, and their repositories in
    /// `[[repositories]].project`. Filled in at `initialize` by
    /// [`DatabaseConfig::resolve`]; `deny_unknown_fields` on the surrounding
    /// struct is what rejects a `databases = [...]` left over in `[notion]`.
    #[serde(skip)]
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
    /// Internal fetch cadence of the poll loop, in seconds (F-06). Moved
    /// here from `[plugins.notion]` in 0.6.0 (#554): the Orchestrator only
    /// ever forwarded it, so it is this plugin's own key. `0` is treated as
    /// unset (busy-spin guard, applied in the server).
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
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
            match &database.triage_status {
                // Name the exact value, so the agent creates the page already
                // in the right column instead of guessing one or leaving it
                // blank.
                Some(value) => {
                    columns.push(format!("`{status}` (status — set it to `{value}`)"));
                }
                None => columns.push(format!("`{status}` (status)")),
            }
        }
        if let Some(repo_hint) = &map.repo_hint {
            columns.push(format!("`{repo_hint}` (the repository name)"));
        }
        format!(
            "Notion database `{}`. Create a page there and fill {}. \
             Totsuka does not create Notion pages itself, so use whatever Notion \
             tooling you have available (an MCP server, the API with your own token).",
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
/// Build a config the way `initialize` does, for tests: the `[notion]` table
/// minus the databases, plus the databases resolved from `[[projects]]` /
/// `[[repositories]]` (#554).
///
/// Tests write the database inline as `"databases": [{ name, database_id,
/// repos }]` and this splits it into the two lists the Orchestrator actually
/// sends, so the test exercises [`DatabaseConfig::resolve`] rather than
/// bypassing it.
#[cfg(test)]
pub(crate) fn config_from_json(mut value: serde_json::Value) -> NotionConfig {
    use plugin_protocol::methods::{ProjectInfo, RepoInfo};

    let entries = value
        .as_object_mut()
        .and_then(|o| o.remove("databases"))
        .and_then(|d| d.as_array().cloned())
        .unwrap_or_default();
    let mut projects = Vec::new();
    let mut repositories = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let mut options = entry.as_object().cloned().unwrap_or_default();
        let repos = options.remove("repos").unwrap_or(serde_json::json!([]));
        let name = options
            .remove("name")
            .and_then(|n| n.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("db-{i}"));
        for repo in repos.as_array().cloned().unwrap_or_default() {
            repositories.push(RepoInfo {
                name: repo.as_str().unwrap_or_default().to_string(),
                summary: None,
                path: None,
                project: Some(name.clone()),
            });
        }
        projects.push(ProjectInfo { name, options });
    }
    let mut config: NotionConfig = serde_json::from_value(value).expect("notion config");
    config.databases =
        DatabaseConfig::resolve(&projects, &repositories).expect("databases resolve");
    config
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

    /// The destination is prose that lands in an agent's prompt, so it must
    /// read as prose: no run of spaces from a line continuation written
    /// without its backslash, and no newline.
    ///
    /// Checked on the rendered value rather than by reading the literal —
    /// that is the thing the agent sees.
    #[test]
    fn the_destination_reads_as_one_paragraph() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["totsuka"] }],
            "property_map": { "title": "Name", "status": "Status", "repo_hint": "Repo" }
        }));
        let destination = &cfg.claimed_repos()[0].destination;
        assert!(
            !destination.contains("  ") && !destination.contains('\n'),
            "{destination:?}"
        );
        // And it carries what an agent creating the page cannot guess: the
        // database and the operator's own column names.
        for needle in ["db1", "`Name`", "`Status`", "`Repo`"] {
            assert!(
                destination.contains(needle),
                "{needle} missing: {destination}"
            );
        }
    }

    /// `triage_status` names the exact value inline on the status column;
    /// absent keeps the plain column listing.
    #[test]
    fn triage_status_lands_in_the_destination_only_when_set() {
        let with = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["totsuka"],
                            "triage_status": "📥 Inbox" }],
            "property_map": { "title": "Name", "status": "Status" }
        }));
        let destination = &with.claimed_repos()[0].destination;
        assert!(
            destination.contains("`Status` (status — set it to `📥 Inbox`)"),
            "{destination}"
        );
        assert!(
            !destination.contains("  ") && !destination.contains('\n'),
            "{destination:?}"
        );

        let without = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["totsuka"] }],
            "property_map": { "title": "Name", "status": "Status" }
        }));
        let destination = &without.claimed_repos()[0].destination;
        assert!(destination.contains("`Status` (status)"), "{destination}");
        assert!(!destination.contains("set it to"), "{destination}");
    }

    /// Only the mapped columns appear. Naming a property the operator never
    /// mapped would send the agent looking for a column that does not exist.
    #[test]
    fn the_destination_names_only_mapped_columns() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["totsuka"] }],
            "property_map": { "title": "Name" }
        }));
        let destination = &cfg.claimed_repos()[0].destination;
        assert!(destination.contains("`Name`"), "{destination}");
        assert!(!destination.contains("(status)"), "{destination}");
        assert!(!destination.contains("repository name"), "{destination}");
    }

    /// The "a repository is on two databases" check (#542) is gone here for
    /// the same reason as in the github plugin: #554 made the state unwritable
    /// rather than invalid, because `repos` is derived from the
    /// `[[repositories]]` entries whose single-valued `project` names this
    /// database. Pinned through `resolve`, not through the absence of an error
    /// — the latter would keep passing if the derivation itself started
    /// handing one repository to two databases.
    #[test]
    fn resolve_gives_each_repository_to_exactly_one_database() {
        use plugin_protocol::methods::{ProjectInfo, RepoInfo};

        let project = |name: &str, id: &str| ProjectInfo {
            name: name.to_string(),
            options: serde_json::json!({ "database_id": id })
                .as_object()
                .unwrap()
                .clone(),
        };
        let repo = |name: &str, project: &str| RepoInfo {
            name: name.to_string(),
            summary: None,
            path: None,
            project: Some(project.to_string()),
        };

        let databases = DatabaseConfig::resolve(
            &[project("design", "db1"), project("ops", "db2")],
            &[
                repo("totsuka", "design"),
                repo("shared", "design"),
                repo("infra", "ops"),
            ],
        )
        .expect("resolves");

        let mut homes: Vec<(&str, &str)> = Vec::new();
        for database in &databases {
            for r in &database.repos {
                homes.push((r.as_str(), database.name.as_str()));
            }
        }
        homes.sort_unstable();
        assert_eq!(
            homes,
            [
                ("infra", "ops"),
                ("shared", "design"),
                ("totsuka", "design")
            ]
        );
    }

    /// The same repository twice **within one database** is harmless: both
    /// entries name the same destination.
    #[test]
    fn a_repository_repeated_within_one_database_is_not_an_error() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["r", "r"] }],
            "property_map": { "title": "Name", "status": "Status" }
        }));
        assert!(crate::client::static_config_errors(&cfg).is_empty());
    }

    /// `triage_status` with no mapped status property is an instruction to
    /// fill a column nobody can name — rejected at validation.
    #[test]
    fn static_errors_flag_triage_status_without_a_status_property() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["r"],
                            "triage_status": "Inbox" }],
            "property_map": { "title": "Name" }
        }));
        let errors = crate::client::static_config_errors(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("triage_status") && e.contains("property_map.status")),
            "got {errors:?}"
        );
    }

    /// A database no repository points at polls nothing and claims nothing, so
    /// it is reported. The message names the entry and the key to set, because
    /// there is no longer a `repos` list to fill in on the database side.
    #[test]
    fn static_errors_flag_a_database_no_repository_is_bound_to() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "name": "lonely", "database_id": "db1", "repos": [] }],
            "property_map": { "title": "Name", "status": "Status" }
        }));
        let errors = crate::client::static_config_errors(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("lonely") && e.contains("[[repositories]]")),
            "got {errors:?}"
        );
    }

    /// `databases = []` deserializes fine (it is a list, not a missing field),
    /// so the check has to live in validation.
    #[test]
    fn static_errors_flag_no_databases() {
        let cfg = parse(serde_json::json!({
            "token": "t", "databases": [],
            "property_map": { "title": "Name", "status": "Status" }
        }));
        let errors = crate::client::static_config_errors(&cfg);
        assert!(
            errors.iter().any(|e| e.contains(r#"`source = "notion"`"#)),
            "got {errors:?}"
        );
    }

    /// A page whose `repo_hint` is absent passes every database's filter — the
    /// asymmetry with the github plugin, where an issue always has a
    /// repository. Dropping those would ingest nothing at all for anyone who
    /// has not mapped `repo_hint`.
    #[test]
    fn a_page_without_a_repo_hint_passes_the_filter() {
        let cfg = parse(serde_json::json!({
            "token": "t",
            "databases": [{ "database_id": "db1", "repos": ["totsuka"] }]
        }));
        let database = &cfg.databases[0];
        assert!(database.repo_allowed(Some("totsuka")));
        assert!(!database.repo_allowed(Some("other")));
        assert!(database.repo_allowed(None));
    }

    fn parse(json: serde_json::Value) -> NotionConfig {
        config_from_json(json)
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
            "token": "t", "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    /// A `databases = [...]` left in `[notion]` is now an unknown field: the
    /// entries moved to the Orchestrator's `[[projects]]` (#554), and
    /// accepting the old spelling would leave a table that reads as configured
    /// and is never consulted.
    #[test]
    fn the_old_databases_key_in_the_plugin_table_is_rejected() {
        let err = serde_json::from_value::<NotionConfig>(serde_json::json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["totsuka"] }]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("databases"), "got {err}");
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
