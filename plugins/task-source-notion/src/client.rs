//! Notion task-source operations over a [`NotionTransport`]: fetch + normalize
//! via the property map (F-01/F-03), status write-back (F-84), result publish
//! (F-07), and token/schema validation (F-59). All bodies are plain JSON built
//! with `serde_json` — no Notion SDK dependency.

use serde_json::{Value, json};

use plugin_protocol::Task;

use crate::blocks::{blocks_to_markdown, markdown_to_blocks, rich_text_plain};
use crate::config::{BodySource, NotionConfig};
use crate::error::NotionError;
use crate::transport::{HttpMethod, NotionTransport};

/// Pages requested per database query. Notion's max page size is 100.
const QUERY_PAGE_SIZE: usize = 100;

/// Max query pages walked per fetch. Bounds a poll on a very large database;
/// reaching it is logged, not silently truncated.
const MAX_FETCH_PAGES: usize = 40;

/// Blocks appended per `result/publish` request (Notion's per-request cap).
const APPEND_BATCH: usize = 100;

/// A parsed trigger condition (workflow-defined shape, F-81). Supports a status
/// value (`{"status": "実装待ち"}`, also accepted as `project_status`) and/or a
/// raw Notion `filter` object passed straight to the query API.
#[derive(Debug, Default)]
struct TriggerFilter {
    status: Option<String>,
    raw: Option<Value>,
}

impl TriggerFilter {
    fn parse(trigger: &Value) -> Self {
        let status = trigger
            .get("status")
            .or_else(|| trigger.get("project_status"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Self {
            status,
            raw: trigger.get("filter").cloned(),
        }
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
}

impl<T: NotionTransport> NotionClient<T> {
    /// A client using `config` and `transport`.
    pub fn new(config: NotionConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// The plugin settings.
    pub fn config(&self) -> &NotionConfig {
        &self.config
    }

    /// Fetch database pages matching `trigger`, normalize to [`Task`] via the
    /// property map, and apply ingest gating (F-08): skip other people's tasks
    /// and in-progress statuses. When body comes from the page, its blocks are
    /// fetched only for surviving tasks.
    pub async fn fetch(&self, trigger: &Value) -> Result<Vec<Task>, NotionError> {
        let filter = TriggerFilter::parse(trigger);
        let server_filter = self.build_filter(&filter);
        let mut tasks = Vec::new();
        let mut cursor: Option<String> = None;

        for page_num in 0..MAX_FETCH_PAGES {
            let mut body = json!({ "page_size": QUERY_PAGE_SIZE });
            if let Some(f) = &server_filter {
                body["filter"] = f.clone();
            }
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let path = format!("/databases/{}/query", self.config.database_id);
            let resp = self
                .transport
                .request(HttpMethod::Post, &path, Some(body), true)
                .await?;

            for page in resp["results"].as_array().into_iter().flatten() {
                if let Some(mut task) = self.normalize_page(page, &filter) {
                    if self.config.body_source == BodySource::Page {
                        task.body = self.fetch_page_body(&task.id).await?;
                    }
                    tasks.push(task);
                }
            }

            if resp["has_more"].as_bool() != Some(true) {
                return Ok(tasks);
            }
            cursor = resp["next_cursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(tasks); // defensive: has_more without a cursor
            }
            if page_num + 1 == MAX_FETCH_PAGES {
                tracing::warn!(
                    pages = MAX_FETCH_PAGES,
                    "reached the fetch page cap; some database pages were not scanned this poll"
                );
            }
        }
        Ok(tasks)
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
        let assignee_ids: Vec<&str> = map
            .assignee
            .as_deref()
            .map(|name| people_ids(&props[name]))
            .unwrap_or_default();
        if !self.config.assignable_to_me(&assignee_ids) {
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
            thread_key: None,
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
                "no status property mapped → set property_map.status in plugins/notion.toml".into(),
            )
        })?;
        let target = self.config.map_status(status).to_string();
        let kind = self.config.property_map.status_kind;

        // Verify the option exists on the property (F-84 clear error).
        let options = self.status_options(status_prop).await?;
        if !options.iter().any(|o| o == &target) {
            return Err(NotionError::NotFound(format!(
                "unknown status `{target}` for property `{status_prop}` (options: {}) → add it in Notion or fix status_map in plugins/notion.toml",
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
    async fn status_options(&self, status_prop: &str) -> Result<Vec<String>, NotionError> {
        let path = format!("/databases/{}", self.config.database_id);
        let resp = self
            .transport
            .request(HttpMethod::Get, &path, None, true)
            .await?;
        let prop = &resp["properties"][status_prop];
        if prop.is_null() {
            return Err(NotionError::NotFound(format!(
                "database has no property `{status_prop}` → fix property_map.status in plugins/notion.toml"
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

    /// Publish `content` by appending it as blocks to the task's page (F-07).
    /// Markdown is converted to Notion blocks, split to the 2000-char limit, and
    /// appended in batches of [`APPEND_BATCH`].
    ///
    /// `_format` is accepted for protocol symmetry but ignored: `v1` always
    /// parses `content` as Markdown (the only format the orchestrator emits).
    pub async fn publish(
        &self,
        task_id: &str,
        content: &str,
        _format: Option<&str>,
    ) -> Result<(), NotionError> {
        let mut blocks = markdown_to_blocks(content);
        if blocks.is_empty() {
            return Ok(()); // nothing to publish (all-blank content)
        }
        let path = format!("/blocks/{task_id}/children");
        // Chunk to Notion's per-request block cap.
        while !blocks.is_empty() {
            let rest = blocks.split_off(blocks.len().min(APPEND_BATCH));
            let body = json!({ "children": blocks });
            // Non-idempotent: a retried append would post duplicate blocks.
            self.transport
                .request(HttpMethod::Patch, &path, Some(body), false)
                .await?;
            blocks = rest;
        }
        Ok(())
    }

    /// Confirm the token works (`users/me`) and every mapped property exists on
    /// the database (F-59). Static config problems are reported separately by
    /// [`static_config_errors`].
    pub async fn validate(&self) -> Result<(), NotionError> {
        // Token check: a 401 surfaces as `Unauthorized` with guidance.
        self.transport
            .request(HttpMethod::Get, "/users/me", None, true)
            .await?;

        let path = format!("/databases/{}", self.config.database_id);
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
                "database is missing mapped properties: {} → fix property_map in plugins/notion.toml or share the right database",
                missing.join(", ")
            )));
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
    if config.database_id.is_empty() {
        errors.push("`database_id` is empty → set the source database id".into());
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
    if config.property_map.status.is_none()
        && (!config.status_map.is_empty() || !config.in_progress_statuses.is_empty())
    {
        errors.push(
            "`status_map`/`in_progress_statuses` set but `property_map.status` is unset → map the status property".into(),
        );
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: serde_json::Value) -> NotionConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn trigger_matches_status_alias() {
        let f = TriggerFilter::parse(&json!({ "project_status": "実装待ち" }));
        assert!(f.matches(Some("実装待ち")));
        assert!(!f.matches(Some("実装中")));
        let empty = TriggerFilter::parse(&json!({}));
        assert!(empty.matches(Some("anything")));
        assert!(empty.matches(None));
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
            "token": "t", "database_id": "db", "body_source": "property",
            "in_progress_statuses": ["実装中"]
        }));
        let errors = static_config_errors(&cfg);
        assert!(errors.iter().any(|e| e.contains("property_map.body")));
        assert!(errors.iter().any(|e| e.contains("property_map.status")));
    }

    #[test]
    fn mapped_property_names_include_configured_only() {
        let cfg = config(json!({
            "token": "t", "database_id": "db",
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
