//! `agent-ide-orca`: a totsuka agent_ide plugin that adapts the Orchestrator's
//! JSON-RPC 2.0 plugin protocol (NDJSON, [`plugin_protocol`]) onto the **orca
//! CLI** (F-30〜F-38). Protocol-side it is identical to the herdr plugin; the
//! orca-specific launch/state means are hidden inside (F-32).
//!
//! # Why the CLI
//!
//! orca exposes no public socket/REST API — wrapping the `orca` CLI with
//! `--json` is the officially recommended integration. The execution unit is a
//! git worktree; the agent runs as a TUI process in that worktree's terminal.
//! See [`docs/references/orca-cli-control.md`].
//!
//! # Method mapping ([`agent::OrcaAgent`])
//!
//! - `task/dispatch` → `orca worktree create --agent … --prompt … --json`
//! - `task/cancel`   → `orca worktree rm --worktree id:<id> --force --json`
//! - `session/attach`→ `orca worktree show --worktree id:<id> --json` (weak
//!   absorption: orca keeps the Agent Session History + `claude --resume`)
//! - `state/subscribe` → poll `orca worktree ps --json` (state dots), pacing
//!   with `orca terminal wait --for tui-idle`, mapping to normalized state.
//!
//! # Claude Code / orca caveats (F-32/F-33)
//!
//! State is a coarse 3-value derived from OSC "state dots" (a status-line hook),
//! not a structured stream; `failed` has no native signal (derived from an
//! abnormal terminal exit / timeout); and orca has no structured plan/preview
//! API, so `design_preview` is **not** declared.

pub mod agent;
pub mod cli;
pub mod config;
pub mod error;
pub mod server;
pub mod state;
