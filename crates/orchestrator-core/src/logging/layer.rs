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
use tracing_subscriber::registry::LookupSpan;

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

/// A span's own fields, already redacted, stored in the span's extensions so
/// every event inside it can be labelled with them (#497 follow-up).
///
/// **Extensions are per-span and shared by every layer**, and this process
/// installs two `RedactingLayer`s (JSON to file, human to stderr). So the
/// store must be written at most once and must be **policy-free**: whichever
/// layer sees the span first wins, and if what it stored depended on that
/// layer's settings the other layer would silently render the wrong thing.
///
/// Hence prompt/payload fields are stored (redacted) and filtered at *render*
/// time by each layer, rather than dropped here.
///
/// Without this the layer renders **only the event's own fields**, and a span
/// carrying `plugin` / `method` contributes nothing to the line — which is how
/// `plugin rpc finished elapsed_ms=12 outcome=ok` reached production without
/// saying *which plugin's which method*, the one question the instrumentation
/// existed to answer.
#[derive(Debug, Default)]
struct SpanFields(Map<String, Value>);

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
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    /// Record a span's fields once, at creation, so events inside it can carry
    /// them. Redaction happens here, on the same path as event fields — a span
    /// field is no less capable of holding a secret.
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        // Already stored by the sibling layer. Inserting twice panics
        // (`Extensions::insert` asserts the slot is empty), which is how the
        // first version of this took down a live `run` on startup.
        if ext.get_mut::<SpanFields>().is_some() {
            return;
        }
        // `true`: store everything, filter at render. See [`SpanFields`].
        let mut collector = FieldCollector::new(true);
        attrs.record(&mut collector);
        // A span's `message` field is dropped rather than stored: the event's
        // message is the line's prose, and letting a span supply one would
        // either fight it or print twice. Spans are named, not messaged.
        ext.insert(SpanFields(collector.fields));
    }

    /// Fields added after creation (`span.record(…)`) land here. Without this
    /// the capture would silently cover only what was passed to the macro —
    /// the kind of partial coverage that reads as working.
    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else { return };
        let mut collector = FieldCollector::new(true);
        values.record(&mut collector);
        let mut ext = span.extensions_mut();
        if let Some(SpanFields(existing)) = ext.get_mut::<SpanFields>() {
            // Idempotent across the two layers: both record the same values,
            // so the second pass overwrites with what is already there.
            existing.extend(collector.fields);
        } else {
            ext.insert(SpanFields(collector.fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Prompt/payload fields are only ever logged at debug+ (convention in
        // ai-docs/development/logging-conventions.md), so a stray `info!(prompt=…)`
        // cannot leak the body even with `log_prompts = true`.
        let allow_prompts = self.log_prompts
            && matches!(*meta.level(), tracing::Level::DEBUG | tracing::Level::TRACE);
        let mut collector = FieldCollector::new(allow_prompts);
        event.record(&mut collector);
        // Outermost span first, then inward, then the event's own fields last:
        // the nearest name for a key wins, which is what a reader assumes.
        let mut fields = Map::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(SpanFields(stored)) = span.extensions().get::<SpanFields>() {
                    for (k, v) in stored {
                        // The prompt policy is applied here, per layer, not at
                        // store time — see [`SpanFields`].
                        if is_prompt_field(k) && !allow_prompts {
                            continue;
                        }
                        fields.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        fields.extend(collector.fields);
        collector.fields = fields;
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

    /// **Two layers, one subscriber — the shape production actually runs.**
    ///
    /// `logging::init` installs a JSON file layer *and* a human stderr layer.
    /// Span extensions are per-span and shared by every layer, so a layer that
    /// stores into them unconditionally panics on the second insert
    /// (`Extensions::insert` asserts the slot is empty). The first version of
    /// span-field rendering did exactly that and took down a live `run` at
    /// startup — while every test here passed, because they all registered a
    /// single layer.
    ///
    /// Capture both streams, so this cannot regress into a one-layer test.
    fn capture_two_layers(emit: impl FnOnce()) -> (String, String) {
        let json_buf = Arc::new(Mutex::new(Vec::new()));
        let human_buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default()
            .with(RedactingLayer::new(
                BufWriter(json_buf.clone()),
                LogFormat::Json,
                false,
                false,
            ))
            .with(RedactingLayer::new(
                BufWriter(human_buf.clone()),
                LogFormat::Human,
                false,
                false,
            ));
        with_default(subscriber, emit);
        (
            String::from_utf8(json_buf.lock().unwrap().clone()).unwrap(),
            String::from_utf8(human_buf.lock().unwrap().clone()).unwrap(),
        )
    }

    #[test]
    fn two_layers_share_one_span_without_panicking() {
        let (json, human) = capture_two_layers(|| {
            let span = tracing::info_span!("plugin_rpc", plugin = "slack", method = "task/submit");
            let _g = span.enter();
            tracing::info!(outcome = "ok", "plugin rpc finished");
        });
        // Both streams must carry the span's fields — storing once must not
        // mean only one layer can read them.
        let doc: Value = serde_json::from_str(json.trim()).unwrap();
        assert_eq!(doc["plugin"], "slack", "{json}");
        assert_eq!(doc["method"], "task/submit");
        assert!(human.contains("plugin=slack"), "{human}");
        assert!(human.contains("method=task/submit"), "{human}");
    }

    /// #497 follow-up: an event inside a span carries the span's fields.
    ///
    /// This is the whole reason the instrumentation exists — `plugin rpc
    /// finished elapsed_ms=12 outcome=ok` reached production without naming
    /// the plugin or the method, because the layer only ever rendered an
    /// event's *own* fields and silently dropped everything the span carried.
    #[test]
    fn an_event_carries_the_fields_of_its_spans() {
        let out = capture(false, || {
            let span =
                tracing::info_span!("plugin_rpc", plugin = "slack", method = "task/dispatch");
            let _g = span.enter();
            tracing::info!(outcome = "ok", "plugin rpc finished");
        });
        let doc: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(doc["plugin"], "slack", "{out}");
        assert_eq!(doc["method"], "task/dispatch", "{out}");
        assert_eq!(doc["outcome"], "ok");
        assert_eq!(doc["message"], "plugin rpc finished");
    }

    /// Nested spans compose, and the **nearest** value for a key wins — an
    /// inner span (or the event) refining an outer one must not be shadowed by
    /// the outer value.
    #[test]
    fn the_nearest_value_for_a_key_wins() {
        let out = capture(false, || {
            let outer = tracing::info_span!("outer", plugin = "slack", scope = "outer");
            let _o = outer.enter();
            let inner = tracing::info_span!("inner", scope = "inner");
            let _i = inner.enter();
            tracing::info!(scope = "event", "done");
        });
        let doc: Value = serde_json::from_str(out.trim()).unwrap();
        // The event is nearest, so it wins over both spans…
        assert_eq!(doc["scope"], "event", "{out}");
        // …while a key only the outer span sets still comes through.
        assert_eq!(doc["plugin"], "slack");
    }

    /// Span fields go through redaction too. A span is no less capable of
    /// carrying a secret than an event, and this layer's contract is that the
    /// stream is redacted **by construction**.
    #[test]
    fn span_fields_are_redacted_like_event_fields() {
        let out = capture(false, || {
            let span = tracing::info_span!("auth", token = "xoxb-super-secret-value");
            let _g = span.enter();
            tracing::info!("in the span");
        });
        assert!(
            !out.contains("xoxb-super-secret-value"),
            "a span field must not reach the stream unredacted: {out}"
        );
    }

    /// The human format gets the span fields too — it is the stream a person
    /// actually reads while a run is live, which is where the gap was found.
    #[test]
    fn the_human_format_shows_span_fields() {
        let out = capture_as(LogFormat::Human, false, || {
            let span = tracing::info_span!("plugin_rpc", plugin = "herdr");
            let _g = span.enter();
            tracing::info!(outcome = "timeout", "plugin rpc finished");
        });
        assert!(out.contains("plugin=herdr"), "{out}");
        assert!(out.contains("outcome=timeout"), "{out}");
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
