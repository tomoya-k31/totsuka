//! Notion task-source operations over a [`NotionTransport`]: fetch + normalize
//! via the property map (F-01/F-03), status write-back (F-84), and
//! token/schema validation (F-59). There is no publish path — the agent writes
//! the deliverable itself (#398). All bodies are plain JSON built
//! with `serde_json` — no Notion SDK dependency.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use plugin_protocol::Task;
use plugin_sdk::AssigneeFilter;

use crate::blocks::{blocks_to_markdown, rich_text_plain};
use crate::config::{BodySource, DatabaseConfig, NotionConfig};
use crate::error::NotionError;
use crate::transport::{HttpMethod, NotionTransport};

/// Pages requested per database query. Notion's max page size is 100.
const QUERY_PAGE_SIZE: usize = 100;

/// Max query pages walked per fetch. Bounds a poll on a very large database;
/// reaching it is logged, not silently truncated.
const MAX_FETCH_PAGES: usize = 40;

/// A parsed trigger condition (workflow-defined shape, F-81). Supports a status
/// value (`{"status": "実装待ち"}`) and/or a raw Notion `filter` object passed
/// straight to the query API.
///
/// `status` used to also be spelled `project_status`; that alias went away with
/// #575. It cost more than a second spelling: the Orchestrator's cycle check
/// reads `status` out of the trigger, so a workflow written the other way was
/// invisible to it and could close a loop without `config validate` noticing.
#[derive(Debug, Default)]
struct TriggerFilter {
    status: Option<String>,
    raw: Option<Value>,
    /// Which instruction set this workflow's profile asks for (#398).
    ///
    /// Derived by the Orchestrator rather than here: `[[workflows]].profile`
    /// is core's schema, and this plugin stays unaware of it. It arrives as
    /// `WorkflowInfo.instructions_kind` (a dedicated field since 0.6.0 /
    /// #554, no longer a key inside the trigger table) and is threaded in via
    /// [`NotionClient::fetch`]. Absent means the task carries no
    /// instructions — exactly the pre-#398 behaviour.
    instructions_kind: Option<String>,
    /// Who may hold the task for this workflow to take it (#572).
    ///
    /// Absent from the trigger means the pre-#572 gate (`["@me", "@none"]`),
    /// so this is the *only* assignee gate — there is no plugin-wide one left
    /// behind it that could overrule what the operator wrote.
    assignee: AssigneeFilter,
}

/// The `[[workflows]].trigger` keys this source reads (#574).
///
/// Kept beside `TriggerFilter::parse` because that is what makes them true.
/// `initialize` rejects every other key, so a typo cannot silently widen a
/// trigger — add a key here in the same edit that teaches the parser to read
/// it.
pub const TRIGGER_KEYS: &[&str] = &["assignee", "filter", "status"];

impl TriggerFilter {
    /// `Err` only for a malformed `assignee`; everything else is parsed
    /// leniently because `initialize` has already rejected unknown keys (#574).
    fn parse(
        trigger: &Value,
        instructions_kind: Option<&str>,
        workflow: &str,
    ) -> Result<Self, String> {
        let status = trigger
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self {
            status,
            raw: trigger.get("filter").cloned(),
            instructions_kind: instructions_kind.map(str::to_string),
            assignee: AssigneeFilter::parse(trigger, workflow)?,
        })
    }

    /// Whether a candidate's status matches what the trigger asked for.
    fn matches(&self, status: Option<&str>) -> bool {
        match &self.status {
            Some(want) => status == Some(want.as_str()),
            None => true,
        }
    }
}

/// Notion task-source client, generic over its transport for testability.
pub struct NotionClient<T> {
    config: NotionConfig,
    transport: T,
    /// Which `[[databases]]` entry an ingested page came from, keyed by page id
    /// (#542).
    ///
    /// `task/update_status` patches the page directly, but it first verifies
    /// the target option exists on **that page's** database, and the request
    /// carries only `{task_id, status}`. A miss is normal (the map is
    /// process-local and the Orchestrator's tasks outlive a restart), so
    /// [`database_of`](Self::database_of) falls back to asking Notion for the
    /// page's parent rather than guessing.
    page_database: Mutex<HashMap<String, usize>>,
}

impl<T: NotionTransport> NotionClient<T> {
    /// A client using `config` and `transport`.
    pub fn new(config: NotionConfig, transport: T) -> Self {
        Self {
            config,
            transport,
            page_database: Mutex::new(HashMap::new()),
        }
    }

    /// The plugin settings.
    pub fn config(&self) -> &NotionConfig {
        &self.config
    }

    /// Fetch database pages matching `trigger`, normalize to [`Task`] via the
    /// property map, and apply ingest gating (F-08): skip other people's tasks
    /// and in-progress statuses. When body comes from the page, its blocks are
    /// fetched only for surviving tasks.
    pub async fn fetch(
        &self,
        trigger: &Value,
        instructions_kind: Option<&str>,
        workflow: &str,
    ) -> Result<Vec<Task>, NotionError> {
        let filter = TriggerFilter::parse(trigger, instructions_kind, workflow)
            .map_err(NotionError::InvalidTrigger)?;
        let server_filter = self.build_filter(&filter);
        let mut tasks = Vec::new();
        for (index, database) in self.config.databases.iter().enumerate() {
            self.fetch_database(index, database, &filter, server_filter.as_ref(), &mut tasks)
                .await?;
        }
        Ok(tasks)
    }

    /// Page one database, appending its ingestable tasks to `tasks`.
    ///
    /// One database failing fails the whole poll, for the reason the GitHub
    /// plugin gives: a skipped database is indistinguishable from a quiet one,
    /// so a revoked token would look like "nothing to do" forever.
    async fn fetch_database(
        &self,
        index: usize,
        database: &DatabaseConfig,
        filter: &TriggerFilter,
        server_filter: Option<&Value>,
        tasks: &mut Vec<Task>,
    ) -> Result<(), NotionError> {
        let mut cursor: Option<String> = None;

        for page_num in 0..MAX_FETCH_PAGES {
            let mut body = json!({ "page_size": QUERY_PAGE_SIZE });
            if let Some(f) = server_filter {
                body["filter"] = f.clone();
            }
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let path = format!("/databases/{}/query", database.database_id);
            let resp = self
                .transport
                .request(HttpMethod::Post, &path, Some(body), true)
                .await?;

            for page in resp["results"].as_array().into_iter().flatten() {
                if let Some(mut task) = self.normalize_page(page, filter) {
                    if !database.repo_allowed(task.repo_hint.as_deref()) {
                        continue;
                    }
                    if self.config.body_source == BodySource::Page {
                        task.body = self.fetch_page_body(&task.id).await?;
                    }
                    self.remember_database(&task.id, index);
                    tasks.push(task);
                }
            }

            if resp["has_more"].as_bool() != Some(true) {
                return Ok(());
            }
            cursor = resp["next_cursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(()); // defensive: has_more without a cursor
            }
            if page_num + 1 == MAX_FETCH_PAGES {
                tracing::warn!(
                    pages = MAX_FETCH_PAGES,
                    database = database.database_id,
                    "reached the fetch page cap; some database pages were not scanned this poll"
                );
            }
        }
        Ok(())
    }

    /// Note that `page_id` lives in database `index`.
    fn remember_database(&self, page_id: &str, index: usize) {
        // A poisoned mutex means an earlier holder panicked. The map is a
        // cache, so carrying on without it beats propagating the panic.
        if let Ok(mut memo) = self.page_database.lock() {
            memo.insert(page_id.to_string(), index);
        }
    }

    /// The configured database holding `page_id`.
    ///
    /// The memo first; otherwise Notion is **asked** (`GET /pages/{id}` →
    /// `parent.database_id`) rather than guessed at. Trying each database's
    /// options in turn would accept an option that exists on some *other*
    /// database, turning a clear "unknown status" error into a confusing
    /// failure from the Notion API on the patch.
    async fn database_of(&self, page_id: &str) -> Result<&DatabaseConfig, NotionError> {
        if let Some(index) = self
            .page_database
            .lock()
            .ok()
            .and_then(|memo| memo.get(page_id).copied())
            && let Some(database) = self.config.databases.get(index)
        {
            return Ok(database);
        }

        let page = self
            .transport
            .request(HttpMethod::Get, &format!("/pages/{page_id}"), None, true)
            .await?;
        let parent = page["parent"]["database_id"].as_str().ok_or_else(|| {
            NotionError::NotFound(format!(
                "page `{page_id}` has no parent database → it is not a database page, so its status cannot be moved"
            ))
        })?;
        // Notion accepts ids with and without hyphens and echoes back the
        // hyphenated form, so compare with them stripped.
        let normalize = |id: &str| id.replace('-', "").to_ascii_lowercase();
        let wanted = normalize(parent);
        let found = self
            .config
            .databases
            .iter()
            .position(|d| normalize(&d.database_id) == wanted);
        match found {
            Some(index) => {
                self.remember_database(page_id, index);
                Ok(&self.config.databases[index])
            }
            None => Err(NotionError::NotFound(format!(
                "page `{page_id}` lives in database `{parent}`, which is not in \
                 any `[[projects]]` entry with `source = \"notion\"` → add it, or check that the task is still where it was"
            ))),
        }
    }

    /// Build the server-side query filter: a raw passthrough if the trigger
    /// carried one, else an `equals` on the mapped status property, else none.
    fn build_filter(&self, filter: &TriggerFilter) -> Option<Value> {
        if let Some(raw) = &filter.raw {
            return Some(raw.clone());
        }
        let status_prop = self.config.property_map.status.as_ref()?;
        let want = filter.status.as_ref()?;
        Some(json!({
            "property": status_prop,
            self.config.property_map.status_kind.key(): { "equals": want }
        }))
    }

    /// Normalize one database page to a [`Task`] via the property map, or `None`
    /// if it is filtered out by trigger or ingest gating (F-08). The body is set
    /// here only for the `property`/`none` sources; the `page` source is filled
    /// by the caller for surviving tasks.
    fn normalize_page(&self, page: &Value, filter: &TriggerFilter) -> Option<Task> {
        let props = &page["properties"];
        let map = &self.config.property_map;

        let status = map
            .status
            .as_deref()
            .and_then(|name| prop_option_name(&props[name]));
        if !filter.matches(status) {
            return None;
        }

        // Ingest gating (F-08): exclude in-progress and other people's tasks.
        if status.is_some_and(|s| self.config.is_in_progress(s)) {
            return None;
        }
        // The assignee gate lives in the trigger now (#572), defaulting to the
        // old plugin-wide rule when the workflow says nothing. With no people
        // property mapped every page reads as unassigned — which is why writing
        // the key at all requires that mapping (`initialize` refuses otherwise).
        let assignee_ids: Vec<&str> = map
            .assignee
            .as_deref()
            .map(|name| people_ids(&props[name]))
            .unwrap_or_default();
        if !filter
            .assignee
            .matches(&assignee_ids, self.config.notion_user_id.as_deref())
        {
            return None;
        }

        let id = page["id"].as_str()?.to_string();
        let title = rich_text_plain(&props[&map.title]["title"]);
        let body = match self.config.body_source {
            BodySource::Property => map
                .body
                .as_deref()
                .map(|name| rich_text_plain(&props[name]["rich_text"]))
                .filter(|b| !b.is_empty()),
            // Filled by the caller from page blocks; None here.
            BodySource::Page | BodySource::None => None,
        };
        let repo_hint = map
            .repo_hint
            .as_deref()
            .and_then(|name| prop_text(&props[name]))
            .filter(|r| !r.is_empty());
        let priority = map
            .priority
            .as_deref()
            .map(|name| self.priority_of(&props[name]))
            .unwrap_or(0);
        let assignee = self.pick_assignee(map.assignee.as_deref().map(|n| &props[n]));

        // Rendered before the struct takes `title` by value.
        let instructions = filter
            .instructions_kind
            .as_deref()
            .and_then(|kind| self.config.prompts.for_kind(kind))
            .map(|template| {
                crate::template::render(
                    template,
                    &[
                        ("page_url", page["url"].as_str().unwrap_or_default()),
                        ("title", title.as_str()),
                    ],
                )
            });
        Some(Task {
            id,
            source: self.config.source_name.clone(),
            title,
            body,
            repo_hint,
            labels: Vec::new(),
            priority,
            status: status.map(str::to_string),
            url: page["url"].as_str().map(str::to_string),
            assignee,
            message_key: None,
            // Layer 1 of ADR-0024: where this task's deliverable goes. Absent
            // unless the Orchestrator asked for a kind (#398), which keeps
            // every pre-profile config behaving exactly as before.
            instructions,
        })
    }

    /// The numeric priority of a property: a `number` value directly, else a
    /// `select`/`status` option name resolved through `priority_map`.
    fn priority_of(&self, prop: &Value) -> i64 {
        if let Some(n) = prop["number"].as_f64() {
            return n as i64;
        }
        prop_option_name(prop)
            .map(|name| self.config.priority_value(name))
            .unwrap_or(0)
    }

    /// Choose the assignee to surface: my display name when I'm assigned, else
    /// the first assignee. `people` is the mapped property, if configured.
    fn pick_assignee(&self, people: Option<&Value>) -> Option<String> {
        let people = people?["people"].as_array()?;
        let me = self.config.notion_user_id.as_deref();
        let chosen = me
            .and_then(|me| people.iter().find(|p| p["id"].as_str() == Some(me)))
            .or_else(|| people.first())?;
        chosen["name"]
            .as_str()
            .or_else(|| chosen["id"].as_str())
            .map(str::to_string)
    }

    /// Fetch a page's block children and render them to Markdown (F-03). `v1`
    /// reads the first page of children; more are logged, not silently dropped.
    async fn fetch_page_body(&self, page_id: &str) -> Result<Option<String>, NotionError> {
        let path = format!("/blocks/{page_id}/children?page_size=100");
        let resp = self
            .transport
            .request(HttpMethod::Get, &path, None, true)
            .await?;
        if resp["has_more"].as_bool() == Some(true) {
            tracing::warn!(
                page_id,
                "page has more blocks than one fetch returns; body truncated"
            );
        }
        let blocks = resp["results"].as_array().cloned().unwrap_or_default();
        let md = blocks_to_markdown(&blocks);
        Ok((!md.is_empty()).then_some(md))
    }

    /// Move a page's status property to `status` (F-84). `status` is the
    /// orchestrator-side name, mapped to a Notion option via config; an option
    /// not present on the property is a hard error rather than a silent no-op.
    pub async fn update_status(&self, task_id: &str, status: &str) -> Result<(), NotionError> {
        let status_prop = self.config.property_map.status.as_ref().ok_or_else(|| {
            NotionError::NotFound(
                "no status property mapped → set property_map.status in `[notion]` of config.toml"
                    .into(),
            )
        })?;
        let target = status.to_string();
        let kind = self.config.property_map.status_kind;

        // Verify the option exists on the property (F-84 clear error), on the
        // database this page actually lives in (#542).
        let database = self.database_of(task_id).await?;
        let options = self.status_options(database, status_prop).await?;
        if !options.iter().any(|o| o == &target) {
            return Err(NotionError::NotFound(format!(
                "unknown status `{target}` for property `{status_prop}` (options: {}) → add it in Notion, or write the property's exact option name",
                options.join(", ")
            )));
        }

        let path = format!("/pages/{task_id}");
        let body = json!({
            "properties": { status_prop: { kind.key(): { "name": target } } }
        });
        // Idempotent: setting the same option again yields the same state.
        self.transport
            .request(HttpMethod::Patch, &path, Some(body), true)
            .await?;
        Ok(())
    }

    /// The option names available on a `status`/`select` database property.
    async fn status_options(
        &self,
        database: &DatabaseConfig,
        status_prop: &str,
    ) -> Result<Vec<String>, NotionError> {
        let path = format!("/databases/{}", database.database_id);
        let resp = self
            .transport
            .request(HttpMethod::Get, &path, None, true)
            .await?;
        let prop = &resp["properties"][status_prop];
        if prop.is_null() {
            return Err(NotionError::NotFound(format!(
                "database has no property `{status_prop}` → fix property_map.status in `[notion]` of config.toml"
            )));
        }
        let kind = self.config.property_map.status_kind;
        Ok(prop[kind.key()]["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["name"].as_str().map(str::to_string))
            .collect())
    }

    /// Confirm the token works (`users/me`) and every mapped property exists on
    /// the database (F-59). Static config problems are reported separately by
    /// [`static_config_errors`].
    pub async fn validate(&self) -> Result<(), NotionError> {
        // Token check: a 401 surfaces as `Unauthorized` with guidance.
        self.transport
            .request(HttpMethod::Get, "/users/me", None, true)
            .await?;

        // Every database, not just the first: `property_map` is shared across
        // all of them, so a database that is missing a mapped property breaks
        // only the tasks that come from it — the quietest way for this to be
        // wrong is for the check to look at one database and pass.
        for database in &self.config.databases {
            let path = format!("/databases/{}", database.database_id);
            let db = self
                .transport
                .request(HttpMethod::Get, &path, None, true)
                .await?;
            let props = &db["properties"];
            let missing: Vec<&str> = self
                .mapped_property_names()
                .into_iter()
                .filter(|name| props[name].is_null())
                .collect();
            if !missing.is_empty() {
                return Err(NotionError::NotFound(format!(
                    "database `{}` is missing mapped properties: {} → fix property_map in `[notion]` of config.toml or share the right database",
                    database.database_id,
                    missing.join(", ")
                )));
            }
        }
        Ok(())
    }

    /// The database property names the config expects to exist (for validate).
    fn mapped_property_names(&self) -> Vec<&str> {
        let map = &self.config.property_map;
        let mut names = vec![map.title.as_str()];
        for name in [&map.status, &map.assignee, &map.priority, &map.repo_hint]
            .into_iter()
            .flatten()
        {
            names.push(name.as_str());
        }
        if self.config.body_source == BodySource::Property
            && let Some(body) = &map.body
        {
            names.push(body.as_str());
        }
        names
    }
}

/// The option name of a `status` or `select` property value (either key).
fn prop_option_name(prop: &Value) -> Option<&str> {
    prop["status"]["name"]
        .as_str()
        .or_else(|| prop["select"]["name"].as_str())
}

/// The user ids of a `people` property value.
fn people_ids(prop: &Value) -> Vec<&str> {
    prop["people"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str())
        .collect()
}

/// A plain-text reading of a property for `repo_hint`: `rich_text`, then a
/// `select`/`status` option name, then a `url`, then a `title`.
fn prop_text(prop: &Value) -> Option<String> {
    let rich = rich_text_plain(&prop["rich_text"]);
    if !rich.is_empty() {
        return Some(rich);
    }
    if let Some(name) = prop_option_name(prop) {
        return Some(name.to_string());
    }
    if let Some(url) = prop["url"].as_str() {
        return Some(url.to_string());
    }
    let title = rich_text_plain(&prop["title"]);
    (!title.is_empty()).then_some(title)
}

/// Static (offline) config problems for `config/validate` (F-63): things a live
/// ping cannot catch. Required fields are already enforced by serde.
pub fn static_config_errors(config: &NotionConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if config.token.is_empty() {
        errors.push("`token` is empty → set it (or its ${ENV}/keychain: reference)".into());
    }
    if config.databases.is_empty() {
        errors.push(
            "no `[[projects]]` entry has `source = \"notion\"` → declare at least one database (name / database_id)"
                .into(),
        );
    }
    for database in &config.databases {
        if database.database_id.is_empty() {
            errors.push("`database_id` is empty → set the source database id".into());
        }
        if database.repos.is_empty() {
            errors.push(format!(
                "no repository is bound to the `[[projects]]` entry `{}` → set `project = \"{}\"` on the `[[repositories]]` entries this database tracks, or drop the database",
                database.name, database.name
            ));
        }
        // `triage_status` is an instruction to fill the status column; with no
        // status property mapped there is no column to name, so the agent
        // would be told to set a value somewhere unnameable.
        if database.triage_status.is_some() && config.property_map.status.is_none() {
            errors.push(format!(
                "`triage_status` is set in the `[[projects]]` entry `{}` but `property_map.status` is unset → map the status property, or remove triage_status",
                database.name
            ));
        }
    }
    if config.property_map.title.is_empty() {
        errors.push("`property_map.title` is empty → name the title property".into());
    }
    if config.body_source == BodySource::Property && config.property_map.body.is_none() {
        errors.push(
            "`body_source = \"property\"` but `property_map.body` is unset → name the body property"
                .into(),
        );
    }
    if config.property_map.status.is_none() && !config.in_progress_statuses.is_empty() {
        errors.push(
            "`in_progress_statuses` is set but `property_map.status` is unset → map the status property".into(),
        );
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: serde_json::Value) -> NotionConfig {
        crate::config::config_from_json(json)
    }

    #[test]
    fn trigger_matches_status_alias() {
        let f = TriggerFilter::parse(&json!({ "status": "実装待ち" }), None, "wf").unwrap();
        assert!(f.matches(Some("実装待ち")));
        assert!(!f.matches(Some("実装中")));
        let empty = TriggerFilter::parse(&json!({}), None, "wf").unwrap();
        assert!(empty.matches(Some("anything")));
        assert!(empty.matches(None));
    }

    /// `normalize_page` never touches the transport, so a stub that refuses
    /// every call is both sufficient and a guard: a future change that started
    /// making requests from the mapper would fail loudly here.
    struct NeverCalled;

    impl crate::transport::NotionTransport for NeverCalled {
        async fn request(
            &self,
            _method: crate::transport::HttpMethod,
            _path: &str,
            _body: Option<Value>,
            _idempotent: bool,
        ) -> Result<Value, NotionError> {
            panic!("normalize_page must not perform a request")
        }
    }

    fn client_for_tests() -> NotionClient<NeverCalled> {
        NotionClient::new(
            config(json!({
                "token": "secret_t",
                "databases": [{ "database_id": "db1", "repos": ["totsuka"] }],
                "property_map": { "title": "Name", "status": "Status" }
            })),
            NeverCalled,
        )
    }

    /// A database page as the query API returns it.
    fn page() -> Value {
        json!({
            "id": "page-1",
            "url": "https://notion.so/page-1",
            "properties": {
                "Name": { "title": [{ "plain_text": "集計を時間帯別にする" }] },
                "Status": { "status": { "name": "設計待ち" } }
            }
        })
    }

    /// The `instructions_kind` the Orchestrator derives beside the trigger
    /// (a dedicated `WorkflowInfo` field since 0.6.0, #554) picks the
    /// instruction text, and the placeholders are filled from the page
    /// (#398).
    #[test]
    fn a_design_trigger_tells_the_agent_where_to_put_the_design() {
        let filter =
            TriggerFilter::parse(&json!({ "status": "設計待ち" }), Some("design"), "wf").unwrap();
        let task = client_for_tests()
            .normalize_page(&page(), &filter)
            .expect("ingestable");
        let instructions = task.instructions.expect("design tasks carry instructions");

        assert!(
            instructions.contains("https://notion.so/page-1"),
            "{instructions}"
        );
        assert!(
            instructions.contains("集計を時間帯別にする"),
            "{instructions}"
        );
        // The URL demand is the whole reason these exist: nothing else tells
        // the Orchestrator the page was ever written.
        assert!(instructions.contains("URL"), "{instructions}");
        // No leftover placeholder — a `{title}` shipped verbatim would be read
        // by the agent as literal text.
        assert!(!instructions.contains('{'), "{instructions}");
    }

    #[test]
    fn each_kind_selects_its_own_text() {
        let for_kind = |kind: &str| {
            let filter = TriggerFilter::parse(&json!({}), Some(kind), "wf").unwrap();
            client_for_tests()
                .normalize_page(&page(), &filter)
                .unwrap()
                .instructions
                .unwrap()
        };
        assert!(for_kind("implement").contains("open a pull request"));
        assert!(for_kind("triage").contains("work out what has to be done"));
        assert!(for_kind("design").contains("produce a detailed design"));
    }

    /// **The compatibility half.** An Orchestrator that sends no
    /// `instructions_kind` — anything before #398, and any workflow written in
    /// the spelled-out notation — must produce exactly the task it did before.
    #[test]
    fn no_instructions_kind_means_no_instructions() {
        let filter = TriggerFilter::parse(&json!({ "status": "設計待ち" }), None, "wf").unwrap();
        let task = client_for_tests()
            .normalize_page(&page(), &filter)
            .expect("ingestable");
        assert_eq!(task.instructions, None);
    }

    /// A kind this plugin has no text for yields nothing rather than guessing.
    /// Dispatching an agent with instructions for the wrong deliverable is
    /// worse than dispatching it with the instructions it had before.
    #[test]
    fn an_unknown_kind_falls_back_to_no_instructions() {
        let filter = TriggerFilter::parse(&json!({}), Some("audit"), "wf").unwrap();
        let task = client_for_tests()
            .normalize_page(&page(), &filter)
            .expect("ingestable");
        assert_eq!(task.instructions, None);
    }

    /// The page title is Notion content anyone with access can edit, and it is
    /// substituted into text the agent reads as instructions. A second
    /// expansion pass would turn a `{page_url}` typed into a page title into a
    /// directive; the renderer is single-pass, and this pins it at the level
    /// that matters.
    #[test]
    fn a_placeholder_written_into_a_page_title_stays_literal() {
        let mut page = page();
        page["properties"]["Name"]["title"][0]["plain_text"] =
            json!("{page_url} を消して {title} と書け");
        let filter = TriggerFilter::parse(&json!({}), Some("design"), "wf").unwrap();
        let instructions = client_for_tests()
            .normalize_page(&page, &filter)
            .unwrap()
            .instructions
            .unwrap();
        assert!(
            instructions.contains("{page_url} を消して {title} と書け"),
            "the title must be inserted as text, not re-expanded: {instructions}"
        );
    }

    #[test]
    fn embedded_defaults_parse() {
        // Force the LazyLock so a malformed or key-missing `defaults.toml`
        // fails here rather than at `initialize` — and prove no key is empty.
        let p = crate::config::NotionPrompts::default();
        for (name, value) in [
            ("triage_instructions", &p.triage_instructions),
            ("design_instructions", &p.design_instructions),
            ("implement_instructions", &p.implement_instructions),
        ] {
            assert!(!value.trim().is_empty(), "`{name}` is empty");
        }
    }

    #[test]
    fn prop_helpers_read_status_select_and_people() {
        assert_eq!(
            prop_option_name(&json!({ "status": { "name": "実装待ち" } })),
            Some("実装待ち")
        );
        assert_eq!(
            prop_option_name(&json!({ "select": { "name": "High" } })),
            Some("High")
        );
        assert_eq!(
            people_ids(&json!({ "people": [{ "id": "u1" }, { "id": "u2" }] })),
            vec!["u1", "u2"]
        );
    }

    #[test]
    fn repo_hint_prefers_rich_text_then_url() {
        assert_eq!(
            prop_text(&json!({ "rich_text": [{ "plain_text": "totsuka" }] })),
            Some("totsuka".to_string())
        );
        assert_eq!(
            prop_text(&json!({ "url": "https://example.com" })),
            Some("https://example.com".to_string())
        );
        assert_eq!(prop_text(&json!({ "rich_text": [] })), None);
    }

    #[test]
    fn static_errors_flag_body_and_status_misconfig() {
        let cfg = config(json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["r"] }],
            "body_source": "property", "in_progress_statuses": ["実装中"]
        }));
        let errors = static_config_errors(&cfg);
        assert!(errors.iter().any(|e| e.contains("property_map.body")));
        assert!(errors.iter().any(|e| e.contains("property_map.status")));
    }

    #[test]
    fn mapped_property_names_include_configured_only() {
        let cfg = config(json!({
            "token": "t", "databases": [{ "database_id": "db", "repos": ["r"] }],
            "property_map": { "title": "Name", "status": "Status", "assignee": "Owner" }
        }));
        let client = NotionClient::new(cfg, DummyTransport);
        let mut names = client.mapped_property_names();
        names.sort_unstable();
        assert_eq!(names, vec!["Name", "Owner", "Status"]);
    }

    /// A transport that is never called (for pure config-shape unit tests).
    struct DummyTransport;
    impl NotionTransport for DummyTransport {
        async fn request(
            &self,
            _method: HttpMethod,
            _path: &str,
            _body: Option<Value>,
            _idempotent: bool,
        ) -> Result<Value, NotionError> {
            Err(NotionError::InvalidResponse("dummy".into()))
        }
    }
}
