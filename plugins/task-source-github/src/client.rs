//! GitHub task-source operations over a [`GithubTransport`]: fetch + normalize
//! (F-01/F-02), status write-back (F-84), result publish (F-07), and token
//! validation (F-59). All GraphQL is built as plain JSON bodies so no GraphQL
//! client dependency is needed (mirrors the LLM adapter in orchestrator-core).

use serde_json::{Value, json};

use plugin_protocol::Task;

use crate::config::GithubConfig;
use crate::error::GithubError;
use crate::transport::GithubTransport;

/// Content longer than this is folded into a `<details>` block on publish (F-07).
const FOLD_THRESHOLD: usize = 800;

/// Max project-item pages walked per fetch / status lookup. Bounds a poll on a
/// very large board (50–100 items per page); reaching it is logged, not silent.
const MAX_FETCH_PAGES: usize = 40;

/// A parsed trigger condition (workflow-defined shape, F-81), e.g.
/// `{"project_status": "実装待ち"}` and/or `{"label": "bug"}`.
#[derive(Debug, Default)]
struct TriggerFilter {
    project_status: Option<String>,
    label: Option<String>,
}

impl TriggerFilter {
    fn parse(trigger: &Value) -> Self {
        Self {
            project_status: trigger
                .get("project_status")
                .and_then(Value::as_str)
                .map(str::to_string),
            label: trigger
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    /// Whether a candidate task matches the trigger the workflow asked for.
    fn matches(&self, status: Option<&str>, labels: &[String]) -> bool {
        let status_ok = match &self.project_status {
            Some(want) => status == Some(want.as_str()),
            None => true,
        };
        let label_ok = match &self.label {
            Some(want) => labels.iter().any(|l| l == want),
            None => true,
        };
        status_ok && label_ok
    }
}

/// GitHub task-source client, generic over its transport for testability.
pub struct GithubClient<T> {
    config: GithubConfig,
    transport: T,
}

impl<T: GithubTransport> GithubClient<T> {
    /// A client using `config` and `transport`.
    pub fn new(config: GithubConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// The plugin settings.
    pub fn config(&self) -> &GithubConfig {
        &self.config
    }

    /// Fetch project issues matching `trigger`, normalize to [`Task`], and apply
    /// ingest gating (F-08): skip other people's tasks, in-progress statuses,
    /// and repositories outside the configured filter.
    pub async fn fetch(&self, trigger: &Value) -> Result<Vec<Task>, GithubError> {
        let filter = TriggerFilter::parse(trigger);
        let query = fetch_query(self.config.owner_type.graphql_root());
        let mut tasks = Vec::new();
        let mut cursor: Option<String> = None;

        // Filtering is client-side (ProjectsV2 has no server-side status filter),
        // so cap the walk to keep a poll bounded on very large boards; a hit is
        // logged rather than silently truncating.
        for page_num in 0..MAX_FETCH_PAGES {
            let body = json!({
                "query": query,
                "variables": {
                    "owner": self.config.owner,
                    "number": self.config.project_number,
                    "statusField": self.config.status_field,
                    "cursor": cursor,
                },
            });
            let resp = self.transport.post_graphql(body, true).await?;
            let project = self.project_node(&resp)?;
            let items = &project["items"];

            for node in items["nodes"].as_array().into_iter().flatten() {
                if let Some(task) = self.normalize_item(node, &filter) {
                    tasks.push(task);
                }
            }

            let page = &items["pageInfo"];
            if page["hasNextPage"].as_bool() != Some(true) {
                return Ok(tasks);
            }
            cursor = page["endCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(tasks); // defensive: hasNextPage without a cursor
            }
            if page_num + 1 == MAX_FETCH_PAGES {
                tracing::warn!(
                    pages = MAX_FETCH_PAGES,
                    "reached the fetch page cap; some project items were not scanned this poll"
                );
            }
        }
        Ok(tasks)
    }

    /// Normalize one project item to a [`Task`], or `None` if it is not an
    /// ingestable Issue (non-Issue content, or filtered out by trigger/gating).
    fn normalize_item(&self, node: &Value, filter: &TriggerFilter) -> Option<Task> {
        let content = &node["content"];
        if content["__typename"].as_str() != Some("Issue") {
            return None; // draft items and PRs are not tasks
        }
        let status = node["status"]["name"].as_str();
        let repo = content["repository"]["name"].as_str().unwrap_or_default();
        let labels: Vec<String> = content["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_string))
            .collect();
        // Consider *all* assignees, not just the first: whether I am assigned
        // must not depend on GitHub's node ordering (F-08).
        let assignees: Vec<&str> = content["assignees"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["login"].as_str())
            .collect();

        // Workflow trigger first, then multi-user ingest gating (F-08).
        if !filter.matches(status, &labels) {
            return None;
        }
        if !self.config.repo_allowed(repo)
            || !self.config.assignable_to_me(&assignees)
            || status.is_some_and(|s| self.config.is_in_progress(s))
        {
            return None;
        }

        let id = content["id"].as_str()?.to_string();
        let body = content["body"].as_str().filter(|b| !b.is_empty());
        // Surface my own login when I'm an assignee, else the first assignee.
        let assignee = assignees
            .iter()
            .find(|l| l.eq_ignore_ascii_case(&self.config.github_login))
            .or(assignees.first())
            .map(|l| l.to_string());
        Some(Task {
            id,
            source: self.config.source_name.clone(),
            title: content["title"].as_str().unwrap_or_default().to_string(),
            body: body.map(str::to_string),
            repo_hint: (!repo.is_empty()).then(|| repo.to_string()),
            labels,
            priority: 0,
            status: status.map(str::to_string),
            url: content["url"].as_str().map(str::to_string),
            assignee,
            message_key: None,
            instructions: None,
        })
    }

    /// Move a task's project status column to `status` (F-84). `status` is the
    /// orchestrator-side name; it is mapped to a project option via config, and
    /// an unknown option is a hard error rather than a silent no-op.
    pub async fn update_status(&self, task_id: &str, status: &str) -> Result<(), GithubError> {
        let target = self.config.map_status(status).to_string();
        let query = resolve_query(self.config.owner_type.graphql_root());

        // Resolve the project id + status option once, then page the item list
        // until the issue's item is found (a busy board can hold >1 page).
        let mut project_id = String::new();
        let mut field_id = String::new();
        let mut option_id = String::new();
        let mut item_id: Option<String> = None;
        let mut cursor: Option<String> = None;

        for page_num in 0..MAX_FETCH_PAGES {
            let resolve = json!({
                "query": query,
                "variables": {
                    "owner": self.config.owner,
                    "number": self.config.project_number,
                    "statusField": self.config.status_field,
                    "cursor": cursor,
                },
            });
            let resp = self.transport.post_graphql(resolve, true).await?;
            let project = self.project_node(&resp)?;

            if page_num == 0 {
                project_id = project["id"]
                    .as_str()
                    .ok_or_else(|| GithubError::InvalidResponse("project id missing".into()))?
                    .to_string();
                let field = &project["field"];
                field_id = field["id"]
                    .as_str()
                    .ok_or_else(|| GithubError::InvalidResponse("status field missing".into()))?
                    .to_string();
                option_id = field["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|o| o["name"].as_str() == Some(target.as_str()))
                    .and_then(|o| o["id"].as_str())
                    .ok_or_else(|| {
                        GithubError::NotFound(format!(
                            "unknown status `{target}` for field `{}` → add the option in the project or fix status_map in plugins/github.toml",
                            self.config.status_field
                        ))
                    })?
                    .to_string();
            }

            let items = &project["items"];
            if let Some(found) = items["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|n| n["content"]["id"].as_str() == Some(task_id))
                .and_then(|n| n["id"].as_str())
            {
                item_id = Some(found.to_string());
                break;
            }
            let page = &items["pageInfo"];
            if page["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            cursor = page["endCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }

        let item_id = item_id.ok_or_else(|| {
            GithubError::NotFound(format!(
                "issue `{task_id}` is not an item of project #{}",
                self.config.project_number
            ))
        })?;

        let mutation = json!({
            "query": UPDATE_STATUS_MUTATION,
            "variables": {
                "project": project_id, "item": item_id,
                "field": field_id, "option": option_id,
            },
        });
        // Idempotent: setting the same option again yields the same state.
        let resp = self.transport.post_graphql(mutation, true).await?;
        check_errors(&resp)?;
        Ok(())
    }

    /// Publish `content` as an Issue comment (F-07), folding long bodies into a
    /// `<details>` block. `task_id` is the Issue's node id (the comment subject).
    pub async fn publish(
        &self,
        task_id: &str,
        content: &str,
        _format: Option<&str>,
    ) -> Result<(), GithubError> {
        let body = fold_long_content(content);
        let mutation = json!({
            "query": ADD_COMMENT_MUTATION,
            "variables": { "subject": task_id, "body": body },
        });
        // Non-idempotent: a retried addComment would post a duplicate comment.
        let resp = self.transport.post_graphql(mutation, false).await?;
        check_errors(&resp)?;
        Ok(())
    }

    /// Confirm the token works by reading `viewer.login` (F-59). Static config
    /// problems are reported separately by [`static_config_errors`].
    pub async fn validate(&self) -> Result<(), GithubError> {
        let resp = self
            .transport
            .post_graphql(json!({ "query": VIEWER_QUERY }), true)
            .await?;
        let data = check_errors(&resp)?;
        if data["viewer"]["login"].as_str().is_some() {
            Ok(())
        } else {
            Err(GithubError::InvalidResponse(
                "viewer query returned no login".into(),
            ))
        }
    }

    /// Extract `data.<owner-root>.projectV2`, surfacing GraphQL errors and a
    /// missing project (e.g. wrong owner/number) as actionable failures.
    fn project_node<'a>(&self, resp: &'a Value) -> Result<&'a Value, GithubError> {
        let data = check_errors(resp)?;
        let root = self.config.owner_type.graphql_root();
        let project = &data[root]["projectV2"];
        if project.is_null() {
            return Err(GithubError::NotFound(format!(
                "project #{} not found for {} `{}` → check owner/owner_type/project_number in plugins/github.toml",
                self.config.project_number, root, self.config.owner
            )));
        }
        Ok(project)
    }
}

/// Return the `data` object, or a [`GithubError::GraphQl`] if the response
/// carried a non-empty top-level `errors` array.
fn check_errors(resp: &Value) -> Result<&Value, GithubError> {
    if let Some(errors) = resp.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let joined = errors
            .iter()
            .filter_map(|e| e["message"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(GithubError::GraphQl(joined));
    }
    Ok(&resp["data"])
}

/// Fold `content` into a collapsed `<details>` block when it is long (F-07).
fn fold_long_content(content: &str) -> String {
    if content.len() <= FOLD_THRESHOLD {
        content.to_string()
    } else {
        format!("<details>\n<summary>totsuka の生成結果を表示</summary>\n\n{content}\n\n</details>")
    }
}

/// Static (offline) config problems for `config/validate` (F-63): things a
/// viewer ping cannot catch. Required fields are already enforced by serde.
pub fn static_config_errors(config: &GithubConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if config.token.is_empty() {
        errors.push("`token` is empty → set it (or its ${ENV}/keychain: reference)".into());
    }
    if config.owner.is_empty() {
        errors.push("`owner` is empty → set the project owner login".into());
    }
    if config.github_login.is_empty() {
        errors.push(
            "`github_login` is empty → set your GitHub login for ingest gating (F-08)".into(),
        );
    }
    if config.project_number <= 0 {
        errors.push(format!(
            "`project_number` must be positive, got {} → use the ProjectsV2 number",
            config.project_number
        ));
    }
    errors
}

/// The paginated project-items query (F-02). `root` is `user` or `organization`
/// — a fixed, non-user-controlled keyword, so string interpolation is safe.
fn fetch_query(root: &str) -> String {
    format!(
        r#"query($owner: String!, $number: Int!, $statusField: String!, $cursor: String) {{
  {root}(login: $owner) {{
    projectV2(number: $number) {{
      items(first: 50, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          status: fieldValueByName(name: $statusField) {{
            ... on ProjectV2ItemFieldSingleSelectValue {{ name }}
          }}
          content {{
            __typename
            ... on Issue {{
              id number title body url
              repository {{ name }}
              assignees(first: 10) {{ nodes {{ login }} }}
              labels(first: 100) {{ nodes {{ name }} }}
            }}
          }}
        }}
      }}
    }}
  }}
}}"#
    )
}

/// Resolves the project id, status field + options, and (paginated) item ids
/// for `update_status`. `root` is `user`/`organization` (see [`fetch_query`]).
fn resolve_query(root: &str) -> String {
    format!(
        r#"query($owner: String!, $number: Int!, $statusField: String!, $cursor: String) {{
  {root}(login: $owner) {{
    projectV2(number: $number) {{
      id
      field(name: $statusField) {{
        ... on ProjectV2SingleSelectField {{ id options {{ id name }} }}
      }}
      items(first: 100, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{ id content {{ ... on Issue {{ id }} }} }}
      }}
    }}
  }}
}}"#
    )
}

const UPDATE_STATUS_MUTATION: &str = r#"mutation($project: ID!, $item: ID!, $field: ID!, $option: String!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $project, itemId: $item, fieldId: $field,
    value: { singleSelectOptionId: $option }
  }) { projectV2Item { id } }
}"#;

const ADD_COMMENT_MUTATION: &str = r#"mutation($subject: ID!, $body: String!) {
  addComment(input: { subjectId: $subject, body: $body }) {
    commentEdge { node { url } }
  }
}"#;

const VIEWER_QUERY: &str = "query { viewer { login } }";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_filter_matches_status_and_label() {
        let f = TriggerFilter::parse(&json!({ "project_status": "実装待ち", "label": "bug" }));
        assert!(f.matches(Some("実装待ち"), &["bug".into()]));
        assert!(!f.matches(Some("実装中"), &["bug".into()])); // wrong status
        assert!(!f.matches(Some("実装待ち"), &["docs".into()])); // missing label
    }

    #[test]
    fn empty_trigger_matches_everything() {
        let f = TriggerFilter::parse(&json!({}));
        assert!(f.matches(Some("anything"), &[]));
        assert!(f.matches(None, &["x".into()]));
    }

    #[test]
    fn short_content_is_not_folded() {
        assert_eq!(fold_long_content("hi"), "hi");
    }

    #[test]
    fn long_content_is_folded_in_details() {
        let long = "x".repeat(FOLD_THRESHOLD + 1);
        let folded = fold_long_content(&long);
        assert!(folded.starts_with("<details>"));
        assert!(folded.contains("</details>"));
        assert!(folded.contains(&long));
    }

    #[test]
    fn static_errors_flag_bad_project_number() {
        let cfg: GithubConfig = serde_json::from_value(json!({
            "token": "t", "owner": "me", "project_number": 0, "github_login": "me"
        }))
        .unwrap();
        let errors = static_config_errors(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("project_number")),
            "got {errors:?}"
        );
    }
}
