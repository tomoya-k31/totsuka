//! `task-source-notion`: a totsuka task_source plugin exposing a Notion
//! database as tasks over a stdio JSON-RPC 2.0 server (F-02/F-03/F-51).
//!
//! The binary ([`main`](../main.rs)) is a thin stdio loop over [`server::Server`];
//! all logic lives here behind a [`transport::NotionTransport`] seam so it is
//! testable against recorded responses without a network. A configurable
//! [property map](config::PropertyMap) normalizes any database shape onto the
//! shared [`plugin_protocol::Task`] schema (F-01).

pub mod blocks;
pub mod client;
pub mod config;
pub mod error;
pub mod server;
pub mod template;
pub mod transport;
