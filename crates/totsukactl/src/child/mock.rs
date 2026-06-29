use super::{ChildSpec, ChildSpawner};
use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

pub struct MockSpawner {
    pub next_pid: AtomicI32,
    pub spawned: Mutex<Vec<String>>,
    pub fail_for: Mutex<Vec<String>>,
}

impl Default for MockSpawner {
    fn default() -> Self {
        Self {
            next_pid: AtomicI32::new(10_000),
            spawned: Mutex::new(Vec::new()),
            fail_for: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChildSpawner for MockSpawner {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError> {
        if self.fail_for.lock().unwrap().iter().any(|n| n == &spec.name) {
            return Err(TotsukactlError::Spawn(format!("mock: fail {}", spec.name)));
        }
        self.spawned.lock().unwrap().push(spec.name.clone());
        Ok(self.next_pid.fetch_add(1, Ordering::SeqCst))
    }
}
