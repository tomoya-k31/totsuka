//! Anthropic Messages API classifier. tool_use is forced via tool_choice so
//! the response is always structured JSON matching ClassifyResponse.top_candidates.

use super::{
    prompt::build_prompt, schema::{ClassifyRequest, ClassifyResponse, RepoVerdict}, Classifier,
};
use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use totsuka_core::Secret;

pub struct AnthropicClassifier {
    client: Client,
    endpoint: String,
    api_key: Secret<String>,
    model: String,
    max_tokens: u32,
    top_n: u32,
    request_timeout: Duration,
}

impl AnthropicClassifier {
    pub fn new(
        api_key: Secret<String>,
        model: String,
        max_tokens: u32,
        top_n: u32,
        request_timeout: Duration,
        override_endpoint: Option<String>,
    ) -> Self {
        let endpoint = override_endpoint
            .unwrap_or_else(|| "https://api.anthropic.com/v1/messages".into());
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint,
            api_key,
            model,
            max_tokens,
            top_n,
            request_timeout,
        }
    }

    fn tool_schema(top_n: u32) -> Value {
        json!({
            "name": "classify_repo",
            "description": "Return the most-likely candidate repositories for the question.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "top_candidates": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": top_n,
                        "items": {
                            "type": "object",
                            "properties": {
                                "repo":       { "type": "string" },
                                "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                                "rationale":  { "type": "string" }
                            },
                            "required": ["repo", "confidence"]
                        }
                    }
                },
                "required": ["top_candidates"]
            }
        })
    }
}

#[async_trait]
impl Classifier for AnthropicClassifier {
    fn provider(&self) -> &str { "anthropic" }
    fn model(&self) -> &str { &self.model }

    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        let (system, user) = build_prompt(&req, self.top_n);
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": [ { "role": "user", "content": user } ],
            "tools": [ Self::tool_schema(self.top_n) ],
            "tool_choice": { "type": "tool", "name": "classify_repo" }
        });
        let start = Instant::now();
        let resp = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .timeout(self.request_timeout)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(QaError::Classifier(format!("anthropic {status}: {v}")));
        }
        let content = v["content"].as_array().ok_or_else(|| {
            QaError::Classifier(format!("anthropic: missing content array: {v}"))
        })?;
        let tool_input = content
            .iter()
            .find(|c| c["type"] == "tool_use")
            .and_then(|c| c.get("input"))
            .cloned()
            .ok_or_else(|| QaError::Classifier(format!("anthropic: no tool_use block: {v}")))?;
        let verdicts: Vec<RepoVerdict> = serde_json::from_value(tool_input["top_candidates"].clone())
            .map_err(|e| QaError::Classifier(format!("anthropic tool_use parse: {e}")))?;
        Ok(ClassifyResponse {
            top_candidates: verdicts,
            provider: self.provider().into(),
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}
