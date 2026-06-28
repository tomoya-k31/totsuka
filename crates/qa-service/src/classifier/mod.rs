//! LLM repo classifier. See spec §8.4 — 2 impls (anthropic + openai_compat)
//! cover 4 mandatory providers (anthropic / openai / openrouter / litellm)
//! plus 1 catch-all (openai_compatible). Factory dispatch by provider string.

use async_trait::async_trait;

pub mod anthropic;
pub mod mock;
pub mod openai_compat;
pub mod prompt;
pub mod retry;
pub mod schema;

pub use anthropic::AnthropicClassifier;
pub use mock::MockClassifier;
pub use openai_compat::OpenAiCompatClassifier;
pub use prompt::build_prompt;
pub use retry::with_classify_retry;
pub use schema::{ClassifyRequest, ClassifyResponse, RepoCandidate, RepoVerdict};

use crate::error::QaError;

#[async_trait]
pub trait Classifier: Send + Sync + 'static {
    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError>;
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
}

use std::sync::Arc;
use std::time::Duration;
use totsuka_config::schema::ClassifierSection;

pub fn build(cfg: &ClassifierSection) -> Result<Arc<dyn Classifier>, QaError> {
    let timeout = Duration::from_secs(cfg.request_timeout_secs);
    match cfg.provider.as_str() {
        "anthropic" => {
            let endpoint = if cfg.api_base.is_empty() {
                None
            } else {
                Some(format!(
                    "{}/v1/messages",
                    cfg.api_base.trim_end_matches('/')
                ))
            };
            Ok(Arc::new(anthropic::AnthropicClassifier::new(
                cfg.api_key.clone(),
                cfg.model.clone(),
                cfg.max_tokens,
                cfg.top_candidates,
                timeout,
                endpoint,
            )))
        }
        provider @ ("openai" | "openrouter" | "litellm" | "openai_compatible") => {
            let base = if cfg.api_base.is_empty() {
                match provider {
                    "openai" => "https://api.openai.com/v1".to_string(),
                    "openrouter" => "https://openrouter.ai/api/v1".to_string(),
                    _ => {
                        return Err(QaError::Classifier(format!(
                            "{provider}: api_base is required (no default)"
                        )))
                    }
                }
            } else {
                cfg.api_base.trim_end_matches('/').to_string()
            };
            let endpoint = format!("{base}/chat/completions");
            Ok(Arc::new(openai_compat::OpenAiCompatClassifier::new(
                provider.into(),
                endpoint,
                cfg.api_key.clone(),
                cfg.model.clone(),
                cfg.max_tokens,
                cfg.top_candidates,
                timeout,
            )))
        }
        other => Err(QaError::Classifier(format!("unknown provider: {other}"))),
    }
}
