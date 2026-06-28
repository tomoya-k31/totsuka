use crate::{envelope::envelope_to_json, pgmq::BusError};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::sync::Arc;
use totsuka_core::{Clock, DomainEvent, EventEnvelope};

pub struct Publisher {
    queue: String,
    clock: Arc<dyn Clock>,
}

impl Publisher {
    pub fn new(queue: impl Into<String>, clock: Arc<dyn Clock>) -> Self {
        Self {
            queue: queue.into(),
            clock,
        }
    }

    /// 通常の (tx 外) publish
    pub async fn send(
        &self,
        pool: &PgPool,
        ev: DomainEvent,
        trace_id: Option<String>,
    ) -> Result<i64, BusError> {
        let env = EventEnvelope::from_domain(ev, self.clock.now(), trace_id);
        let v = envelope_to_json(&env)?;
        crate::pgmq::send_json(pool, &self.queue, &v).await
    }

    /// 同一 tx で publish (cursor 更新等とアトミック)。spec §9.3
    pub async fn send_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        ev: DomainEvent,
        trace_id: Option<String>,
    ) -> Result<i64, BusError> {
        let env = EventEnvelope::from_domain(ev, self.clock.now(), trace_id);
        let v = envelope_to_json(&env)?;
        let row = sqlx::query("SELECT pgmq.send($1, $2::jsonb) AS msg_id")
            .bind(&self.queue)
            .bind(&v)
            .fetch_one(&mut **tx)
            .await?;
        Ok(row.get::<i64, _>("msg_id"))
    }
}
