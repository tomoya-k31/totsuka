//! Domain layer: pure types and the task state machine.

pub mod state;
pub mod workflow;

pub use state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
pub use workflow::{
    OutcomeAction, Severity, Trigger, Workflow, WorkflowIssue, match_workflow, validate_workflows,
};
