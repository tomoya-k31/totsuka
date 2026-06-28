use super::graphql::{PROJECT_ITEMS_QUERY, PROJECT_NODE_QUERY_ORG, PROJECT_NODE_QUERY_USER};
use super::{
    GhClient, IssueUpdate, ProjectItem, ProjectItemPage, PrUpdate, ReleaseUpdate, RepoSlug,
};
use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use totsuka_core::Secret;

pub struct HttpGhClient {
    client: Client,
    token: Secret<String>,
    endpoint_graphql: String,
    endpoint_rest: String,
}

impl HttpGhClient {
    pub fn new(token: Secret<String>) -> Self {
        Self::with_endpoints(
            token,
            "https://api.github.com/graphql".into(),
            "https://api.github.com".into(),
        )
    }

    pub fn with_endpoints(token: Secret<String>, graphql: String, rest: String) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-github-watcher")
                .build()
                .expect("reqwest client"),
            token,
            endpoint_graphql: graphql,
            endpoint_rest: rest,
        }
    }

    async fn graphql(&self, query: &'static str, variables: Value) -> Result<Value, WatcherError> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .client
            .post(&self.endpoint_graphql)
            .bearer_auth(self.token.expose())
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(WatcherError::GraphQl(errors[0].to_string()));
            }
        }
        if !status.is_success() {
            return Err(WatcherError::GraphQl(format!("status={status} body={v}")));
        }
        Ok(v)
    }
}

#[derive(Deserialize)]
struct PageInfoPart {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[async_trait]
impl GhClient for HttpGhClient {
    async fn resolve_project_node_id(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<String, WatcherError> {
        let vars = json!({ "login": owner, "number": number });
        // Try user first.
        let v = self.graphql(PROJECT_NODE_QUERY_USER, vars.clone()).await?;
        if let Some(id) = v
            .pointer("/data/user/projectV2/id")
            .and_then(|x| x.as_str())
        {
            return Ok(id.into());
        }
        // Fall back to organization.
        let v = self.graphql(PROJECT_NODE_QUERY_ORG, vars).await?;
        if let Some(id) = v
            .pointer("/data/organization/projectV2/id")
            .and_then(|x| x.as_str())
        {
            return Ok(id.into());
        }
        Err(WatcherError::GraphQl(format!(
            "no ProjectV2 for {owner}/#{number} under user or organization"
        )))
    }

    async fn project_items_page(
        &self,
        project_node_id: &str,
        after: Option<&str>,
        first: u32,
    ) -> Result<ProjectItemPage, WatcherError> {
        let vars = json!({
            "projectId": project_node_id,
            "first": first,
            "after": after,
        });
        let v = self.graphql(PROJECT_ITEMS_QUERY, vars).await?;
        let items_node = v
            .pointer("/data/node/items")
            .ok_or_else(|| WatcherError::GraphQl("missing data.node.items".into()))?;
        let pi: PageInfoPart = serde_json::from_value(
            items_node.get("pageInfo").cloned().unwrap_or(Value::Null),
        )
        .map_err(|e| WatcherError::GraphQl(format!("pageInfo: {e}")))?;
        let nodes = items_node
            .get("nodes")
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default();
        let mut items = Vec::with_capacity(nodes.len());
        for n in nodes {
            let id = n
                .get("id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| WatcherError::GraphQl("item missing id".into()))?
                .to_string();
            let status_display = n
                .pointer("/fieldValueByName/name")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let content = n.get("content").cloned().unwrap_or(Value::Null);
            let repo = content
                .pointer("/repository/nameWithOwner")
                .and_then(|x| x.as_str())
                .and_then(RepoSlug::parse);
            let content_number = content.get("number").and_then(|x| x.as_u64());
            let closed_at = content
                .get("closedAt")
                .and_then(|x| x.as_str())
                .and_then(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|d| d.with_timezone(&Utc))
                });
            items.push(ProjectItem {
                id,
                status_display,
                repo,
                content_number,
                closed_at,
            });
        }
        Ok(ProjectItemPage {
            items,
            end_cursor: pi.end_cursor,
            has_next: pi.has_next_page,
        })
    }

    // The remaining methods are filled in by Tasks 9 / 10.
    async fn issues_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<IssueUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::issues_since not yet implemented (Task 9)".into(),
        ))
    }

    async fn prs_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<PrUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::prs_since not yet implemented (Task 10)".into(),
        ))
    }

    async fn releases_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<ReleaseUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::releases_since not yet implemented (Task 10)".into(),
        ))
    }
}
