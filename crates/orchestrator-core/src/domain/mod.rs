//! Domain layer: pure types and the task state machine.

pub mod signal;
pub mod state;
pub mod workflow;

pub use signal::{AgentSignal, InvalidJobId, JobId, SignalEvent, SignalSource, StopStatus};
pub use state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
pub use workflow::{
    OutcomeAction, Severity, Trigger, Workflow, WorkflowIssue, match_workflow, validate_workflows,
};
