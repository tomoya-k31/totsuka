//! A `tracing` [`Layer`] that emits one redacted event per line (§5.2).
//!
//! Every field passes through [`redact`](super::redact) before it is written,
//! so the output is a redacted-by-construction stream. The JSON format
//! guarantees valid JSON Lines (one object per line, `jq`-parseable); the human
//! format is for the terminal. Prompt/payload fields are only recorded at
//! debug+ and only when `log_prompts` is enabled; otherwise they are dropped.
//!
//! Redaction and terminal escaping are deliberately two separate stages
//! (#297): redaction is about *who may read the value* and applies to both
//! formats, escaping via [`terminal::safe`](crate::terminal::safe) is about
//! *what the value can do to a screen* and applies to the human format only —
//! the JSON file is read by `jq`, which needs the value `serde_json` wrote,
//! not a second escaping of it.

use std::fmt::Debug;
use std::io::Write;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};

use super::redact::{is_prompt_field, redact_field};
use crate::terminal::safe;

/// Output format of a [`RedactingLayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (JSON Lines) — the persistent file format.
    Json,
    /// Human-readable single line — the terminal format.
    Human,
}

/// A layer that redacts every field and writes one line per event.
pub struct RedactingLayer<W> {
    make_writer: W,
    format: LogFormat,
    log_prompts: bool,
    ansi: bool,
}

impl<W> RedactingLayer<W> {
    /// Build a layer writing to `make_writer` in `format`.
    ///
    /// `log_prompts` gates prompt/payload fields; `ansi` enables terminal
    /// colour for the human format (callers pass the NO_COLOR/TTY decision).
    pub fn new(make_writer: W, format: LogFormat, log_prompts: bool, ansi: bool) -> Self {
        Self {
            make_writer,
            format,
            log_prompts,
            ansi,
        }
    }
}

/// Collects an event's fields into a redacted JSON map + message.
struct FieldCollector {
    /// Whether prompt/payload fields may be recorded for *this* event
    /// (`log_prompts` AND the event level is DEBUG/TRACE).
    allow_prompts: bool,
    message: Option<String>,
    fields: Map<String, Value>,
}

impl FieldCollector {
    fn new(allow_prompts: bool) -> Self {
        Self {
            allow_prompts,
            message: None,
            fields: Map::new(),
        }
    }

    fn record(&mut self, field: &Field, value: String) {
        let name = field.name();
        // Drop prompt/payload fields unless allowed (§5.2): only at debug+
        // and only when `log_prompts` is enabled.
        if is_prompt_field(name) && !self.allow_prompts {
            return;
        }
        let redacted = redact_field(name, &value).into_owned();
        if name == "message" {
            self.message = Some(redacted);
        } else {
            self.fields
                .insert(name.to_string(), Value::String(redacted));
        }
    }
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.record(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }
}

impl<S, W> Layer<S> for RedactingLayer<W>
where
    S: Subscriber,
    W: for<'a> MakeWriter<'a> + 'static,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Prompt/payload fields are only ever logged at debug+ (convention in
        // ai-docs/development/logging-conventions.md), so a stray `info!(prompt=…)`
        // cannot leak the body even with `log_prompts = true`.
        let allow_prompts = self.log_prompts
            && matches!(*meta.level(), tracing::Level::DEBUG | tracing::Level::TRACE);
        let mut collector = FieldCollector::new(allow_prompts);
        event.record(&mut collector);
        let ts = now_rfc3339();

        let line = match self.format {
            LogFormat::Json => {
                let mut obj = Map::new();
                obj.insert("timestamp".into(), Value::String(ts));
                obj.insert("level".into(), Value::String(meta.level().to_string()));
                obj.insert("target".into(), Value::String(meta.target().to_string()));
                if let Some(message) = collector.message {
                    obj.insert("message".into(), Value::String(message));
                }
                for (k, v) in collector.fields {
                    obj.entry(k).or_insert(v);
                }
                // A map of strings always serializes; default keeps output valid.
                serde_json::to_string(&Value::Object(obj)).unwrap_or_default()
            }
            LogFormat::Human => {
                let level = level_label(meta.level(), self.ansi);
                let mut line = format!("{ts} {level} {}", meta.target());
                // Field values carry externally-authored text (`run` logs
                // `title = %task.title`, #297) and this stream goes straight
                // to a terminal, so every value is escaped on the way out.
                // Only the values: the timestamp, level, target and field
                // names are ours, and running our own ANSI colour through
                // `safe` would print the escape instead of applying it.
                if let Some(message) = &collector.message {
                    line.push_str(": ");
                    line.push_str(&safe(message));
                }
                for (k, v) in &collector.fields {
                    if let Value::String(s) = v {
                        line.push_str(&format!(" {k}={}", safe(s)));
                    }
                }
                line
            }
        };

        let mut writer = self.make_writer.make_writer();
        let _ = writeln!(writer, "{line}");
    }
}

/// Format the level, optionally with ANSI colour.
fn level_label(level: &tracing::Level, ansi: bool) -> String {
    let name = level.as_str();
    if !ansi {
        return name.to_string();
    }
    let color = match *level {
        tracing::Level::ERROR => "31", // red
        tracing::Level::WARN => "33",  // yellow
        tracing::Level::INFO => "32",  // green
        tracing::Level::DEBUG => "34", // blue
        tracing::Level::TRACE => "35", // magenta
    };
    format!("\x1b[{color}m{name}\x1b[0m")
}

/// Current time as an RFC 3339 UTC string (matches the state DB convention).
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting of current UTC time is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::Registry;

    /// A `MakeWriter` collecting output into a shared buffer.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufGuard;
        fn make_writer(&'a self) -> Self::Writer {
            BufGuard(self.0.clone())
        }
    }

    struct BufGuard(Arc<Mutex<Vec<u8>>>);
    impl Write for BufGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_as(format: LogFormat, log_prompts: bool, emit: impl FnOnce()) -> String {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer = RedactingLayer::new(BufWriter(buf.clone()), format, log_prompts, false);
        let subscriber = Registry::default().with(layer);
        with_default(subscriber, emit);
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    fn capture(log_prompts: bool, emit: impl FnOnce()) -> String {
        capture_as(LogFormat::Json, log_prompts, emit)
    }

    #[test]
    fn emits_valid_json_lines() {
        let out = capture(true, || {
            tracing::info!(repo = "totsuka", task_id = 7, "dispatching");
            tracing::warn!(count = 3, "slots busy");
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: Value = serde_json::from_str(line).expect("each line must be valid JSON");
            assert!(v.get("timestamp").is_some());
            assert!(v.get("level").is_some());
        }
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["message"], "dispatching");
        assert_eq!(first["repo"], "totsuka");
        assert_eq!(first["task_id"], "7");
    }

    #[test]
    fn redacts_secret_fields_and_values() {
        let out = capture(true, || {
            tracing::info!(
                api_key = "ghp_shouldNotAppear0123456789",
                note = "auth: Bearer secrettoken123",
                "calling api"
            );
        });
        assert!(
            !out.contains("shouldNotAppear"),
            "secret field leaked: {out}"
        );
        assert!(
            !out.contains("secrettoken123"),
            "bearer token leaked: {out}"
        );
        let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(v["api_key"], "***");
        assert_eq!(v["note"], "auth: Bearer ***");
    }

    #[test]
    fn prompt_fields_dropped_when_disabled() {
        let out = capture(false, || {
            tracing::debug!(prompt = "secret user prompt body", "dispatching");
        });
        assert!(
            !out.contains("secret user prompt body"),
            "prompt leaked: {out}"
        );
        let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert!(v.get("prompt").is_none());

        // ...but present at debug+ when enabled.
        let out = capture(true, || {
            tracing::debug!(prompt = "visible prompt", "dispatching");
        });
        assert!(out.contains("visible prompt"));
    }

    /// The human stream is stderr, and `run` logs the task title, which the
    /// person who filed the issue wrote (#297). It must reach the terminal as
    /// text, not as instructions to the terminal — while the JSON file, read
    /// by `jq` and not by a screen, keeps the value exactly as sent.
    #[test]
    fn human_stream_escapes_external_text_while_json_keeps_it_verbatim() {
        let esc = char::from_u32(0x1b).unwrap();
        // ESC[2J clears the screen; the bare CR rewrites the row from column
        // 0, so the operator sees only what came after it.
        let title = format!("{esc}[2Jinnocent\rFORGED");

        let out = capture_as(LogFormat::Human, true, || {
            tracing::info!(title = %title, "task ingested");
        });
        assert!(
            !out.contains(esc),
            "a live ESC reached the terminal: {out:?}"
        );
        assert!(
            !out.contains('\r'),
            "a bare CR reached the terminal: {out:?}"
        );
        // Neutralised, not deleted: the operator can still read what arrived.
        assert!(
            out.contains("innocent") && out.contains("FORGED"),
            "the payload was swallowed: {out:?}"
        );
        // One event stays one line, so a log line cannot forge another.
        assert_eq!(out.lines().count(), 1, "the event split rows: {out:?}");

        // A message field is external text too (it is formatted from one).
        let out = capture_as(LogFormat::Human, true, || {
            tracing::info!("ingested {title}");
        });
        assert!(
            !out.contains(esc),
            "a live ESC reached the terminal: {out:?}"
        );
        assert_eq!(out.lines().count(), 1, "the event split rows: {out:?}");

        // The file format is untouched: `serde_json` already escaped the
        // control characters, and escaping again would corrupt the value.
        let out = capture_as(LogFormat::Json, true, || {
            tracing::info!(title = %title, "task ingested");
        });
        let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(v["title"].as_str().unwrap(), title, "double-escaped: {out}");
    }

    #[test]
    fn prompt_fields_dropped_above_debug_even_when_enabled() {
        // A stray info!(prompt=…) must not leak the body: prompts are debug+.
        let out = capture(true, || {
            tracing::info!(prompt = "should not appear at info", "dispatching");
        });
        assert!(
            !out.contains("should not appear at info"),
            "prompt leaked: {out}"
        );
        let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert!(v.get("prompt").is_none());
    }
}
