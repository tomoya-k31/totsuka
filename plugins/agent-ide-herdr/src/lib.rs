//! `agent-ide-herdr`: a totsuka agent_ide plugin that adapts the Orchestrator's
//! JSON-RPC 2.0 plugin protocol (NDJSON, [`plugin_protocol`]) onto herdr's
//! Socket API (F-30〜F-38). The reference agent_ide implementation.
//!
//! # Two protocols, one adapter
//!
//! - **Orchestrator side** ([`server`]): JSON-RPC 2.0 over stdio — `task/dispatch`,
//!   `task/cancel`, `session/attach`, `state/subscribe` → streamed
//!   `state/notification`.
//! - **herdr side** ([`transport`]): NDJSON over a Unix socket (**not** JSON-RPC),
//!   dotted methods (`workspace.create`, `agent.start`, `agent.send`,
//!   `pane.read`, `events.subscribe`, …), one request per connection.
//!
//! [`agent::HerdrAgent`] holds the mapping logic behind a
//! [`transport::HerdrTransport`] seam so dispatch/attach/cancel/subscribe are
//! testable against a fake herdr socket.
//!
//! # Screen-manifest agent caveats (F-32/F-35, #124)
//!
//! herdr's default target Claude Code has **no lifecycle authority**: its
//! idle/working/blocked/done come from herdr's screen-manifest scraping, and
//! `waiting_input` questions are a best-effort screen extraction, not a
//! structured signal. The screen is also unusable as the answer artifact
//! (no scrollback, TUI chrome), so the agent's final output comes from its own
//! transcript ([`transcript`]). See [`docs/references/herdr-socket-api.md`].

pub mod agent;
pub mod config;
pub mod error;
pub mod server;
pub mod state;
pub mod transcript;
pub mod transport;
