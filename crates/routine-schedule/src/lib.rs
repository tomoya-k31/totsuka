//! Routines (#918, ADR-0117): the `routines/<host>/<name>.md` file format
//! and the five-field cron schedules in it.
//!
//! A leaf crate shared by the `task-source-routine` plugin, which submits a
//! routine when it is due, and `totsuka routine list`, which shows what the
//! plugin will do — so the two cannot read a file differently.

pub mod cron;
pub mod routine;

pub use cron::Schedule;
pub use routine::{Routine, load_dir, parse, parse_duration};
