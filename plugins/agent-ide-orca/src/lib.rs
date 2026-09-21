//! `agent-ide-orca`: a totsuka agent_ide plugin that adapts the Orchestrator's
//! JSON-RPC 2.0 plugin protocol (NDJSON, [`plugin_protocol`]) onto the **orca
//! CLI** (F-30〜F-38). Protocol-side it is the herdr plugin's twin — the same
//! methods and capabilities — and the orca-specific means are hidden inside
//! (F-32).
//!
//! # Why the CLI
//!
//! orca exposes no public socket/REST API — wrapping the `orca` CLI with
//! `--json` is the officially recommended integration. See
//! `ai-docs/references/orca-cli-control.md`.
//!
//! # Method mapping ([`agent::OrcaAgent`])
//!
//! - `task/dispatch`  → `orca terminal create --worktree path:<worktree>
//!   --command "exec sh -c … <tool_launch>"` (the env through a FIFO, #744),
//!   `terminal wait --for tui-idle`,
//!   `terminal send --wait-submit`
//! - `task/cancel` / `session/release` → `orca terminal close --tab`
//! - `session/attach` → `orca terminal show` (+ `worktree ps` for the state)
//! - `session/focus`  → `orca terminal switch`
//! - `session/list`   → `orca terminal list`, joined to the worktrees whose orca
//!   comment carries the `totsuka ` marker (`orca worktree list --repo`)
//! - `diagnostics/snapshot` → `orca terminal read --screen`
//! - `state/subscribe` → an exit deadman on `orca terminal wait --for exit`
//!
//! # Completion is hook-based, as under herdr
//!
//! The agent is launched from the Orchestrator's `tool_launch`, which carries
//! the hook settings and env, so Claude Code reports completion out-of-band
//! through its hooks (#131). orca's own coarse state dots are only read to
//! tell recovery what a re-attached agent is doing.

pub mod agent;
pub mod cli;
pub mod config;
pub mod error;
pub mod handoff;
pub mod launch;
pub mod server;
pub mod state;
