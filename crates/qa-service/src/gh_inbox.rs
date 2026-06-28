//! Create a DraftIssue in the GitHub Project Inbox column.
//!
//! GraphQL injection prevention: project_node_id, title, body MUST go through
//! `variables`, never `format!`-interpolated into the document — same shape
//! as orchestrator's gh_writeback after PR #4 and github-watcher's
//! gh_client/graphql.rs.

use crate::error::QaError;
use reqwest::Client;
use serde_json::{json, Value};
use totsuka_core::Secret;

const MUTATION: &str = r#"
    mutation($input: AddProjectV2DraftIssueInput!) {
      addProjectV2DraftIssue(input: $input) {
        projectItem { id }
      }
    }
"#;

pub struct GhInboxClient {
    client: Client,
    endpoint: String,
    token: Secret<String>,
}

impl GhInboxClient {
    pub fn new(token: Secret<String>, override_endpoint: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint: override_endpoint.unwrap_or_else(|| "https://api.github.com/graphql".into()),
            token,
        }
    }

    pub async fn create_draft(
        &self,
        project_node_id: &str,
        title: &str,
        body: &str,
    ) -> Result<String, QaError> {
        let req_body = json!({
            "query": MUTATION,
            "variables": {
                "input": {
                    "projectId": project_node_id,
                    "title":     title,
                    "body":      body,
                }
            }
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.token.expose())
            .json(&req_body)
            .send()
            .await?;
        let v: Value = resp.json().await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(QaError::GraphQl(errors[0].to_string()));
            }
        }
        v.pointer("/data/addProjectV2DraftIssue/projectItem/id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                QaError::GraphQl(format!("addProjectV2DraftIssue: missing item id: {v}"))
            })
    }
}
