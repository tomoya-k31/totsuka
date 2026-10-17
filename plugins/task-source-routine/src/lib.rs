//! `task-source-routine`: a totsuka task_source plugin that turns the
//! routines in `<config dir>/routines/<host>/*.md` into tasks on a cron
//! schedule (#918, ADR-0117).
//!
//! Each routine is one Markdown file: a frontmatter of `repo`, `schedule`
//! and friends, and a body that is the prompt. The plugin keeps no state of
//! its own — a task's id is `<name>@<slot>`, so re-submitting a time slot is
//! the Orchestrator's `duplicate`, and a run of a routine still in progress
//! makes the next one `busy` through [`plugin_protocol::Task::series`].

pub mod config;
pub mod server;
