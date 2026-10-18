//! [`DegradedReporter`]: tell the Orchestrator about a runtime problem the
//! plugin noticed, and that it is over (`plugin/degraded`, protocol 0.7.9,
//! ADR-0119).
//!
//! A notification, not a request: nothing waits for an answer, and an
//! Orchestrator that predates the method simply never reads it. The reporter
//! remembers what it last said per key and only writes on change, so a caller
//! can report the outcome of every call without flooding the host.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use plugin_protocol::jsonrpc::{Notification, to_line};
use plugin_protocol::method;
use plugin_protocol::methods::PluginDegradedParams;

use crate::runtime::Writer;

/// Reports and clears degradations over the shared [`Writer`]. Clonable; all
/// clones share one record of what was last sent.
#[derive(Debug, Clone)]
pub struct DegradedReporter {
    writer: Writer,
    /// `key` → the message last reported active. Absent = clear.
    sent: Arc<Mutex<HashMap<String, String>>>,
}

impl DegradedReporter {
    /// A reporter writing to `writer`.
    pub fn new(writer: Writer) -> Self {
        Self {
            writer,
            sent: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Report `key` as degraded with `message`. A no-op when that exact
    /// message is already the one reported.
    pub fn report(&self, key: &str, message: &str) {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        if sent.get(key).is_some_and(|m| m == message) {
            return;
        }
        sent.insert(key.to_string(), message.to_string());
        self.send(key, message, true);
    }

    /// Clear `key`. A no-op when it is not currently reported.
    pub fn clear(&self, key: &str) {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        if sent.remove(key).is_some() {
            self.send(key, "", false);
        }
    }

    fn send(&self, key: &str, message: &str, active: bool) {
        let params = PluginDegradedParams {
            key: key.to_string(),
            message: message.to_string(),
            active,
        };
        let line = serde_json::to_value(&params)
            .ok()
            .map(|params| Notification::new(method::PLUGIN_DEGRADED, Some(params)))
            .and_then(|note| to_line(&note).ok());
        if let Some(line) = line {
            // A closed writer means the host is gone; there is no one left
            // to tell.
            self.writer.send_line(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::sync::mpsc;

    fn reporter() -> (DegradedReporter, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (DegradedReporter::new(Writer::from_channel(tx)), rx)
    }

    fn params(line: &str) -> Value {
        let note: Value = serde_json::from_str(line).unwrap();
        assert_eq!(note["method"], "plugin/degraded");
        assert!(note.get("id").is_none(), "a notification has no id");
        note["params"].clone()
    }

    #[test]
    fn only_changes_are_sent() {
        let (r, mut rx) = reporter();
        r.clear("llm"); // nothing reported yet
        r.report("llm", "down");
        r.report("llm", "down"); // same message
        r.report("llm", "key rejected");
        r.clear("llm");
        r.clear("llm");

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(params(&line));
        }
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0]["message"], "down");
        assert_eq!(lines[0]["active"], true);
        assert_eq!(lines[1]["message"], "key rejected");
        assert_eq!(lines[2]["active"], false);
    }
}
