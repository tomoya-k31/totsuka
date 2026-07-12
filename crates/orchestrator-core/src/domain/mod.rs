//! Domain layer: pure types and the task state machine.

pub mod state;

pub use state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
