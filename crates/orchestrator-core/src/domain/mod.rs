//! Domain layer: pure types and the task state machine.

pub mod event_detail;
pub mod signal;
pub mod state;
pub mod task;
pub mod workflow;

pub use event_detail::EventDetail;
pub use signal::{AgentSignal, InvalidJobId, JobId, SignalEvent, SignalSource, StopStatus};
pub use state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
pub use task::{SourceTaskId, Task, TaskId};
pub use workflow::{
    CleanupPolicy, OutcomeAction, OutputPolicy, Profile, Severity, Trigger, VerificationMode,
    Workflow, WorkflowIssue, WorkflowMode, validate_workflows,
};
