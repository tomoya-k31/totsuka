//! Structured logging with unconditional secret masking (§5.2).
//!
//! Three concerns, each independently testable:
//! - [`redact`]: the masking rules (field denylist + value patterns).
//! - [`layer`]: a `tracing` layer emitting redacted JSON Lines / human lines.
//! - [`rotation`]: startup pruning of old daily log files.
//!
//! [`init`] wires them together: a JSON file layer under a daily-rotated
//! appender in `$XDG_STATE_HOME/totsuka/logs/`, plus (optionally) a human
//! terminal layer that respects `NO_COLOR` and non-TTY output (§7).
//!
//! ## Log line convention (read by the `logs` command, #64)
//!
//! One JSON object per line with `timestamp`, `level`, `target`, optional
//! `message`, and event fields. Attach `task_id` as a field to correlate a
//! task's events (`logs --task <id>`).

pub mod layer;
pub mod redact;
pub mod rotation;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use tracing::Level;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

pub use layer::{LogFormat, RedactingLayer};

/// Default number of daily log files to keep.
pub const DEFAULT_MAX_FILES: usize = 7;

/// Runtime logging configuration (assembled by the CLI, #64).
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Directory for rotated log files (`$XDG_STATE_HOME/totsuka/logs`).
    pub dir: PathBuf,
    /// Base filename for daily files (`{prefix}.YYYY-MM-DD`).
    pub file_prefix: String,
    /// Minimum level to record.
    pub level: Level,
    /// Whether prompt/RPC-payload fields are logged (§5.2).
    pub log_prompts: bool,
    /// Number of daily files to keep (0 = keep all).
    pub max_files: usize,
    /// Whether to also log a human-readable stream to stderr.
    pub terminal: bool,
}

impl LogConfig {
    /// A config rooted at `dir` with sensible defaults.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            file_prefix: "totsuka.log".to_string(),
            level: Level::INFO,
            log_prompts: true,
            max_files: DEFAULT_MAX_FILES,
            terminal: true,
        }
    }
}

/// Errors from initializing logging.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// Creating the log directory or pruning old files failed.
    #[error("log io error: {0}")]
    Io(#[from] std::io::Error),
    /// Installing the global subscriber failed (already set).
    #[error("failed to install tracing subscriber: {0}")]
    Install(String),
}

/// Keeps the background log-writer worker alive; drop on shutdown to flush.
#[must_use = "dropping the guard stops the background log writer"]
pub struct LogGuard {
    _appender: tracing_appender::non_blocking::WorkerGuard,
}

/// A `MakeWriter` that writes to the process stderr.
struct StderrWriter;
impl<'a> MakeWriter<'a> for StderrWriter {
    type Writer = std::io::Stderr;
    fn make_writer(&'a self) -> Self::Writer {
        std::io::stderr()
    }
}

/// Install the global logging subscriber. Returns a guard that must be kept
/// alive for the lifetime of the program.
pub fn init(config: &LogConfig) -> Result<LogGuard, LogError> {
    std::fs::create_dir_all(&config.dir)?;
    // Prune old daily files on startup (§5.2).
    rotation::enforce_retention(&config.dir, &config.file_prefix, config.max_files)?;

    let appender = tracing_appender::rolling::daily(&config.dir, &config.file_prefix);
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let level = LevelFilter::from_level(config.level);

    let file_layer = RedactingLayer::new(file_writer, LogFormat::Json, config.log_prompts, false)
        .with_filter(level);

    let registry = tracing_subscriber::registry().with(file_layer);

    let result = if config.terminal {
        // Respect NO_COLOR and non-TTY output (§7).
        let ansi = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        let term_layer =
            RedactingLayer::new(StderrWriter, LogFormat::Human, config.log_prompts, ansi)
                .with_filter(level);
        registry.with(term_layer).try_init()
    } else {
        registry.try_init()
    };
    result.map_err(|e| LogError::Install(e.to_string()))?;

    Ok(LogGuard { _appender: guard })
}

/// Parse a level name (`error`/`warn`/`info`/`debug`/`trace`), case-insensitive.
pub fn parse_level(name: &str) -> Option<Level> {
    match name.to_ascii_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" | "warning" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

/// Convenience: the default log directory under a state directory.
pub fn default_log_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels_case_insensitively() {
        assert_eq!(parse_level("DEBUG"), Some(Level::DEBUG));
        assert_eq!(parse_level("warn"), Some(Level::WARN));
        assert_eq!(parse_level("Warning"), Some(Level::WARN));
        assert_eq!(parse_level("nope"), None);
    }

    #[test]
    fn default_config_has_expected_shape() {
        let cfg = LogConfig::new("/state/logs");
        assert_eq!(cfg.level, Level::INFO);
        assert!(cfg.log_prompts);
        assert_eq!(cfg.max_files, DEFAULT_MAX_FILES);
        assert_eq!(
            default_log_dir(Path::new("/state")),
            Path::new("/state/logs")
        );
    }
}
