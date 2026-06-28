use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub struct WipGate {
    sem: Arc<Semaphore>,
}

impl WipGate {
    pub fn new(capacity: u32) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(capacity as usize)),
        }
    }

    pub fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.sem.clone().try_acquire_owned().ok()
    }

    pub fn available(&self) -> usize {
        self.sem.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_blocks_when_full() {
        let g = WipGate::new(2);
        let _a = g.try_acquire().unwrap();
        let _b = g.try_acquire().unwrap();
        assert!(g.try_acquire().is_none());
    }

    #[test]
    fn permit_release_restores_slot() {
        let g = WipGate::new(1);
        let permit = g.try_acquire().unwrap();
        assert!(g.try_acquire().is_none());
        drop(permit);
        assert!(g.try_acquire().is_some());
    }
}
