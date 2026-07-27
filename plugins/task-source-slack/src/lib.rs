//! `task-source-slack`: a totsuka task_source plugin that turns Slack mentions
//! of the operator into tasks over a stdio JSON-RPC 2.0 server (F-02/F-51).
//!
//! The binary ([`main`](../main.rs)) is a thin stdio loop over
//! [`server::Server`]; all logic lives here behind a
//! [`transport::SlackTransport`] seam so it is testable against recorded
//! responses without a network. `initialize` runs the TokenGuard: it verifies
//! the user token via `auth.test` and that the token's identity matches
//! `target_user_id`, so a wrong or revoked token stops the plugin at startup
//! with actionable guidance instead of failing later mid-flow.

pub mod approval;
pub mod config;
pub mod draft;
pub mod error;
pub mod llm;
pub mod mention;
pub mod notify;
pub mod persist;
pub mod pipeline;
pub mod repo_resolver;
pub mod server;
pub mod slack_api;
pub mod socket_mode;
pub mod transport;
