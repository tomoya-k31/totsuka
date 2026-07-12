//! Plugin-host implementation of the [`AgentSession`] port (F-37, §5.3).
//!
//! Wraps the already-launched plugins the run loop (#63) owns and speaks
//! `session/attach` + `state/subscribe` to them. It does not launch plugins
//! itself: the caller decides which plugins to start (F-58) and hands them here
//! as a name→[`Plugin`] map.

use std::collections::HashMap;
use std::future::Future;

use plugin_protocol::method;
use plugin_protocol::methods::{SessionAttachParams, SessionAttachResult, StateSubscribeParams};
use serde_json::Value;

use crate::adapters::plugin_host::Plugin;
use crate::ports::agent_session::{AgentSession, AgentSessionError, AttachOutcome};

/// Re-attaches to sessions over the JSON-RPC plugin host.
pub struct PluginAgentSession<'a> {
    /// Launched plugins keyed by instance name (owned by the run loop, #63).
    plugins: &'a HashMap<String, Plugin>,
}

impl<'a> PluginAgentSession<'a> {
    /// Wrap the plugins the caller has already launched.
    pub fn new(plugins: &'a HashMap<String, Plugin>) -> Self {
        Self { plugins }
    }
}

impl AgentSession for PluginAgentSession<'_> {
    fn attach(
        &self,
        plugin: &str,
        session_id: &str,
    ) -> impl Future<Output = Result<AttachOutcome, AgentSessionError>> + Send {
        // Resolve the plugin synchronously so the returned future borrows only
        // the handle, not `self`/`plugin`.
        let handle = self.plugins.get(plugin);
        let plugin = plugin.to_string();
        let session_id = session_id.to_string();
        async move {
            let handle = handle.ok_or_else(|| AgentSessionError::Unavailable {
                plugin: plugin.clone(),
                reason: "not launched".to_string(),
            })?;

            let result: SessionAttachResult = handle
                .call(
                    method::SESSION_ATTACH,
                    &SessionAttachParams {
                        session_id: session_id.clone(),
                    },
                )
                .await
                .map_err(|e| AgentSessionError::Attach {
                    plugin: plugin.clone(),
                    reason: e.to_string(),
                })?;

            if !result.attached {
                return Ok(AttachOutcome::Lost);
            }

            // Re-establish the state/log stream so recovery resumes streaming
            // (F-38). A failure here means the re-attach did not fully succeed,
            // so surface it rather than pretend the session is live.
            let _: Value = handle
                .call(
                    method::STATE_SUBSCRIBE,
                    &StateSubscribeParams { session_id },
                )
                .await
                .map_err(|e| AgentSessionError::Attach {
                    plugin: plugin.clone(),
                    reason: format!("re-subscribe failed: {e}"),
                })?;

            Ok(AttachOutcome::Attached(result.state))
        }
    }
}
