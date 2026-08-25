//! `task-source-github`: a totsuka task_source plugin exposing GitHub
//! Issues / ProjectsV2 as tasks over a stdio JSON-RPC 2.0 server (F-02/F-51).
//!
//! The binary ([`main`](../main.rs)) is a thin stdio loop over [`server::Server`];
//! all logic lives here behind a [`transport::GithubTransport`] seam so it is
//! testable against recorded responses without a network.

pub mod claim;
pub mod client;
pub mod config;
pub mod error;
pub mod server;
pub mod template;
pub mod transport;
