//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/notifier-macos.toml` as JSON (F-64), and the workflow × event filter
//! (F-92).

use std::collections::HashMap;

use plugin_protocol::methods::NotifierEvent;
use serde::Deserialize;

/// The notification delivery backend (F-94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// AppleScript `display notification` — always available, but a click
    /// opens Script Editor (the osascript owner), never the task's pane.
    #[default]
    Osascript,
    /// `terminal-notifier` — clickable: `-execute` runs `totsuka focus` and
    /// `-activate` brings the GUI terminal to the front (click-to-focus).
    TerminalNotifier,
}

/// macOS notifier settings.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NotifierConfig {
    /// Which delivery backend to use. Defaults to `osascript` for backward
    /// compatibility; set `terminal_notifier` for click-to-focus (F-94).
    #[serde(default)]
    pub backend: Backend,
    /// The `osascript` executable (name on PATH or absolute path).
    #[serde(default = "default_osascript")]
    pub osascript_bin: String,
    /// The `terminal-notifier` executable (name on PATH or absolute path).
    #[serde(default = "default_terminal_notifier")]
    pub terminal_notifier_bin: String,
    /// Bundle id of the GUI app a click brings to the front (`-activate`),
    /// e.g. `org.alacritty`. Environment-specific; unset ⇒ no `-activate`
    /// (only the pane focus via `click_command` remains).
    #[serde(default)]
    pub activate_bundle_id: Option<String>,
    /// Shell command template a click runs (`-execute`). `{task_id}` is
    /// replaced with the notification's task id, **shell-quoted** (injection
    /// safety). Empty ⇒ no `-execute`.
    #[serde(default = "default_click_command")]
    pub click_command: String,
    /// The delivery filter (F-92).
    #[serde(default)]
    pub filter: Filter,
}

/// Per-event on/off toggles. `None` means "inherit" (unspecified).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventToggles {
    /// Toggle for `waiting_input` (F-35).
    #[serde(default)]
    pub waiting_input: Option<bool>,
    /// Toggle for `done`.
    #[serde(default)]
    pub done: Option<bool>,
    /// Toggle for `failed`.
    #[serde(default)]
    pub failed: Option<bool>,
    /// Toggle for `pending` (F-14).
    #[serde(default)]
    pub pending: Option<bool>,
    /// Toggle for `escalated` (#131 D-02/D-03: a task handed to a human).
    #[serde(default)]
    pub escalated: Option<bool>,
    /// Toggle for `verification_pending` (#131 D-01: awaiting `task verify`).
    #[serde(default)]
    pub verification_pending: Option<bool>,
}

impl EventToggles {
    /// The toggle for `event`, if specified.
    fn get(&self, event: NotifierEvent) -> Option<bool> {
        match event {
            NotifierEvent::WaitingInput => self.waiting_input,
            NotifierEvent::Done => self.done,
            NotifierEvent::Failed => self.failed,
            NotifierEvent::Pending => self.pending,
            NotifierEvent::Escalated => self.escalated,
            NotifierEvent::VerificationPending => self.verification_pending,
        }
    }
}

/// The workflow × event delivery filter (F-92). Precedence: a per-workflow
/// toggle wins over the global toggle, which wins over the default (all on).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// Global per-event toggles applied to every workflow.
    #[serde(default)]
    pub events: EventToggles,
    /// Per-workflow overrides, keyed by workflow name.
    #[serde(default)]
    pub workflows: HashMap<String, EventToggles>,
}

impl Filter {
    /// Whether an event from `workflow` should be delivered (F-92): the most
    /// specific toggle wins; unspecified everywhere means deliver (all on).
    pub fn allows(&self, workflow: Option<&str>, event: NotifierEvent) -> bool {
        if let Some(name) = workflow
            && let Some(toggles) = self.workflows.get(name)
            && let Some(enabled) = toggles.get(event)
        {
            return enabled;
        }
        self.events.get(event).unwrap_or(true)
    }
}

fn default_osascript() -> String {
    "osascript".to_string()
}

fn default_terminal_notifier() -> String {
    "terminal-notifier".to_string()
}

fn default_click_command() -> String {
    "totsuka focus {task_id}".to_string()
}

impl NotifierConfig {
    /// The notification binary to run: the configured value, or the standard
    /// `osascript` when unset/empty (so an explicit empty string still works).
    pub fn osascript_bin(&self) -> &str {
        if self.osascript_bin.is_empty() {
            "osascript"
        } else {
            &self.osascript_bin
        }
    }

    /// The `terminal-notifier` binary, with the same empty-string fallback as
    /// [`osascript_bin`](Self::osascript_bin).
    pub fn terminal_notifier_bin(&self) -> &str {
        if self.terminal_notifier_bin.is_empty() {
            "terminal-notifier"
        } else {
            &self.terminal_notifier_bin
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> NotifierConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn unspecified_filter_allows_everything() {
        let cfg = parse(serde_json::json!({}));
        assert_eq!(cfg.osascript_bin, "osascript");
        for event in [
            NotifierEvent::WaitingInput,
            NotifierEvent::Done,
            NotifierEvent::Failed,
            NotifierEvent::Pending,
            NotifierEvent::Escalated,
            NotifierEvent::VerificationPending,
        ] {
            assert!(cfg.filter.allows(Some("wf"), event));
            assert!(cfg.filter.allows(None, event));
        }
    }

    #[test]
    fn hook_event_toggles_are_filter_eligible() {
        // The two hook-epic events (#131) are now first-class: a global toggle
        // and a per-workflow override both take effect.
        let cfg = parse(serde_json::json!({
            "filter": {
                "events": { "escalated": false, "verification_pending": false },
                "workflows": { "reply": { "escalated": true } }
            }
        }));
        // Global off silences both everywhere by default.
        assert!(!cfg.filter.allows(Some("impl"), NotifierEvent::Escalated));
        assert!(!cfg.filter.allows(None, NotifierEvent::VerificationPending));
        // A per-workflow override re-enables escalated for `reply`.
        assert!(cfg.filter.allows(Some("reply"), NotifierEvent::Escalated));
        // …without touching verification_pending, which still follows the global off.
        assert!(
            !cfg.filter
                .allows(Some("reply"), NotifierEvent::VerificationPending)
        );
    }

    #[test]
    fn global_event_toggle_applies() {
        let cfg = parse(serde_json::json!({
            "filter": { "events": { "done": false } }
        }));
        assert!(!cfg.filter.allows(Some("wf"), NotifierEvent::Done));
        assert!(cfg.filter.allows(Some("wf"), NotifierEvent::Failed));
    }

    #[test]
    fn workflow_override_beats_global() {
        let cfg = parse(serde_json::json!({
            "filter": {
                "events": { "done": false },
                "workflows": { "release": { "done": true }, "chore": { "failed": false } }
            }
        }));
        // release re-enables done despite the global off.
        assert!(cfg.filter.allows(Some("release"), NotifierEvent::Done));
        // an unlisted workflow inherits the global off.
        assert!(!cfg.filter.allows(Some("other"), NotifierEvent::Done));
        // chore turns failed off; done still follows the global off.
        assert!(!cfg.filter.allows(Some("chore"), NotifierEvent::Failed));
        assert!(!cfg.filter.allows(Some("chore"), NotifierEvent::Done));
    }

    #[test]
    fn backend_defaults_to_osascript_for_backward_compat() {
        let cfg = parse(serde_json::json!({}));
        assert_eq!(cfg.backend, Backend::Osascript);
        assert_eq!(cfg.terminal_notifier_bin(), "terminal-notifier");
        assert!(cfg.activate_bundle_id.is_none());
        assert_eq!(cfg.click_command, "totsuka focus {task_id}");
    }

    #[test]
    fn terminal_notifier_backend_parses_with_its_settings() {
        let cfg = parse(serde_json::json!({
            "backend": "terminal_notifier",
            "terminal_notifier_bin": "/opt/homebrew/bin/terminal-notifier",
            "activate_bundle_id": "org.alacritty",
            "click_command": "totsuka focus {task_id}",
        }));
        assert_eq!(cfg.backend, Backend::TerminalNotifier);
        assert_eq!(
            cfg.terminal_notifier_bin(),
            "/opt/homebrew/bin/terminal-notifier"
        );
        assert_eq!(cfg.activate_bundle_id.as_deref(), Some("org.alacritty"));
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let err = serde_json::from_value::<NotifierConfig>(
            serde_json::json!({ "backend": "carrier_pigeon" }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("carrier_pigeon"), "got {err}");
    }

    #[test]
    fn osascript_bin_falls_back_when_empty() {
        let cfg = parse(serde_json::json!({ "osascript_bin": "" }));
        assert_eq!(cfg.osascript_bin(), "osascript");
        let cfg = parse(serde_json::json!({ "osascript_bin": "/opt/osascript" }));
        assert_eq!(cfg.osascript_bin(), "/opt/osascript");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err =
            serde_json::from_value::<NotifierConfig>(serde_json::json!({ "typo": 1 })).unwrap_err();
        assert!(err.to_string().contains("typo"), "got {err}");
    }
}
