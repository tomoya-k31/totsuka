//! Typed method descriptors: the one place that pairs a method name with its
//! `params` and `result` types (#757, ADR-0101).
//!
//! Each O→P request is a zero-sized type implementing [`Method`], so a caller
//! writes `plugin.request::<TaskDispatch>(&params)` and the compiler checks
//! that `params` is a [`TaskDispatchParams`] and that the answer is read as a
//! [`TaskDispatchResult`]. Before this, every call site paired a
//! [`method`] constant with a turbofish by hand, and a mismatch built fine and
//! only surfaced as a serde error at runtime.
//!
//! Only O→P requests are described here. `shutdown` (a best-effort request
//! with no params that nobody waits on), the `notify` notification and the
//! P→O requests (`task/submit`, `task/lookup`) are not — yet: a descriptor
//! lives in this crate precisely so the plugin side can adopt the same table
//! later.
//!
//! The wire format is untouched: [`Method::NAME`] is the existing [`method`]
//! constant, which stays because plugins match on it.

use serde::Serialize;
use serde::de::{DeserializeOwned, IgnoredAny};

use crate::method;
use crate::methods::{
    ConfigValidateParams, ConfigValidateResult, DiagnosticsSnapshotParams,
    DiagnosticsSnapshotResult, InitializeParams, InitializeResult, ResultPublishParams,
    SessionAttachParams, SessionAttachResult, SessionFocusParams, SessionFocusResult,
    SessionListParams, SessionListResult, SessionReleaseParams, SessionReleaseResult,
    StateSubscribeParams, TaskCancelParams, TaskClaimParams, TaskClaimResult, TaskDispatchParams,
    TaskDispatchResult, TaskUpdateStatusParams,
};

/// A JSON-RPC request method: its name and the types it carries each way.
pub trait Method {
    /// The wire method name (one of the [`method`] constants).
    const NAME: &'static str;
    /// The `params` the caller sends.
    type Params: Serialize;
    /// The `result` the plugin answers with.
    ///
    /// [`IgnoredAny`] for the methods whose result the protocol leaves
    /// undefined: plugins answer `null`, `{}` or anything else there without
    /// violating it, so `()` (which accepts only `null`) would turn a valid
    /// answer into an error. `IgnoredAny` accepts all of them and says in the
    /// type that nobody reads it.
    type Result: DeserializeOwned;
}

macro_rules! methods {
    ($($(#[$doc:meta])* $ty:ident = $name:ident, $params:ty => $result:ty;)*) => {$(
        $(#[$doc])*
        pub enum $ty {}

        impl Method for $ty {
            const NAME: &'static str = method::$name;
            type Params = $params;
            type Result = $result;
        }
    )*};
}

methods! {
    /// `initialize` (O→P).
    Initialize = INITIALIZE, InitializeParams => InitializeResult;
    /// `config/validate` (O→P, F-59).
    ConfigValidate = CONFIG_VALIDATE, ConfigValidateParams => ConfigValidateResult;
    /// `task/update_status` (O→P, F-84).
    TaskUpdateStatus = TASK_UPDATE_STATUS, TaskUpdateStatusParams => IgnoredAny;
    /// `task/claim` (O→P, #556).
    TaskClaim = TASK_CLAIM, TaskClaimParams => TaskClaimResult;
    /// `result/publish` (O→P, F-07).
    ResultPublish = RESULT_PUBLISH, ResultPublishParams => IgnoredAny;
    /// `task/dispatch` (O→P).
    TaskDispatch = TASK_DISPATCH, TaskDispatchParams => TaskDispatchResult;
    /// `task/cancel` (O→P).
    TaskCancel = TASK_CANCEL, TaskCancelParams => IgnoredAny;
    /// `session/attach` (O→P, F-37).
    SessionAttach = SESSION_ATTACH, SessionAttachParams => SessionAttachResult;
    /// `state/subscribe` (O→P). The stream itself arrives as
    /// `state/notification`s; the reply carries nothing.
    StateSubscribe = STATE_SUBSCRIBE, StateSubscribeParams => IgnoredAny;
    /// `diagnostics/snapshot` (O→P, R-10).
    DiagnosticsSnapshot = DIAGNOSTICS_SNAPSHOT, DiagnosticsSnapshotParams => DiagnosticsSnapshotResult;
    /// `session/focus` (O→P, F-94).
    SessionFocus = SESSION_FOCUS, SessionFocusParams => SessionFocusResult;
    /// `session/release` (O→P, #210).
    SessionRelease = SESSION_RELEASE, SessionReleaseParams => SessionReleaseResult;
    /// `session/list` (O→P, #211).
    SessionList = SESSION_LIST, SessionListParams => SessionListResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each descriptor names the method its types belong to. The types are
    /// checked by the compiler; the pairing with a name is the one thing it
    /// cannot see.
    #[test]
    fn descriptor_names_match_method_constants() {
        let pairs = [
            (Initialize::NAME, "initialize"),
            (ConfigValidate::NAME, "config/validate"),
            (TaskUpdateStatus::NAME, "task/update_status"),
            (TaskClaim::NAME, "task/claim"),
            (ResultPublish::NAME, "result/publish"),
            (TaskDispatch::NAME, "task/dispatch"),
            (TaskCancel::NAME, "task/cancel"),
            (SessionAttach::NAME, "session/attach"),
            (StateSubscribe::NAME, "state/subscribe"),
            (DiagnosticsSnapshot::NAME, "diagnostics/snapshot"),
            (SessionFocus::NAME, "session/focus"),
            (SessionRelease::NAME, "session/release"),
            (SessionList::NAME, "session/list"),
        ];
        for (name, wire) in pairs {
            assert_eq!(name, wire);
        }
    }
}
