//! Wall-clock port (#174).
//!
//! All wall-clock reads in the engine and the state DB go through [`Clock`]
//! so time-dependent logic (worktree retention, signal-timeout sweeps,
//! persisted timestamps) can be tested deterministically. The production
//! implementation ([`SystemClock`](crate::adapters::clock::SystemClock))
//! reads the system time; tests inject
//! [`ManualClock`](crate::adapters::clock::ManualClock).

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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
        format_rfc3339(self.now_utc())
    }
}

/// The persisted-timestamp text of `t` — the only formatter the state DB's
/// timestamps go through, on the way in and back out (`--json`, #765).
///
/// Round-trips with [`parse_rfc3339`] byte for byte on everything it writes:
/// the subsecond digits are as many as needed and no more (`…00Z`,
/// `…00.5Z`, `…00.53Z`), which is also why these strings must never be
/// compared as text (#478).
pub fn format_rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339)
        .expect("RFC3339 formatting of a UTC timestamp is infallible")
}

/// Parse a timestamp written by [`format_rfc3339`].
pub fn parse_rfc3339(s: &str) -> Result<OffsetDateTime, time::error::Parse> {
    OffsetDateTime::parse(s, &Rfc3339)
}
