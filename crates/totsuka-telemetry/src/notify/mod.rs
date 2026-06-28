pub mod payload;
pub mod routing;
pub mod sink_log;
pub mod sink_slack;

pub use payload::NotifyPayload;
pub use routing::{default_dedup_ttl, default_routing, SinkId};
pub use sink_log::LogSink;
pub use sink_slack::SlackSink;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use totsuka_core::{Clock, NotifyKind};

#[async_trait::async_trait]
pub trait NotifySink: Send + Sync {
    fn id(&self) -> SinkId;
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("sink io: {0}")]
    Io(String),
    #[error("sink http: {0}")]
    Http(String),
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    dedup: HashMap<String, DateTime<Utc>>,
}

pub struct Notifier {
    clock: Arc<dyn Clock>,
    state: Arc<Mutex<PersistedState>>,
    state_path: PathBuf,
    sinks: Vec<Arc<dyn NotifySink>>,
    routing: HashMap<NotifyKind, Vec<SinkId>>,
    dedup_ttl: HashMap<NotifyKind, u64>,
}

impl Notifier {
    pub async fn new(
        clock: Arc<dyn Clock>,
        state_path: PathBuf,
        sinks: Vec<Arc<dyn NotifySink>>,
        routing: HashMap<NotifyKind, Vec<SinkId>>,
        dedup_ttl: HashMap<NotifyKind, u64>,
    ) -> Self {
        let state = load_state(&state_path).await.unwrap_or_default();
        Self {
            clock,
            state: Arc::new(Mutex::new(state)),
            state_path,
            sinks,
            routing,
            dedup_ttl,
        }
    }

    pub async fn notify(
        &self,
        kind: NotifyKind,
        dedup_key: impl Into<String>,
        payload: NotifyPayload,
    ) {
        let dkey = format!("{}:{}", kind.as_snake(), dedup_key.into());
        let ttl_secs = self.dedup_ttl.get(&kind).copied().unwrap_or(0);
        let now = self.clock.now();

        if ttl_secs > 0 {
            let mut g = self.state.lock().await;
            if let Some(last) = g.dedup.get(&dkey) {
                // clamp at 0 so a backward clock step (NTP slew, MockClock reset) does NOT
                // wrap to u64::MAX and silently bypass the dedup window
                let age = (now - *last).num_seconds().max(0) as u64;
                if age < ttl_secs {
                    tracing::debug!(kind=?kind, dedup_key=%dkey, age_secs=age, "notify deduped");
                    return;
                }
            }
            // Optimistically record now before dispatching (prevents TOCTOU)
            g.dedup.insert(dkey.clone(), now);
            let snapshot =
                serde_json::to_vec_pretty(&*g).expect("PersistedState serialization is infallible");
            drop(g);
            // Persist in background (fire-and-forget with log on error)
            if let Err(e) = atomic_write(&self.state_path, &snapshot).await {
                tracing::warn!(error=%e, "failed to persist notify state");
            }
        }

        let sink_ids = self
            .routing
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| vec![SinkId::Log]);
        for sid in sink_ids {
            if let Some(sink) = self.sinks.iter().find(|s| s.id() == sid) {
                if let Err(e) = sink.send(kind, &payload).await {
                    tracing::warn!(kind=?kind, sink=?sid, error=%e, "sink failed");
                }
            }
        }
    }
}

async fn load_state(path: &PathBuf) -> Option<PersistedState> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn atomic_write(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicU32, Ordering};
    use totsuka_core::MockClock;

    struct CountSink {
        id: SinkId,
        count: AtomicU32,
    }
    #[async_trait::async_trait]
    impl NotifySink for CountSink {
        fn id(&self) -> SinkId {
            self.id
        }
        async fn send(&self, _: NotifyKind, _: &NotifyPayload) -> Result<(), SinkError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn ttl_map() -> HashMap<NotifyKind, u64> {
        let mut m = HashMap::new();
        m.insert(NotifyKind::TaskStuck, 60);
        m.insert(NotifyKind::ProcessDead, 0);
        m
    }

    fn route_log_only() -> HashMap<NotifyKind, Vec<SinkId>> {
        let mut m = HashMap::new();
        for k in [NotifyKind::TaskStuck, NotifyKind::ProcessDead] {
            m.insert(k, vec![SinkId::Log]);
        }
        m
    }

    #[tokio::test]
    async fn dedup_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns.json");
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
        ));
        let sink = Arc::new(CountSink {
            id: SinkId::Log,
            count: AtomicU32::new(0),
        });
        let n = Notifier::new(
            clock.clone(),
            path,
            vec![sink.clone()],
            route_log_only(),
            ttl_map(),
        )
        .await;

        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default())
            .await;
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default())
            .await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 1);

        clock.advance(chrono::Duration::seconds(61));
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default())
            .await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ttl_zero_never_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
        ));
        let sink = Arc::new(CountSink {
            id: SinkId::Log,
            count: AtomicU32::new(0),
        });
        let n = Notifier::new(
            clock,
            dir.path().join("ns.json"),
            vec![sink.clone()],
            route_log_only(),
            ttl_map(),
        )
        .await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default())
            .await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default())
            .await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default())
            .await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn dedup_state_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns.json");
        let clock = Arc::new(MockClock::new(
            Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
        ));

        let sink1 = Arc::new(CountSink {
            id: SinkId::Log,
            count: AtomicU32::new(0),
        });
        let n = Notifier::new(
            clock.clone(),
            path.clone(),
            vec![sink1.clone()],
            route_log_only(),
            ttl_map(),
        )
        .await;
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default())
            .await;
        assert_eq!(sink1.count.load(Ordering::SeqCst), 1);

        // 「再起動」をシミュレート: 新 Notifier を同じ state_path で構築
        let sink2 = Arc::new(CountSink {
            id: SinkId::Log,
            count: AtomicU32::new(0),
        });
        let n2 = Notifier::new(
            clock,
            path,
            vec![sink2.clone()],
            route_log_only(),
            ttl_map(),
        )
        .await;
        n2.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default())
            .await;
        // dedup state が読み込まれているので 0 のまま
        assert_eq!(sink2.count.load(Ordering::SeqCst), 0);
    }
}
