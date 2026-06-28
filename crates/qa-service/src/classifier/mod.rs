//! LLM repo classifier. See spec §8.4 — 2 impls (anthropic + openai_compat)
//! cover 4 mandatory providers (anthropic / openai / openrouter / litellm)
//! plus 1 catch-all (openai_compatible). Factory dispatch by provider string.

use async_trait::async_trait;

pub mod anthropic;
pub mod mock;
pub mod prompt;
pub mod retry;
pub mod schema;

pub use anthropic::AnthropicClassifier;
pub use mock::MockClassifier;
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
