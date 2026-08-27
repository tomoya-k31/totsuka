//! Runtime health of the `run` process (F-110): what it currently **cannot**
//! do, written where any other command can read it.
//!
//! ## Why a file next to `run.lock`, and not a table in `state.db`
//!
//! This is a fact about the process that is running right now, not about the
//! history of a task — the same category as [`run_lock`](super::run_lock),
//! which is already a file in the state directory rather than a row. Adding a
//! table would also mean a schema migration, and an older totsuka refuses to
//! open a newer database (ADR-0017): a wall worth raising for the audit trail,
//! not for a menu-bar glyph.
//!
//! ## What belongs in here
//!
//! **Only facts that can be re-asked every cycle.** The file is rewritten in
//! full on each `cycle()`, so a condition that clears disappears on its own
//! and the operator never has to dismiss anything. A one-off event (a
//! notification that failed to deliver, a cleanup that could not run) cannot
//! be re-derived, so it would latch on forever and turn the warning glyph into
//! background noise — those stay in the log.
//!
//! ## Structure, not prose
//!
//! Each [`Degradation`] stores its fields and nothing else; the sentence is
//! built at read time by [`Degradation::message`]. An upgraded binary then
//! explains an old file with its current wording, and a `kind` this build has
//! never heard of still gets a row rather than vanishing — the same contract
//! `totsuka status` keeps for task wait reasons.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How long a published document stays believable.
///
/// `run` republishes at the end of **every** `cycle()`, and the loop runs one
/// at least every `SETTLE_TICK` (200 ms) even when nothing happens — so 120 s
/// is 600× the expected gap. Wide enough that a slow worktree sweep never
/// trips it; tight enough that a run wedged inside a plugin call (its pid
/// still alive, so `run.lock` still says "running") stops being read as the
/// current truth within two minutes.
///
/// **Staleness is not silence.** A reader that simply dropped a stale document
/// would report "healthy" about a process that has stopped saying anything,
/// which is the failure this whole module exists to prevent. It is surfaced as
/// its own condition instead.
pub const STALE_AFTER_SECS: i64 = 120;

/// One reason the orchestrator cannot do its whole job right now.
///
/// `kind` is the wire discriminant; an unknown one deserializes into
/// [`Degradation::Unknown`] rather than failing the whole document, so a
/// downgraded reader degrades to "something is wrong, and here is its name".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Degradation {
    /// The hook receiver could not bind its socket, so no agent can report
    /// completion for the whole of this run.
    HookReceiverDown {
        /// The socket path that could not be bound.
        socket: String,
    },
    /// A plugin process is not answering.
    PluginDown {
        /// Plugin instance name.
        plugin: String,
        /// Whether the supervisor has given up relaunching it (#495/#499) —
        /// the difference between "it will probably come back" and "it will
        /// not".
        abandoned: bool,
    },
    /// Hook signals are piling up in the spool because POSTs are failing.
    SpoolBacklog {
        /// Number of spool files waiting to be replayed.
        files: usize,
    },
    /// The LLM gateway rejected the configured credentials (401/403).
    LlmKeyRejected,
    /// A `kind` this build does not know. Naming it beats dropping it: a
    /// dropped row reads as "not degraded".
    #[serde(other)]
    Unknown,
}

impl Degradation {
    /// The `kind` string as it appears on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            Degradation::HookReceiverDown { .. } => "hook_receiver_down",
            Degradation::PluginDown { .. } => "plugin_down",
            Degradation::SpoolBacklog { .. } => "spool_backlog",
            Degradation::LlmKeyRejected => "llm_key_rejected",
            Degradation::Unknown => "unknown",
        }
    }

    /// One operator-facing line: what is broken, and what it costs.
    ///
    /// Built here rather than stored, so the wording follows the binary doing
    /// the reading (see the module docs).
    pub fn message(&self) -> String {
        match self {
            Degradation::HookReceiverDown { socket } => format!(
                "the hook receiver could not bind {socket} → no task can report completion \
                 for this whole run; restart `totsuka run` once the path is free"
            ),
            Degradation::PluginDown { plugin, abandoned } if *abandoned => format!(
                "plugin `{plugin}` is down and will not be relaunched → tasks needing it \
                 fail instead of waiting; fix it and restart `totsuka run`"
            ),
            Degradation::PluginDown { plugin, .. } => {
                format!("plugin `{plugin}` is down → tasks needing it stay queued until it is back")
            }
            Degradation::SpoolBacklog { files } => format!(
                "{files} hook signal file(s) are stuck in the spool → POSTs are failing; \
                 check the socket path and the Bearer token with `totsuka doctor`"
            ),
            Degradation::LlmKeyRejected => "the LLM gateway rejected the API key → repository \
                 selection falls back to asking you for every new conversation; reissue the key \
                 and update `[llm].api_key_ref`"
                .to_string(),
            Degradation::Unknown => {
                "this build does not recognise the reported degradation → check `totsuka logs`"
                    .to_string()
            }
        }
    }
}

/// The whole health document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHealth {
    /// PID of the `run` that wrote this. A reader cross-checks it against
    /// `run.lock`: a file left behind by a crashed run describes a process
    /// that no longer exists, and must not be read as the current truth.
    pub pid: u32,
    /// When it was written (ISO 8601 UTC).
    pub recorded_at: String,
    /// Everything currently wrong. Empty means healthy.
    pub degraded: Vec<Degradation>,
}

impl RunHealth {
    /// Whether anything is wrong.
    pub fn is_degraded(&self) -> bool {
        !self.degraded.is_empty()
    }

    /// Seconds since this document was written, or `None` if `recorded_at`
    /// does not parse (a hand-edited or truncated file).
    pub fn age_secs(&self, now: OffsetDateTime) -> Option<i64> {
        let written = OffsetDateTime::parse(
            &self.recorded_at,
            &time::format_description::well_known::Rfc3339,
        )
        .ok()?;
        Some((now - written).whole_seconds())
    }

    /// Whether the publishing run has gone quiet for longer than
    /// [`STALE_AFTER_SECS`].
    ///
    /// An unparseable timestamp counts as stale: it is not evidence of
    /// freshness, and the safe direction is to say so rather than to trust it.
    pub fn is_stale(&self, now: OffsetDateTime) -> bool {
        self.age_secs(now).is_none_or(|age| age > STALE_AFTER_SECS)
    }
}

/// The health file under a state directory.
pub fn path_in(state_dir: &Path) -> PathBuf {
    state_dir.join("health.json")
}

/// Write `health` to `path`, replacing whatever was there.
///
/// Written to a sibling temporary file and renamed, so a reader polling every
/// few seconds sees either the old document or the new one — never half of
/// one. The temporary file is removed on a failed rename rather than left to
/// accumulate.
pub fn write(path: &Path, health: &RunHealth) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(health)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Delete the health file. A missing file is success — the caller's intent is
/// "there must be no health here", and there is not.
pub fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Read the health file, if it exists and parses.
///
/// **Absent or unreadable is `None`, never an error.** Every reader is a
/// status view whose job is to say what it can; a malformed file must not stop
/// `totsuka status` from listing tasks.
pub fn read(path: &Path) -> Option<RunHealth> {
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(degraded: Vec<Degradation>) -> RunHealth {
        RunHealth {
            pid: 4821,
            recorded_at: "2026-08-28T00:00:00Z".to_string(),
            degraded,
        }
    }

    #[test]
    fn a_round_trip_preserves_every_variant() {
        let all = health(vec![
            Degradation::HookReceiverDown {
                socket: "/tmp/a.sock".to_string(),
            },
            Degradation::PluginDown {
                plugin: "slack".to_string(),
                abandoned: true,
            },
            Degradation::SpoolBacklog { files: 4 },
            Degradation::LlmKeyRejected,
        ]);
        let dir = test_support::scratch("run_health_roundtrip");
        let p = path_in(&dir);
        write(&p, &all).unwrap();
        assert_eq!(read(&p).unwrap(), all);
    }

    /// A `kind` from a newer build must not take the whole document with it.
    #[test]
    fn an_unknown_kind_becomes_a_row_rather_than_a_parse_failure() {
        let dir = test_support::scratch("run_health_unknown_kind");
        let p = path_in(&dir);
        std::fs::write(
            &p,
            r#"{"pid":1,"recorded_at":"t","degraded":[{"kind":"from_the_future","detail":9}]}"#,
        )
        .unwrap();
        let got = read(&p).expect("the document still parses");
        assert_eq!(got.degraded, vec![Degradation::Unknown]);
        assert!(got.is_degraded());
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_none() {
        let dir = test_support::scratch("run_health_missing");
        let p = path_in(&dir);
        assert_eq!(read(&p), None);
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(read(&p), None);
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        let dir = test_support::scratch("run_health_no_tmp");
        let p = path_in(&dir);
        write(&p, &health(vec![])).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "health.json")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn removing_an_absent_file_succeeds() {
        let dir = test_support::scratch("run_health_remove");
        let p = path_in(&dir);
        remove(&p).expect("absent is the intended end state");
        write(&p, &health(vec![])).unwrap();
        remove(&p).unwrap();
        assert!(!p.exists());
    }

    /// The wording lives in the reader, so every variant must have one — and
    /// it must name a next action, per the CLI's error convention (§7).
    #[test]
    fn every_degradation_explains_itself_and_says_what_to_do() {
        for d in [
            Degradation::HookReceiverDown {
                socket: "/s".to_string(),
            },
            Degradation::PluginDown {
                plugin: "p".to_string(),
                abandoned: false,
            },
            Degradation::PluginDown {
                plugin: "p".to_string(),
                abandoned: true,
            },
            Degradation::SpoolBacklog { files: 1 },
            Degradation::LlmKeyRejected,
            Degradation::Unknown,
        ] {
            let m = d.message();
            assert!(m.contains('→'), "{}: {m}", d.kind());
            // Line continuations in the source must not survive as runs of
            // spaces in text a human reads.
            assert!(!m.contains("  "), "{}: {m}", d.kind());
        }
    }

    /// `abandoned` changes what the operator should do, so it must change the
    /// sentence too.
    #[test]
    fn an_abandoned_plugin_reads_differently_from_a_restarting_one() {
        let restarting = Degradation::PluginDown {
            plugin: "slack".to_string(),
            abandoned: false,
        };
        let abandoned = Degradation::PluginDown {
            plugin: "slack".to_string(),
            abandoned: true,
        };
        assert_ne!(restarting.message(), abandoned.message());
        assert!(abandoned.message().contains("will not be relaunched"));
    }
}

#[cfg(test)]
mod staleness_tests {
    use super::*;
    use time::Duration as TimeDuration;

    fn at(recorded_at: &str) -> RunHealth {
        RunHealth {
            pid: 1,
            recorded_at: recorded_at.to_string(),
            degraded: Vec::new(),
        }
    }

    #[test]
    fn a_fresh_document_is_not_stale() {
        let now = OffsetDateTime::now_utc();
        let h = at(&now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap());
        assert_eq!(h.age_secs(now), Some(0));
        assert!(!h.is_stale(now));
        assert!(!h.is_stale(now + TimeDuration::seconds(STALE_AFTER_SECS)));
    }

    /// The case this exists for: a run wedged inside a plugin call keeps its
    /// pid, so `run.lock` still says "running" while nothing is published.
    #[test]
    fn a_document_that_stopped_being_republished_goes_stale() {
        let now = OffsetDateTime::now_utc();
        let h = at(&now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap());
        assert!(h.is_stale(now + TimeDuration::seconds(STALE_AFTER_SECS + 1)));
    }

    /// An unparseable timestamp is not evidence of freshness.
    #[test]
    fn an_unreadable_timestamp_counts_as_stale() {
        let h = at("whenever");
        let now = OffsetDateTime::now_utc();
        assert_eq!(h.age_secs(now), None);
        assert!(h.is_stale(now));
    }
}
