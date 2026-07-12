//! OpenAI-compatible [`LlmRouter`] over `reqwest` (F-12, F-13, §5.3).
//!
//! Posts to `{base_url}/chat/completions` with `response_format: json_schema`
//! and returns the parsed structured content. Retryable failures (network,
//! timeout, 429/5xx) are retried with exponential backoff (§5.3).

use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::SecretString;
use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};

/// Configuration for the OpenAI-compatible router.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL (e.g. `https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// Model name.
    pub model: String,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Number of retries for retryable failures (§5.3).
    pub max_retries: u32,
    /// Base backoff between retries (doubles each attempt).
    pub backoff_base: Duration,
}

impl OpenAiConfig {
    /// Sensible defaults for a base URL + model.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            timeout: Duration::from_secs(30),
            max_retries: 3,
            backoff_base: Duration::from_millis(500),
        }
    }
}

/// An OpenAI-compatible chat router.
pub struct OpenAiRouter {
    client: reqwest::Client,
    config: OpenAiConfig,
    api_key: SecretString,
}

impl OpenAiRouter {
    /// Build a router with a resolved API key (F-65).
    pub fn new(config: OpenAiConfig, api_key: SecretString) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
            api_key,
        }
    }

    /// The `/chat/completions` URL.
    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// One request attempt, mapping transport/status errors to [`LlmError`].
    async fn attempt(&self, body: &Value) -> Result<Value, LlmError> {
        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(self.api_key.expose())
            .timeout(self.config.timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(self.config.timeout.as_secs())
                } else {
                    LlmError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }

        // choices[0].message.content is a JSON string (structured output).
        let envelope: Value =
            serde_json::from_str(&text).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let content = envelope["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                LlmError::InvalidResponse("missing choices[0].message.content".into())
            })?;
        serde_json::from_str(content).map_err(|e| LlmError::InvalidResponse(e.to_string()))
    }
}

impl LlmRouter for OpenAiRouter {
    fn chat_json(
        &self,
        request: &ChatRequest,
    ) -> impl std::future::Future<Output = Result<Value, LlmError>> + Send {
        let body = json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens,
            "messages": [
                {"role": "system", "content": request.system},
                {"role": "user", "content": request.user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "structured_output",
                    "schema": request.json_schema,
                    "strict": true,
                },
            },
        });

        async move {
            let mut attempt = 0;
            loop {
                match self.attempt(&body).await {
                    Ok(value) => return Ok(value),
                    Err(e) if e.is_retryable() && attempt < self.config.max_retries => {
                        let delay = self.config.backoff_base * 2u32.pow(attempt);
                        tracing::warn!(attempt, error = %e, "llm call failed; retrying");
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}
