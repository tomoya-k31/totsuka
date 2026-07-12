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
//!   dotted methods (`workspace.create`, `pane.send_text`, `agent.send`,
//!   `events.subscribe`, `session.snapshot`, …) correlated by `id`.
//!
//! [`agent::HerdrAgent`] holds the mapping logic behind a
//! [`transport::HerdrTransport`] seam so dispatch/attach/cancel/subscribe are
//! testable against a fake herdr socket.
//!
//! # Claude Code caveat (F-32/F-35)
//!
//! herdr's target agent Claude Code has **no lifecycle authority**: its
//! idle/working/blocked/done come from herdr's screen-manifest scraping, and
//! `waiting_input` questions are a best-effort scrollback extraction, not a
//! structured signal. See [`docs/references/herdr-socket-api.md`].

pub mod agent;
pub mod config;
pub mod error;
pub mod server;
pub mod state;
pub mod transport;
