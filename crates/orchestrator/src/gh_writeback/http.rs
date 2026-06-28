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

        // Use a parameterised GraphQL document with variables. The previous
        // implementation interpolated `item_id` (and friends) directly into
        // the query string, which would allow a malicious value to break out
        // of the string literal and inject arbitrary GraphQL — even though
        // these values currently originate inside the system, the
        // server-parsed-as-data shape is the only safe pattern.
        const MUTATION: &str = r#"
            mutation($input: UpdateProjectV2ItemFieldValueInput!) {
              updateProjectV2ItemFieldValue(input: $input) {
                clientMutationId
              }
            }
        "#;
        let body = serde_json::json!({
            "query": MUTATION,
            "variables": {
                "input": {
                    "projectId": self.project_id,
                    "itemId":    item_id,
                    "fieldId":   self.status_field_id,
                    "value":     { "singleSelectOptionId": option_id },
                }
            }
        });

        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.token.expose())
            .json(&body)
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

    /// Regression test for the GraphQL injection fix: a malicious item_id
    /// that contains a `"` MUST NOT close the literal in the GraphQL document.
    /// With the variables-based pattern the value lands in `variables.input.itemId`
    /// where the server parses it as data, so the `"` survives intact and the
    /// `query` field never contains the user value at all.
    #[tokio::test]
    async fn malicious_item_id_lands_in_variables_not_query() {
        // Spin up a tiny HTTP server that captures the request body, replies
        // with a benign empty GraphQL response, then verify the body shape.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read the HTTP request, find the JSON body.
            let mut buf = Vec::new();
            let mut reader = BufReader::new(&mut stream);
            // Read headers until blank line, recording Content-Length.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).await.unwrap();
                if n == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line
                    .strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
                {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            // Read the body.
            buf.resize(content_length, 0);
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf)
                .await
                .unwrap();
            // Reply with an empty GraphQL response so the client returns Ok.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"data\":{}}")
                .await
                .unwrap();
            buf
        });

        let mut wb = GraphqlWriteback::new(
            Secret::new("tok".into()),
            "PVT_x".into(),
            "FIELD_x".into(),
            HashMap::from([("ready".into(), "OPT_x".into())]),
        );
        wb.endpoint = format!("http://{addr}/graphql");

        // An item_id that would have broken out of the old string-formatted
        // query: a literal `"` plus a GraphQL field name.
        let evil = r#""}}}) { __typename } mutation Pwn { __typename "#;
        let _ = wb.move_column(evil, "ready", None).await.unwrap();

        let raw = server.await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

        // The query string MUST NOT contain the user-controlled bytes anywhere.
        let q = body["query"].as_str().expect("query field present");
        assert!(
            !q.contains("__typename"),
            "query string was contaminated by item_id: {q}"
        );
        assert!(
            !q.contains(evil),
            "query string echoed evil item_id verbatim"
        );

        // The actual item_id survives unchanged inside variables.input.itemId.
        assert_eq!(body["variables"]["input"]["itemId"], evil);
        assert_eq!(body["variables"]["input"]["projectId"], "PVT_x");
        assert_eq!(body["variables"]["input"]["fieldId"], "FIELD_x");
        assert_eq!(
            body["variables"]["input"]["value"]["singleSelectOptionId"],
            "OPT_x"
        );
    }
}
