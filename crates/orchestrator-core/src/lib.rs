//! totsuka orchestrator core.
//!
//! The crate has a hexagonal centre and the application that drives it
//! (#762). Only the centre is layered, and only its direction is enforced.
//!
//! # The hexagonal centre
//!
//! - [`domain`]: pure domain types and the task state machine. It knows
//!   neither `config.toml` nor any concrete implementation: config values
//!   reach it already interpreted, through [`config::interpret`].
//! - [`ports`]: trait boundaries (`AgentSession`, `Clock`, `GitRunner`,
//!   `SecretStore`, ...) that adapters implement.
//! - [`adapters`]: concrete implementations (JSON-RPC plugin host, SQLite,
//!   git, the hook socket, ...). [`platform`] holds the OS-dependent ones.
//!
//! `domain` and `ports` must not refer to `config` or `adapters`;
//! `scripts/arch-lint.sh` checks that (`core-layer`, ADR-0102).
//!
//! # The application
//!
//! Everything else assembles the centre and may use adapters directly — that
//! is the direction wiring goes, not a leak:
//!
//! - [`run`] (the main loop), [`scheduler`], [`recovery`], [`worktree`],
//!   [`repo_select`] and [`plugins`] drive tasks through adapters.
//! - [`tool`] (the AI tool CLI an agent *is*), [`agent_prereqs`] (what that
//!   agent needs installed beside it), [`hooks`] and [`prompts`] shape what
//!   runs in the pane.
//!
//! # Foundations
//!
//! [`config`] (loading, validation, and the interpretation into domain
//! values), [`paths`], [`logging`], [`template`] and [`terminal`] are used
//! from every layer above the centre.

pub mod adapters;
pub mod agent_prereqs;
pub mod config;
pub mod domain;
pub mod hooks;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod plugins;
pub mod ports;
pub mod prompts;
pub mod recovery;
pub mod repo_select;
pub mod run;
pub mod scheduler;
pub mod task_control;
pub mod template;
pub mod terminal;
pub mod tool;
pub mod worktree;
