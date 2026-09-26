//! The vocabulary of `events.detail` (#766).
//!
//! Every transition row in the audit log carries a JSON `detail` whose `kind`
//! names what caused it. This module is the one place that says which kinds
//! exist and which fields each one carries; writers hand an [`EventDetail`] to
//! the state store instead of assembling JSON by hand.
//!
//! The log is also read back **as control input**, not only by humans: the
//! dispatch retry budget counts `auto_retry` rows (#492), and a restart
//! recovers the agent's artifact from the `publish_artifact` of the latest
//! `BeginPublish` (#133). That is why the shape is typed.
//!
//! # The stored bytes are a contract
//!
//! Databases in the field already hold rows written by `serde_json::json!`,
//! and every reader of those rows must keep working after an upgrade. So:
//!
//! - **Serialize through [`serde_json::Value`]** — use
//!   [`to_json`](EventDetail::to_json), never `serde_json::to_string` on the
//!   enum. A `Value` object keeps its keys sorted (this workspace does not
//!   enable `preserve_order`), which is the form every existing row is in and
//!   the one `task export` promises to reproduce byte for byte. Serializing
//!   the enum directly would emit fields in declaration order instead.
//! - **An absent value is `null`, not a missing key.** Every `Option` field
//!   below was written as `null` by the old `json!` code, so none of them uses
//!   `skip_serializing_if`. On the way back in they are *required*
//!   (`deserialize_with = "Option::deserialize"`), because serde would
//!   otherwise accept a missing key as `None` and an untagged shape would
//!   match a row it did not write.
//! - **One `kind` may have several shapes** (`dispatch` records a start, a
//!   re-attach and a failure). A variant holds an untagged enum of its shapes,
//!   each with `deny_unknown_fields`, so a row matches exactly the shape whose
//!   key set it has. The golden tests below pin every shape to the bytes the
//!   old code produced.
//! - **A kind with a single shape ignores keys it does not know.** The
//!   strictness above exists only to tell a kind's shapes apart; applied to
//!   `AutoRetry` it would make an older binary stop counting the retries a
//!   newer one recorded with one more field, and quietly reset the budget.
//!   Reading a row is not validating it — the golden tests pin what the
//!   writers produce.
//!
//! A kind this build does not know deserializes as [`EventDetail::Unknown`],
//! and a row that fits no shape fails to deserialize; readers treat both as
//! "not the row I am looking for" rather than as an error, because an audit
//! row written by another version must never stop the engine.
//!
//! Notes (`detail` rows keyed by `note`, #407) are a separate vocabulary with
//! their own writer and reader in the state store, and are not modelled here.

use plugin_protocol::methods::AgentState;
use serde::{Deserialize, Serialize};

/// What caused an `events` row: the typed form of its `detail` column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventDetail {
    /// A task was ingested by polling its source.
    Ingested,
    /// A task was pushed in through `task/submit` (0.1.6).
    Submitted,
    /// An operator ran a `totsuka task …` command.
    Cli(Cli),
    /// An operator control request reached the running engine.
    Control {
        /// The command, as the operator would have typed it.
        command: String,
    },
    /// Repository selection could not settle on one repository.
    RepoSelect {
        /// Why.
        reason: String,
    },
    /// A dispatch started, re-attached, or failed.
    Dispatch(Dispatch),
    /// The engine requeued a failed dispatch on its own (#492).
    /// `StateDb::auto_retry_streak` counts these to enforce the bound.
    AutoRetry {
        /// This attempt's number, from 1.
        attempt: u32,
        /// The bound in force when it was written.
        limit: u32,
    },
    /// Another member claimed the task first.
    ClaimLost {
        /// Who holds it, when the source says.
        #[serde(deserialize_with = "Option::deserialize")]
        holder: Option<String>,
    },
    /// The source refused this member's claim.
    ClaimForbidden,
    /// A finished conversation was requeued with a new instruction.
    Reopen(Reopen),
    /// The agent IDE reported a state change.
    AgentState(AgentStateChange),
    /// Crash recovery re-attached to a session and synced the state machine.
    Recovery {
        /// Always `true`: only a successful attach writes this.
        attached: bool,
        /// The state the agent reported on attach.
        agent_state: AgentState,
    },
    /// A hook signal showed the agent at work.
    HookStart(HookStart),
    /// A hook reported completion of an escalated task (verification off).
    HookComplete {
        /// The accumulated agent output, persisted for restart recovery.
        #[serde(deserialize_with = "Option::deserialize")]
        publish_artifact: Option<String>,
    },
    /// The agent self-reported completion (human verification).
    SelfReport {
        /// The accumulated agent output, persisted for restart recovery.
        #[serde(deserialize_with = "Option::deserialize")]
        publish_artifact: Option<String>,
    },
    /// A hook `Stop` parked or failed the task.
    Hook {
        /// The marker's reason, if it gave one.
        #[serde(deserialize_with = "Option::deserialize")]
        reason: Option<String>,
    },
    /// The agent asked a question while escalated.
    QuestionPending {
        /// The question, if the agent IDE passed it on.
        #[serde(deserialize_with = "Option::deserialize")]
        reason: Option<String>,
    },
    /// The task was escalated to a human (D-02/D-03).
    Escalate {
        /// Why.
        reason: String,
        /// A pane snapshot, when the agent IDE supports one (R-10).
        #[serde(deserialize_with = "Option::deserialize")]
        diagnostics: Option<String>,
    },
    /// The output policy published the task, or failed to.
    Publish(Publish),
    /// A read-only workflow left a side effect behind (#410).
    ReadOnlyViolation {
        /// What was found.
        reason: String,
    },
    /// The agent plugin running the task crashed.
    PluginCrash {
        /// The plugin's name.
        plugin: String,
    },
    /// A kind this build does not know — written by another version.
    /// Deserialize-only: no writer produces it.
    #[serde(other)]
    Unknown,
}

/// The shapes of [`EventDetail::Cli`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Cli {
    /// `task verify --fail`, which records the operator's reason.
    WithReason {
        /// The command.
        command: String,
        /// The operator's reason (empty when none was given).
        reason: String,
    },
    /// Every other command.
    Plain {
        /// The command.
        command: String,
    },
}

/// The shapes of [`EventDetail::Dispatch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Dispatch {
    /// A fresh session was started.
    Started {
        /// The agent plugin.
        plugin: String,
        /// The session it started.
        session_id: String,
    },
    /// A retry re-attached to the previous session.
    Reattached {
        /// The session re-attached to.
        reused_session: String,
        /// The agent plugin.
        plugin: String,
    },
    /// The dispatch failed.
    Failed {
        /// Why.
        reason: String,
    },
}

/// The shapes of [`EventDetail::Reopen`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Reopen {
    /// A message for a different workflow handed the conversation over.
    Handoff {
        /// Always `workflow_handoff`.
        cause: String,
        /// Which workflow it left and entered.
        workflow: WorkflowHandoff,
        /// The delivery that caused it.
        message_key: String,
    },
    /// A new message arrived for a finished conversation.
    Message {
        /// The delivery that caused it.
        message_key: String,
    },
    /// The engine found unsent messages on a finished conversation.
    Cause {
        /// Why.
        cause: String,
    },
}

/// The workflow change recorded by [`Reopen::Handoff`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowHandoff {
    /// The workflow the conversation was in.
    pub from: String,
    /// The workflow it moved to.
    pub to: String,
}

/// The shapes of [`EventDetail::AgentState`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum AgentStateChange {
    /// The `BeginPublish` transition, which persists the artifact.
    WithArtifact {
        /// The reported state.
        state: AgentState,
        /// The accumulated agent output, persisted for restart recovery.
        #[serde(deserialize_with = "Option::deserialize")]
        publish_artifact: Option<String>,
    },
    /// Every other transition.
    Plain {
        /// The reported state.
        state: AgentState,
    },
}

/// The shapes of [`EventDetail::HookStart`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum HookStart {
    /// Any sign of the agent at work started a `Dispatched` task (#790).
    Signal {
        /// The hook event that did it.
        event: String,
    },
    /// A completion self-report was the first signal.
    Plain {},
}

/// The shapes of [`EventDetail::Publish`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Publish {
    /// The output policy succeeded.
    Succeeded {
        /// The policy that ran.
        policy: String,
        /// The pull request it opened, if any.
        #[serde(deserialize_with = "Option::deserialize")]
        pr_url: Option<String>,
    },
    /// The output policy failed, or the workflow is gone.
    Failed {
        /// Why.
        reason: String,
    },
}

impl EventDetail {
    /// The stored form: the JSON the `detail` column holds.
    ///
    /// Goes through [`serde_json::Value`] so the keys come out sorted — see
    /// the module docs for why that is a contract, not a style.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_value(self).map(|v| v.to_string())
    }

    /// The `kind` this detail is stored under, for log lines.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ingested => "ingested",
            Self::Submitted => "submitted",
            Self::Cli(_) => "cli",
            Self::Control { .. } => "control",
            Self::RepoSelect { .. } => "repo_select",
            Self::Dispatch(_) => "dispatch",
            Self::AutoRetry { .. } => "auto_retry",
            Self::ClaimLost { .. } => "claim_lost",
            Self::ClaimForbidden => "claim_forbidden",
            Self::Reopen(_) => "reopen",
            Self::AgentState(_) => "agent_state",
            Self::Recovery { .. } => "recovery",
            Self::HookStart(_) => "hook_start",
            Self::HookComplete { .. } => "hook_complete",
            Self::SelfReport { .. } => "self_report",
            Self::Hook { .. } => "hook",
            Self::QuestionPending { .. } => "question_pending",
            Self::Escalate { .. } => "escalate",
            Self::Publish(_) => "publish",
            Self::ReadOnlyViolation { .. } => "read_only_violation",
            Self::PluginCrash { .. } => "plugin_crash",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    /// Every shape a writer produces, next to the bytes the pre-#766
    /// `serde_json::json!` code stored for the same values. These literals
    /// are the contract with rows already on disk: change one and an upgrade
    /// silently misreads them.
    fn golden() -> Vec<(EventDetail, &'static str)> {
        vec![
            (EventDetail::Ingested, r#"{"kind":"ingested"}"#),
            (EventDetail::Submitted, r#"{"kind":"submitted"}"#),
            (
                EventDetail::Cli(Cli::Plain {
                    command: s("task retry"),
                }),
                r#"{"command":"task retry","kind":"cli"}"#,
            ),
            (
                EventDetail::Cli(Cli::WithReason {
                    command: s("task verify --fail"),
                    reason: s("wrong file"),
                }),
                r#"{"command":"task verify --fail","kind":"cli","reason":"wrong file"}"#,
            ),
            (
                EventDetail::Control {
                    command: s("task cancel"),
                },
                r#"{"command":"task cancel","kind":"control"}"#,
            ),
            (
                EventDetail::RepoSelect {
                    reason: s("two candidates"),
                },
                r#"{"kind":"repo_select","reason":"two candidates"}"#,
            ),
            (
                EventDetail::Dispatch(Dispatch::Started {
                    plugin: s("herdr"),
                    session_id: s("s-1"),
                }),
                r#"{"kind":"dispatch","plugin":"herdr","session_id":"s-1"}"#,
            ),
            (
                EventDetail::Dispatch(Dispatch::Reattached {
                    reused_session: s("s-1"),
                    plugin: s("herdr"),
                }),
                r#"{"kind":"dispatch","plugin":"herdr","reused_session":"s-1"}"#,
            ),
            (
                EventDetail::Dispatch(Dispatch::Failed {
                    reason: s("timed out"),
                }),
                r#"{"kind":"dispatch","reason":"timed out"}"#,
            ),
            (
                EventDetail::AutoRetry {
                    attempt: 1,
                    limit: 3,
                },
                r#"{"attempt":1,"kind":"auto_retry","limit":3}"#,
            ),
            (
                EventDetail::ClaimLost {
                    holder: Some(s("alice")),
                },
                r#"{"holder":"alice","kind":"claim_lost"}"#,
            ),
            (
                EventDetail::ClaimLost { holder: None },
                r#"{"holder":null,"kind":"claim_lost"}"#,
            ),
            (EventDetail::ClaimForbidden, r#"{"kind":"claim_forbidden"}"#),
            (
                EventDetail::Reopen(Reopen::Handoff {
                    cause: s("workflow_handoff"),
                    workflow: WorkflowHandoff {
                        from: s("design"),
                        to: s("implement"),
                    },
                    message_key: s("m-2"),
                }),
                r#"{"cause":"workflow_handoff","kind":"reopen","message_key":"m-2","workflow":{"from":"design","to":"implement"}}"#,
            ),
            (
                EventDetail::Reopen(Reopen::Message {
                    message_key: s("m-2"),
                }),
                r#"{"kind":"reopen","message_key":"m-2"}"#,
            ),
            (
                EventDetail::Reopen(Reopen::Cause {
                    cause: s("messages_arrived_while_working"),
                }),
                r#"{"cause":"messages_arrived_while_working","kind":"reopen"}"#,
            ),
            (
                EventDetail::AgentState(AgentStateChange::Plain {
                    state: AgentState::WaitingInput,
                }),
                r#"{"kind":"agent_state","state":"waiting_input"}"#,
            ),
            (
                EventDetail::AgentState(AgentStateChange::Plain {
                    state: AgentState::Failed,
                }),
                r#"{"kind":"agent_state","state":"failed"}"#,
            ),
            (
                EventDetail::AgentState(AgentStateChange::WithArtifact {
                    state: AgentState::Done,
                    publish_artifact: Some(s("line one\nline two")),
                }),
                r#"{"kind":"agent_state","publish_artifact":"line one\nline two","state":"done"}"#,
            ),
            (
                EventDetail::AgentState(AgentStateChange::WithArtifact {
                    state: AgentState::Done,
                    publish_artifact: None,
                }),
                r#"{"kind":"agent_state","publish_artifact":null,"state":"done"}"#,
            ),
            (
                EventDetail::Recovery {
                    attached: true,
                    agent_state: AgentState::Running,
                },
                r#"{"agent_state":"running","attached":true,"kind":"recovery"}"#,
            ),
            (
                EventDetail::HookStart(HookStart::Signal {
                    event: s("heartbeat"),
                }),
                r#"{"event":"heartbeat","kind":"hook_start"}"#,
            ),
            (
                EventDetail::HookStart(HookStart::Plain {}),
                r#"{"kind":"hook_start"}"#,
            ),
            (
                EventDetail::HookComplete {
                    publish_artifact: Some(s("out")),
                },
                r#"{"kind":"hook_complete","publish_artifact":"out"}"#,
            ),
            (
                EventDetail::SelfReport {
                    publish_artifact: None,
                },
                r#"{"kind":"self_report","publish_artifact":null}"#,
            ),
            (
                EventDetail::Hook {
                    reason: Some(s("marker said so")),
                },
                r#"{"kind":"hook","reason":"marker said so"}"#,
            ),
            (
                EventDetail::Hook { reason: None },
                r#"{"kind":"hook","reason":null}"#,
            ),
            (
                EventDetail::QuestionPending {
                    reason: Some(s("which branch?")),
                },
                r#"{"kind":"question_pending","reason":"which branch?"}"#,
            ),
            (
                EventDetail::Escalate {
                    reason: s("stuck"),
                    diagnostics: None,
                },
                r#"{"diagnostics":null,"kind":"escalate","reason":"stuck"}"#,
            ),
            (
                EventDetail::Publish(Publish::Succeeded {
                    policy: s("source"),
                    pr_url: None,
                }),
                r#"{"kind":"publish","policy":"source","pr_url":null}"#,
            ),
            (
                EventDetail::Publish(Publish::Failed {
                    reason: s("gh failed"),
                }),
                r#"{"kind":"publish","reason":"gh failed"}"#,
            ),
            (
                EventDetail::ReadOnlyViolation {
                    reason: s("a branch was created"),
                },
                r#"{"kind":"read_only_violation","reason":"a branch was created"}"#,
            ),
            (
                EventDetail::PluginCrash { plugin: s("herdr") },
                r#"{"kind":"plugin_crash","plugin":"herdr"}"#,
            ),
        ]
    }

    #[test]
    fn every_shape_serializes_to_the_bytes_the_old_writers_stored() {
        for (detail, stored) in golden() {
            assert_eq!(detail.to_json().unwrap(), stored, "{detail:?}");
        }
    }

    #[test]
    fn every_stored_shape_reads_back_as_the_shape_that_wrote_it() {
        // Also what keeps the untagged shapes apart: a row matching the wrong
        // shape would compare unequal here.
        for (detail, stored) in golden() {
            let read: EventDetail = serde_json::from_str(stored).unwrap();
            assert_eq!(read, detail, "{stored}");
            assert_eq!(read.to_json().unwrap(), stored, "round trip of {stored}");
        }
    }

    #[test]
    fn kind_names_the_stored_tag() {
        for (detail, stored) in golden() {
            let v: serde_json::Value = serde_json::from_str(stored).unwrap();
            assert_eq!(v["kind"], detail.kind(), "{stored}");
        }
    }

    /// Deliberately lenient (see the module docs): an extra key on a kind with
    /// one shape is another version's addition, not a different row.
    #[test]
    fn a_single_shape_kind_ignores_a_key_it_does_not_know() {
        let read: EventDetail =
            serde_json::from_str(r#"{"kind":"auto_retry","attempt":2,"limit":3,"backoff_ms":500}"#)
                .unwrap();
        assert_eq!(
            read,
            EventDetail::AutoRetry {
                attempt: 2,
                limit: 3
            }
        );
    }

    #[test]
    fn a_kind_from_another_version_reads_as_unknown() {
        let read: EventDetail =
            serde_json::from_str(r#"{"kind":"from_the_future","x":1}"#).unwrap();
        assert_eq!(read, EventDetail::Unknown);
    }

    #[test]
    fn a_row_that_fits_no_shape_does_not_deserialize() {
        for row in [
            // missing a field
            r#"{"kind":"auto_retry","attempt":1}"#,
            // an Option field is required, only nullable
            r#"{"kind":"hook"}"#,
            // a key no shape of this kind has
            r#"{"kind":"dispatch","plugin":"herdr"}"#,
            r#"{"kind":"hook_start","event":"x","extra":1}"#,
        ] {
            assert!(serde_json::from_str::<EventDetail>(row).is_err(), "{row}");
        }
    }
}
