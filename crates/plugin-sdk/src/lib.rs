//! # totsuka plugin SDK
//!
//! Helpers for writing `task_source` and `agent_ide` plugins against the
//! [`plugin-protocol`](plugin_protocol) contract, so a plugin author writes
//! the source- or IDE-specific logic and nothing else ([ADR-0008]):
//!
//! - [`runtime`] — the stdio NDJSON loop with a **single writer task**, so
//!   replies and background [`task/submit`](plugin_protocol::method::TASK_SUBMIT)
//!   requests never interleave partial lines on stdout.
//! - [`dispatch`] — the JSON-RPC dispatch boilerplate ([`Reply`],
//!   [`TaskSourceHandler`], parse helpers, [`not_initialized`]).
//! - [`agent_ide`] — [`AgentIdeHandler`]: the agent_ide twin of
//!   [`TaskSourceHandler`], whose [`AgentIdeServer`] also owns the
//!   ACK-then-notifications order of `state/subscribe` (F-38).
//! - [`template`] — single-pass `{placeholder}` substitution for a source's
//!   configurable prompts and instructions.
//! - [`prompt`] — [`compose_prompt`]: the prompt an agent_ide hands the
//!   agent for a `task/dispatch`.
//! - [`submit`] — [`SubmitClient`]: push one task to the Orchestrator and
//!   await the persist-before-ack outcome, retrying retryable errors with
//!   backoff. Final acks (`accepted`/`duplicate`/`rejected`) are never
//!   retried.
//! - [`poll`] — [`poll_loop`]: a jittered, non-overlapping fetch→submit
//!   timer for polling-style sources (GitHub, Notion) migrating off the
//!   deprecated `tasks/fetch`.
//! - [`trigger`] — [`unknown_trigger_keys`]: reject `[[workflows]].trigger`
//!   keys the source does not read, so a typo fails startup instead of
//!   silently widening the trigger (#574); [`unknown_exclude_keys`] does the
//!   same inside `trigger.exclude`, and [`one_or_many`] reads the
//!   string-or-array values (ADR-0091).
//! - [`assignee`] — [`AssigneeFilter`]: the `trigger.assignee` condition
//!   (`@me` / `@none` / `@any` / a login / a list), which replaces the
//!   plugin-wide F-08 gate so a workflow can leave the unassigned to people
//!   (#572).
//! - [`watch`] — [`WatchTrigger`]: the channel watch trigger
//!   (`trigger = { channel = … }`, #616) for chat sources — parsing, the
//!   operator-plus-`from` author gate, the rename check, and
//!   [`BackfillLimits`]: the window a source's startup backfill recovers.
//!
//! Out of scope by design: HTTP clients, LLM helpers, and config schemas —
//! those stay source-specific.
//!
//! [ADR-0008]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0008-task-submit-push-ingestion.md

pub mod agent_ide;
pub mod assignee;
pub mod dispatch;
pub mod lookup;
pub mod poll;
pub mod prompt;
pub mod runtime;
pub mod submit;
pub mod template;
pub mod trigger;
pub mod watch;

pub use agent_ide::{AgentIdeHandler, AgentIdeServer};
pub use assignee::{AssigneeFilter, check as check_assignee_triggers};
pub use dispatch::{
    Reply, TaskSourceHandler, TaskSourceServer, not_initialized, parse_params, request_id,
};
pub use lookup::{Lookup, LookupClient};
pub use poll::poll_loop;
pub use prompt::compose_prompt;
pub use runtime::{LineHandler, Stdio, Writer, serve};
pub use submit::{SubmitClient, SubmitOutcome, Submitter};
pub use trigger::{one_or_many, unknown_exclude_keys, unknown_trigger_keys};
pub use watch::{BackfillLimits, WatchTrigger, resolve as resolve_watch_triggers};
