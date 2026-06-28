use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use totsuka_core::Secret;

use super::{WritebackClient, WritebackResult};
use crate::error::OrchestratorError;

pub struct GraphqlWriteback {
    client: Client,
    token: Secret<String>,
    project_id: String,
    status_field_id: String,
    option_ids: HashMap<String, String>,
    endpoint: String,
}

impl GraphqlWriteback {
    pub fn new(
        token: Secret<String>,
        project_id: String,
        status_field_id: String,
        option_ids: HashMap<String, String>,
    ) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-orchestrator")
                .build()
                .unwrap(),
            token,
            project_id,
            status_field_id,
            option_ids,
            endpoint: "https://api.github.com/graphql".into(),
        }
    }
}

#[derive(Deserialize)]
struct GqlResp {
    #[serde(default)]
    errors: Vec<GqlErr>,
    #[allow(dead_code)]
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GqlErr {
    message: String,
    #[serde(default, rename = "type")]
    r#type: Option<String>,
}

#[async_trait]
impl WritebackClient for GraphqlWriteback {
    async fn move_column(
        &self,
        item_id: &str,
        to_column: &str,
        _version: Option<String>,
    ) -> Result<WritebackResult, OrchestratorError> {
        let option_id = self.option_ids.get(to_column).ok_or_else(|| {
            OrchestratorError::Writeback(format!("no option_id for column {to_column}"))
        })?;

        let query = format!(
            r#"
            mutation {{
              updateProjectV2ItemFieldValue(input: {{
                projectId: "{project}",
                itemId: "{item}",
                fieldId: "{field}",
                value: {{ singleSelectOptionId: "{opt}" }}
              }}) {{ clientMutationId }}
            }}
        "#,
            project = self.project_id,
            item = item_id,
            field = self.status_field_id,
            opt = option_id
        );

        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({"query": query}))
            .send()
            .await
            .map_err(|e| OrchestratorError::Writeback(format!("send: {e}")))?;

        let body: GqlResp = resp
            .json()
            .await
            .map_err(|e| OrchestratorError::Writeback(format!("decode: {e}")))?;

        if let Some(err) = body.errors.first() {
            if err.message.to_lowercase().contains("stale")
                || err.r#type.as_deref() == Some("CONFLICT")
            {
                return Ok(WritebackResult::VersionMismatch);
            }
            return Ok(WritebackResult::Failed(err.message.clone()));
        }

        Ok(WritebackResult::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_core::Secret;

    #[test]
    fn constructor_smoke() {
        let _ = GraphqlWriteback::new(
            Secret::new("tok".into()),
            "PVT_x".into(),
            "FIELD_x".into(),
            HashMap::from([("ready".into(), "OPT_x".into())]),
        );
    }
}
