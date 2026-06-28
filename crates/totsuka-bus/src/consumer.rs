use crate::{
    envelope::json_to_envelope,
    pgmq::{self, BusError, PgmqMessage},
};
use sqlx::PgPool;
use totsuka_core::EventEnvelope;

pub struct Consumer {
    queue: String,
}

impl Consumer {
    pub fn new(queue: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
        }
    }

    pub async fn poll_one(
        &self,
        pool: &PgPool,
        vt_secs: i32,
    ) -> Result<Option<(i64, EventEnvelope)>, BusError> {
        let m = pgmq::read_one(pool, &self.queue, vt_secs).await?;
        let Some(PgmqMessage {
            msg_id, message, ..
        }) = m
        else {
            return Ok(None);
        };
        let env = json_to_envelope(message)?;
        Ok(Some((msg_id, env)))
    }

    pub async fn ack(&self, pool: &PgPool, msg_id: i64) -> Result<bool, BusError> {
        pgmq::delete(pool, &self.queue, msg_id).await
    }
}
