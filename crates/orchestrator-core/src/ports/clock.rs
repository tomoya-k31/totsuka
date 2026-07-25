//! Wall-clock port (#174).
//!
//! All wall-clock reads in the engine and the state DB go through [`Clock`]
//! so time-dependent logic (worktree retention, signal-timeout sweeps,
//! persisted timestamps) can be tested deterministically. The production
//! implementation ([`SystemClock`](crate::adapters::clock::SystemClock))
//! reads the system time; tests inject
//! [`ManualClock`](crate::adapters::clock::ManualClock).

use time::OffsetDateTime;

/// Source of the current wall-clock time.
///
/// `Send + Sync` because it is shared as `Arc<dyn Clock>` and engine futures
/// cross `tokio::spawn`.
pub trait Clock: Send + Sync {
    /// The current instant in UTC.
    fn now_utc(&self) -> OffsetDateTime;

    /// The current instant as an RFC 3339 UTC string — the canonical
    /// persisted-timestamp form shared by the state DB and the run loop.
    fn now_rfc3339(&self) -> String {
        self.now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC3339 formatting of a UTC timestamp is infallible")
    }
}
