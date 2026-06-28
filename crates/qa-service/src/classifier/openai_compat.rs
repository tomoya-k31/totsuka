//! OpenAI-style Chat Completions classifier — shared across openai /
//! openrouter / litellm / openai_compatible. response_format json_schema
//! forces structured output.

use super::{
    prompt::build_prompt,
    schema::{ClassifyRequest, ClassifyResponse, RepoVerdict},
    Classifier,
};
use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use totsuka_core::Secret;

pub struct OpenAiCompatClassifier {
    client: Client,
    endpoint: String,
    api_key: Secret<String>,
    model: String,
    max_tokens: u32,
    top_n: u32,
    request_timeout: Duration,
    provider_name: String,
}

impl OpenAiCompatClassifier {
    pub fn new(
        provider_name: String,
        endpoint: String,
        api_key: Secret<String>,
        model: String,
        max_tokens: u32,
        top_n: u32,
        request_timeout: Duration,
    ) -> Self {
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
            provider_name,
        }
    }

    fn response_schema(top_n: u32) -> Value {
        json!({
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
                            "confidence": { "type": "number" },
                            "rationale":  { "type": "string" }
                        },
                        "required": ["repo", "confidence", "rationale"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["top_candidates"],
            "additionalProperties": false
        })
    }
}

#[async_trait]
impl Classifier for OpenAiCompatClassifier {
    fn provider(&self) -> &str {
        &self.provider_name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        let (system, user) = build_prompt(&req, self.top_n);
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user   }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "classify_repo",
                    "strict": true,
                    "schema": Self::response_schema(self.top_n)
                }
            }
        });
        let start = Instant::now();
        let resp = self
            .client
            .post(&self.endpoint)
            .header("authorization", format!("Bearer {}", self.api_key.expose()))
            .header("content-type", "application/json")
            .timeout(self.request_timeout)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(QaError::Classifier(format!(
                "{} {status}: {v}",
                self.provider_name
            )));
        }
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                QaError::Classifier(format!(
                    "{}: missing choices[0].message.content: {v}",
                    self.provider_name
                ))
            })?;
        let parsed: Value = serde_json::from_str(content).map_err(|e| {
            QaError::Classifier(format!("{}: content not JSON: {e}", self.provider_name))
        })?;
        let verdicts: Vec<RepoVerdict> = serde_json::from_value(parsed["top_candidates"].clone())
            .map_err(|e| {
            QaError::Classifier(format!("{}: top_candidates parse: {e}", self.provider_name))
        })?;
        Ok(ClassifyResponse {
            top_candidates: verdicts,
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}
