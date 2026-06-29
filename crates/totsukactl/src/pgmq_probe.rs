use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use std::sync::Mutex;

#[async_trait]
pub trait PgmqProbe: Send + Sync {
    async fn ping(&self) -> Result<bool, TotsukactlError>;
}

pub struct LivePgmqProbe {
    pub compose: Arc<dyn ComposeExec>,
    pub pool: PgPool,
}

#[async_trait]
impl PgmqProbe for LivePgmqProbe {
    async fn ping(&self) -> Result<bool, TotsukactlError> {
        if !self.compose.ps_running("pgmq").await? {
            return Ok(false);
        }
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error=%e, "pgmq SELECT 1 failed");
                Ok(false)
            }
        }
    }
}

pub struct MockPgmqProbe {
    pub answer: Mutex<bool>,
}

impl MockPgmqProbe {
    pub fn new(initial: bool) -> Self {
        Self {
            answer: Mutex::new(initial),
        }
    }
    pub fn set(&self, v: bool) {
        *self.answer.lock().unwrap() = v;
    }
}

#[async_trait]
impl PgmqProbe for MockPgmqProbe {
    async fn ping(&self) -> Result<bool, TotsukactlError> {
        Ok(*self.answer.lock().unwrap())
    }
}
