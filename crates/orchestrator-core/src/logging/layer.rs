//! A `tracing` [`Layer`] that emits one redacted event per line (§5.2).
//!
//! Every field passes through [`redact`](super::redact) before it is written,
//! so the output is a redacted-by-construction stream. The JSON format
//! guarantees valid JSON Lines (one object per line, `jq`-parseable); the human
//! format is for the terminal. Prompt/payload fields are dropped entirely when
//! `log_prompts` is false.

use std::fmt::Debug;
use std::io::Write;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};

use super::redact::{is_prompt_field, redact_field};

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
    log_prompts: bool,
    message: Option<String>,
    fields: Map<String, Value>,
}

impl FieldCollector {
    fn new(log_prompts: bool) -> Self {
        Self {
            log_prompts,
            message: None,
            fields: Map::new(),
        }
    }

    fn record(&mut self, field: &Field, value: String) {
        let name = field.name();
        // Drop prompt/payload fields unless explicitly enabled (§5.2).
        if is_prompt_field(name) && !self.log_prompts {
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
        let mut collector = FieldCollector::new(self.log_prompts);
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
                if let Some(message) = &collector.message {
                    line.push_str(": ");
                    line.push_str(message);
                }
                for (k, v) in &collector.fields {
                    if let Value::String(s) = v {
                        line.push_str(&format!(" {k}={s}"));
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

    fn capture(log_prompts: bool, emit: impl FnOnce()) -> String {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let layer =
            RedactingLayer::new(BufWriter(buf.clone()), LogFormat::Json, log_prompts, false);
        let subscriber = Registry::default().with(layer);
        with_default(subscriber, emit);
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
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

        // ...but present when enabled.
        let out = capture(true, || {
            tracing::debug!(prompt = "visible prompt", "dispatching");
        });
        assert!(out.contains("visible prompt"));
    }
}
