//! `task-source-discord`: a totsuka task_source plugin that turns posts in a
//! watched Discord channel into tasks, over a stdio JSON-RPC 2.0 server.
//!
//! # Why this source is deliberately small
//!
//! The Slack source's centre of gravity is answering a mention **as the
//! operator**, with a draft and an approval gate in front of it. None of that
//! is possible here: Discord forbids automating a human account, so an app
//! posts as its bot and nothing else. Rebuilding an approval flow around a
//! bot's voice would keep the machinery and lose the reason for it.
//!
//! So this source does one thing — channel watch in, result out — and the
//! result goes out under the bot's name with no approval step. See
//! [ADR-0068] for the decision and what was rejected.
//!
//! [ADR-0068]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0068-channel-watch-trigger.md

pub mod config;
pub mod discord_api;
pub mod error;
pub mod gateway;
pub mod http;
pub mod pipeline;
pub mod run;
pub mod server;
pub mod transport;
pub mod watch;
