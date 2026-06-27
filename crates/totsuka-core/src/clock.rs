use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// テスト用。advance() で時刻を進められる
#[derive(Clone)]
pub struct MockClock {
    inner: Arc<Mutex<DateTime<Utc>>>,
}

impl MockClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(now)),
        }
    }
    pub fn advance(&self, dur: chrono::Duration) {
        let mut g = self.inner.lock().unwrap();
        *g += dur;
    }
    pub fn set(&self, now: DateTime<Utc>) {
        *self.inner.lock().unwrap() = now;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn system_clock_is_close_to_chrono_utc() {
        let c = SystemClock;
        let a = c.now();
        let b = Utc::now();
        assert!((b - a).num_milliseconds().abs() < 100);
    }

    #[test]
    fn mock_clock_advances() {
        let base = Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap();
        let c = MockClock::new(base);
        assert_eq!(c.now(), base);
        c.advance(chrono::Duration::seconds(30));
        assert_eq!(c.now(), base + chrono::Duration::seconds(30));
    }
}
