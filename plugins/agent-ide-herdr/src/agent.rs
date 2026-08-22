//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.
//!
//! Protocol facts this adapter is written against (herdr 0.7.5 / protocol 17,
//! verified live in [ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md),
//! mirrored in `ai-docs/references/herdr-socket-api.md`):
//! - the agent CLI is launched with `agent.start {name, kind, pane_id, args}`
//!   into a pane the **caller** supplies. `kind` picks the executable, `name` is
//!   an identifier (`[a-z][a-z0-9_-]{0,31}`, unique among live agents), and
//!   `cwd`/`env`/`argv` are not accepted at all
//! - the hook environment (`TOTSUKA_JOB_ID`, … from
//!   [`ToolLaunchSpec::env`](plugin_protocol::methods::ToolLaunchSpec::env))
//!   therefore rides on `workspace.create`, whose `env` herdr applies to the
//!   root pane — which is the pane the agent is started in
//! - a freshly created pane is **not immediately usable**: its shell is still
//!   starting, and `agent.start` types the launch command into it regardless.
//!   There is no readiness signal to poll — `pane.process_info` shows the shell
//!   from the moment it is *spawned*, which is not the moment it starts
//!   *accepting input* — so the call is retried. The race surfaces in four
//!   shapes and all four are one bug (#387, #391): `agent_pane_busy` and
//!   `timeout` on `agent.start`, and a start that is accepted while the agent
//!   never becomes addressable, which only `agent.prompt` can see — as
//!   `agent_not_ready`, or as `agent_not_found` when herdr registered nothing
//!   at all. Keystrokes typed into a shell that was not reading are **lost,
//!   not queued**, so waiting longer never helps; only re-issuing
//!   `agent.start` does
//! - the prompt **cannot** ride in `args`: a multi-line prompt passed that way
//!   is never submitted, and every task body here is multi-line. It goes in
//!   through `agent.prompt {target, text, wait}`, which types and submits in one
//!   call and reports `agent_prompt_stalled` when the agent does not react
//! - events arrive as `{event, data}` envelopes whose kind separator is
//!   inconsistent (`pane.agent_status_changed` but `pane_exited`), so kinds are
//!   compared normalized
//! - `agent.start` does **not** split (it did before protocol 17). The pane
//!   arrangement is imposed by `pane.split {direction, ratio, target_pane_id,
//!   cwd, env}` before the agent is started, which is what
//!   `HerdrAgent::apply_layout` does (#356)
//!
//! # Completion detection is now hook-based (0.1.3, #131 / R-07)
//!
//! Task completion is reported out-of-band by Claude Code's Stop/SessionEnd
//! hooks (POST to the Orchestrator's UDS), **not** by this plugin's state
//! stream. So the screen-manifest completion path (mapping `working → idle`,
//! confirming a debounced `done`, scraping the answer off the pane/transcript,
//! extracting a `waiting_input` question) is **removed**. The state stream is
//! reduced to a **deadman**: it subscribes only to `pane.exited` and reports
//! `Failed` on an abnormal exit — the hook already reported a normal end. This
//! reduction is unconditional (it holds even when the launch carries no hook
//! env). An orchestrator older than 0.1.3 would therefore never learn of
//! completion; `initialize` used to warn about that, and since 0.4.0 (#411) the
//! manifest's `>=0.2.3` floor makes such an orchestrator unable to launch this
//! plugin at all — the launcher refuses it before `initialize` (F-54).

use plugin_protocol::methods::{
    AgentState, DiagnosticsSnapshotResult, ExecutionMode, NotReleased, SessionAttachResult,
    SessionFocusResult, SessionInfo, SessionListResult, SessionReleaseParams, SessionReleaseResult,
    StateNotification, TaskDispatchParams, TaskDispatchResult,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, map_agent_status};
use crate::transport::{HerdrTransport, SUBSCRIPTION_CLOSED_EVENT, call_typed, to_params};
use crate::wire::request;
use crate::wire::result::{
    AgentStatus, PaneInfo, PaneInfoEnvelope, PaneListEnvelope, PaneReadEnvelope, WorkspaceInfo,
    WorkspaceListEnvelope,
};

/// The marker that says a herdr container belongs to totsuka, followed by the
/// task's `source_task_id`.
///
/// **It is set on the `WorkspaceInfo.label`, never on a `PaneInfo.label`**
/// (#416). herdr keeps the two apart: `workspace.create { label }` and
/// `workspace.rename` write the former, and only `pane.rename` writes the
/// latter — which totsuka never calls. Reading it back off panes is what made
/// `session/list` return an empty array against every real herdr, and with it
/// [ADR-0013](../../../ai-docs/decisions/adr-0013-orphan-pane-detection.md)'s
/// orphan-pane detection, since 0.2.2.
///
/// [`crate::agent::HerdrAgent::list_sessions`] still reports it as the
/// session's `label`, because `doctor` strips this prefix to recover the
/// source task id.
const OWNED_LABEL_PREFIX: &str = "totsuka ";

/// How many screen lines are read when extracting text from a pane.
const SCREEN_LINES: u32 = 200;

/// The statuses that mean "the agent read the prompt", for `agent.prompt`'s
/// `wait` and the `agent.wait` confirmation behind it.
///
/// `working` alone would be a race on a turn short enough to settle before
/// herdr samples again; `blocked` and `done` are the settled states that also
/// mean it read the prompt.
const SETTLED_OR_WORKING: [request::AgentStatus; 3] = [
    request::AgentStatus::Working,
    request::AgentStatus::Blocked,
    request::AgentStatus::Done,
];

/// How long `agent.prompt` is given to observe the agent reacting, in
/// milliseconds ([ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-5).
///
/// herdr's own floor is 5s for the *first* state change; this is the outer
/// bound on reaching a settled state. Generous on purpose — the cost of being
/// wrong is a dispatch that fails on an agent that was merely slow.
const PROMPT_WAIT_MS: u64 = 120_000;

/// How long a single `agent.start` attempt is given to detect the CLI, in
/// milliseconds.
///
/// **Deliberately short, and shorter than `request_timeout_secs` (30s).** This
/// used to be 120s, on the theory that a pane which has not reached its shell
/// prompt just needs longer. Measured live (#387), that theory is wrong twice
/// over:
///
/// - Waiting does not help. A start issued immediately after
///   `workspace.create` failed after the full 120s, and the pane was
///   **empty the whole time** — the launch command had been typed into a shell
///   that was not accepting input, so it was swallowed rather than queued.
///   Re-issuing `agent.start` on that same pane then succeeded in ~3s.
/// - 120s was unreachable anyway: the transport gives up at
///   `request_timeout_secs`, so the client aborted at 30s and the extra 90s
///   only ever existed on paper.
///
/// So an attempt is now cheap and repeated, rather than long and singular.
const AGENT_START_TIMEOUT_MS: u64 = 15_000;

/// How long [`confirm_submission`](HerdrAgent::confirm_submission) watches a
/// stalled prompt before giving up, in milliseconds.
///
/// Shorter than [`PROMPT_WAIT_MS`] on purpose: herdr has already spent its own
/// 5s floor seeing nothing, so this is the "it was merely slow" allowance, not
/// a fresh full-length wait. A prompt that never landed costs this much before
/// the dispatch fails, so it is bounded rather than generous.
const PROMPT_CONFIRM_MS: u64 = 60_000;

/// How long the whole start-and-prompt handshake keeps being re-attempted.
///
/// Covers every shape the shell-readiness race takes (#387): `agent_pane_busy`
/// and `timeout` on `agent.start`, and `agent_not_ready` on `agent.prompt`.
///
/// Raised from 60s because a single `agent.start` attempt can now cost
/// [`AGENT_START_TIMEOUT_MS`], so 60s bought only a handful of tries. The
/// workflow's own `timeout_secs` (default 1800s, 900s in the E2E config) is the
/// outer bound that matters, so there is room. Past this the refusal is
/// reported as-is — at that point "it is still starting" has stopped being a
/// plausible reading.
const STARTUP_RETRY_BUDGET: Duration = Duration::from_secs(180);

/// How long `agent.prompt` tolerates `agent_not_ready` before the dispatch
/// goes back and re-issues `agent.start`.
///
/// Measured live, a genuinely-still-launching CLI clears in ~4s. An agent whose
/// keystrokes were swallowed never clears at all — the old code asked for the
/// full 60s budget and then failed the dispatch, which is exactly the 40%
/// failure rate #387 reports. Short enough that the doomed case is cheap,
/// generous enough that the slow-but-real case is not cut off.
const PROMPT_READY_WINDOW: Duration = Duration::from_secs(15);

/// How long to wait between those attempts. Short enough that the usual
/// few-second window costs a few probes, long enough not to spin.
const STARTUP_RETRY_POLL: Duration = Duration::from_millis(500);

/// How many times a dispatch goes back and re-issues `agent.start` because the
/// prompt found no agent to talk to.
///
/// A count, not just [`STARTUP_RETRY_BUDGET`], because the two failures that
/// send us back there cost very different amounts of time. `agent_not_ready`
/// is waited out inside [`PROMPT_READY_WINDOW`] first, so a cycle takes
/// seconds; `agent_not_found` comes back immediately, so a purely time-bounded
/// loop would re-launch the CLI as fast as herdr could answer for the whole
/// budget. Three is past the measured need — every live occurrence cleared on
/// the first re-issue (#387, #391) — while keeping a herdr that answers this
/// way forever from turning one dispatch into minutes of thrash.
const MAX_AGENT_RESTARTS: u32 = 3;

/// The herdr agent_ide adapter, generic over its [`HerdrTransport`].
pub struct HerdrAgent<T> {
    client: T,
    config: HerdrConfig,
}

impl<T: HerdrTransport> HerdrAgent<T> {
    /// A new adapter over `client` using `config`.
    pub fn new(client: T, config: HerdrConfig) -> Self {
        Self { client, config }
    }

    /// Dispatch a task (F-31/F-37): create a herdr workspace on the worktree,
    /// start the agent CLI in it (plan mode when asked, F-36), lay the
    /// workspace out as configured (#356), submit the task prompt, and return a
    /// `(pane_id, agent_session_id)` re-attach handle as the session id.
    ///
    /// Returns only once the agent has actually started working, so a caller
    /// that subscribes afterwards can trust the pane's status (see
    /// [`start_state_stream`](Self::start_state_stream)).
    pub async fn dispatch(
        &self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, HerdrError> {
        let (program, args, env) = resolve_launch(&params)?;

        // A workspace per task keeps agent panes out of the operator's own
        // workspaces (there is no "start session" method; the workspace is the
        // container).
        //
        // `env` goes here and **only** here (protocol 17): `agent.start` no
        // longer takes one. It still reaches the agent because herdr applies a
        // workspace's env to its root pane, which is the pane the agent is
        // started in (D-4). A pane made by `pane.split` inherits nothing, which
        // is why the companion shell never sees `TOTSUKA_HOOK_TOKEN`.
        let create_params = request::WorkspaceCreateParams {
            cwd: Some(params.worktree_path.clone()),
            label: Some(format!("{OWNED_LABEL_PREFIX}{}", params.task.id)),
            env: env
                .as_ref()
                .and_then(|env| serde_json::from_value(env.clone()).ok())
                .unwrap_or_default(),
            focus: None,
        };
        let created: CreatedWorkspace =
            call_typed(&self.client, "workspace.create", &create_params).await?;
        let workspace = NewWorkspace::from_created(created);

        // From here on the workspace exists, so every failure path has to take
        // it back down: a failed dispatch reports no session id, which leaves
        // the Orchestrator no handle to cancel with — the pane and its CLI
        // process would run until the operator noticed them (and `task retry`
        // would strand another one).
        //
        // `report_identity` is not one of those paths — it cannot fail the
        // dispatch, by design (#417) — but it belongs inside the boundary
        // rather than before it, because it is the first thing that talks to a
        // workspace that now needs tearing down.
        self.report_identity(&params, &workspace).await;
        let started = self
            .start_agent(&params, &workspace, program, args)
            .await
            .inspect_err(|_| self.abandon(&workspace.id))?;
        Ok(started)
    }

    /// The part of [`dispatch`](Self::dispatch) that runs with a workspace
    /// allocated: start the CLI, submit the prompt, and build the handle.
    ///
    /// **The agent runs in the workspace's own root pane** (protocol 17,
    /// [ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-4).
    /// `agent.start` no longer creates a pane — it takes one that is already at
    /// an interactive shell prompt — and that suits the hook environment: only
    /// the root pane inherits `workspace.create`'s `env`, and the root pane is
    /// now exactly where the agent lands.
    async fn start_agent(
        &self,
        params: &TaskDispatchParams,
        workspace: &NewWorkspace,
        program: String,
        args: Vec<String>,
    ) -> Result<TaskDispatchResult, HerdrError> {
        let pane_id = workspace.root_pane_id.clone().ok_or_else(|| {
            // Fatal now, where a missing root pane used to only cost the
            // layout: without it there is no pane to start the agent in.
            HerdrError::InvalidResponse("workspace.create returned no root pane".into())
        })?;

        // Split BEFORE starting the agent, the reverse of protocol 16's order.
        // The agent's pane is the one being split off, so doing it first means
        // the CLI draws itself once, at its final size — and there is no
        // screen-matching left that a reflow could invalidate (D-5).
        self.apply_layout(&pane_id, &params.worktree_path).await;

        let start_params = to_params(
            "agent.start",
            &request::AgentStartParams {
                name: agent_name(&params.task.id),
                kind: resolve_kind(&self.config, &program),
                pane_id: pane_id.clone(),
                args,
                timeout_ms: Some(AGENT_START_TIMEOUT_MS),
            },
        )?;
        // Start and prompt share one budget, because they are two views of one
        // race (#387). `agent.start` types the launch command into the pane; if
        // the shell was not accepting input yet the keystrokes are swallowed,
        // and herdr reports that in whichever of three shapes it happens to
        // take — `agent_pane_busy`, `timeout`, or a start that is *accepted*
        // while the agent never becomes addressable. Only the last one is
        // visible from `agent.prompt`, so the prompt has to be able to send the
        // dispatch back to `agent.start` rather than keep asking an agent that
        // does not exist.
        let prompt = compose_prompt(params);
        let deadline = tokio::time::Instant::now() + STARTUP_RETRY_BUDGET;
        let mut restarts = 0;
        loop {
            let started = self
                .start_when_pane_is_ready(&pane_id, start_params.clone(), deadline)
                .await?;
            // herdr echoes the pane it was given; trusting our own value keeps
            // the handle well-defined even if a future response drops the field.
            debug_assert_eq!(
                started
                    .get("agent")
                    .and_then(|a| a.get("pane_id"))
                    .and_then(Value::as_str),
                Some(pane_id.as_str()),
            );

            match self.submit_prompt(&pane_id, &prompt, deadline).await {
                Ok(()) => break,
                // The CLI never actually launched, so there is nothing to
                // prompt and no amount of asking will change that. Re-issuing
                // `agent.start` is what clears it, and it is safe: herdr
                // registers no agent for a start that detected none, so the
                // task's name is still free (verified live — the same name
                // succeeded on re-issue).
                Err(e)
                    if restarts < MAX_AGENT_RESTARTS
                        && tokio::time::Instant::now() < deadline
                        && prompt_means_the_cli_never_started(params, &e) =>
                {
                    restarts += 1;
                    tracing::warn!(
                        pane_id, error = %e, restarts,
                        "the agent never became addressable; re-issuing agent.start rather \
                         than prompting a CLI that did not start"
                    );
                }
                Err(e) => return Err(resume_failure(params, e)),
            }
        }

        // The agent's own session id (for `claude --resume`) is reported by its
        // herdr integration hook during startup; by now it is normally there,
        // and an empty one only degrades resume.
        let agent_session_id = pane_record(&self.client, &pane_id)
            .await
            .ok()
            .and_then(|pane| agent_session_id(&pane).map(str::to_string))
            .unwrap_or_default();

        let handle = SessionHandle::new(pane_id, agent_session_id);
        Ok(TaskDispatchResult {
            session_id: handle.encode(),
        })
    }

    /// `agent.start`, waiting out a pane that has not reached its shell prompt.
    ///
    /// Protocol 17 hands `agent.start` the workspace's root pane moments after
    /// `workspace.create` made it, and herdr refuses a pane whose shell is
    /// still starting: `agent_pane_busy`. Measured live, a dispatch calling it
    /// ~1s after creation was refused while the same call seconds later
    /// succeeded.
    ///
    /// **Retried rather than predicted.** How long a shell takes to reach its
    /// prompt is the operator's rc files, not something this plugin can know,
    /// and herdr exposes no readiness signal to poll instead. `agent.start`
    /// *is* the readiness check, so it is asked again rather than
    /// second-guessed.
    ///
    /// `pane.process_info` is **not** that signal, though not for the reason
    /// this comment used to give. Under protocol 17 it is populated from
    /// ~10ms — `foreground_processes: [{argv0: "zsh", …}]` — so the old claim
    /// that it "reports `shell_pid: null` for the whole window" is stale
    /// (that field no longer exists). It is useless here for a different
    /// reason: it shows the shell from the moment it is *spawned*, which is
    /// not the moment it starts *accepting input*, and the gap between those
    /// two is precisely the race (#387).
    ///
    /// Two refusals are retried, because they are one race in two shapes:
    /// `agent_pane_busy` (herdr refused the pane) and `timeout` (herdr took
    /// the pane, typed into it, and never saw the CLI appear — the keystrokes
    /// went into a shell that was not reading yet). Every other failure — an
    /// unknown `kind`, `agent_name_taken` — means something that will not fix
    /// itself, and retrying would only delay the report.
    async fn start_when_pane_is_ready(
        &self,
        pane_id: &str,
        params: Value,
        deadline: tokio::time::Instant,
    ) -> Result<Value, HerdrError> {
        loop {
            match self.client.call("agent.start", params.clone()).await {
                Ok(started) => return Ok(started),
                Err(e) if e.is_pane_not_ready() || e.is_timeout_of("agent.start") => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    tracing::debug!(
                        pane_id,
                        error = %e,
                        "the pane was not ready to launch the CLI; retrying agent.start"
                    );
                    tokio::time::sleep(STARTUP_RETRY_POLL).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Arrange the task's workspace as `[layout]` asks (#356): unless
    /// `shell = false`, split a companion shell off the pane the agent is about
    /// to run in, at the configured direction and ratio.
    ///
    /// **There is no `pane.close` here any more** (protocol 17,
    /// [ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-4).
    /// Under protocol 16 `agent.start` made a *second* pane, leaving the
    /// workspace's initial shell stranded, and closing it was this method's
    /// first act. In 17 the agent runs in that initial pane, so there is
    /// nothing left over — one call, not two, and the slowest of the two
    /// (`pane.close`, measured at 23–25 ms) is the one that went away.
    ///
    /// **The new shell gets no `env`.** herdr's root pane inherits the
    /// workspace's, which for a hook-capable dispatch is the Orchestrator's
    /// hook environment — `TOTSUKA_HOOK_TOKEN` included. A pane a human types
    /// into is not where a bearer token belongs
    /// (`ai-docs/security/hook-security.md`), and a pane made by `pane.split`
    /// inherits nothing, so simply not passing `env` is what removes it.
    ///
    /// **Every failure is a warning, never an error.** The layout is
    /// decoration: a herdr that blips while drawing it must not lose a task
    /// that is otherwise ready to run.
    ///
    /// Focus is left alone deliberately: `pane.split` keeps focus on the pane
    /// it split from — the agent's — so it ends up focused without a
    /// `pane.focus` of our own.
    async fn apply_layout(&self, agent_pane_id: &str, cwd: &str) {
        let layout = &self.config.layout;
        if !layout.shell {
            return;
        }
        if let Err(e) = self
            .client
            .call(
                "pane.split",
                match to_params(
                    "pane.split",
                    &request::PaneSplitParams {
                        target_pane_id: Some(agent_pane_id.to_string()),
                        direction: layout.direction.as_wire(),
                        // herdr's `ratio` is the *split source's* share, and the
                        // source here is the agent's pane — so the configured
                        // agent share goes across unchanged.
                        ratio: Some(layout.ratio),
                        cwd: Some(cwd.to_string()),
                        focus: Some(false),
                        env: BTreeMap::new(),
                        workspace_id: None,
                    },
                ) {
                    Ok(params) => params,
                    Err(e) => {
                        tracing::warn!(pane_id = agent_pane_id, error = %e, "could not build the split");
                        return;
                    }
                },
            )
            .await
        {
            tracing::warn!(
                pane_id = agent_pane_id, error = %e,
                "could not add the companion shell pane; the agent runs full-screen"
            );
        }
    }

    /// Tell herdr which repository, task and mode this workspace is for, so
    /// the sidebar can say so (#417,
    /// [ADR-0039](../../../ai-docs/decisions/adr-0039-herdr-sidebar-identity.md)).
    ///
    /// Reported to the **workspace and its root pane both**: `$name` in a
    /// sidebar row resolves against workspace metadata in the spaces panel and
    /// against pane metadata in the agents panel, so one report only fixes one
    /// panel.
    ///
    /// # Placement
    ///
    /// Before `agent.start`, which is a retry loop of up to 180 seconds. After
    /// it, the rows would stay anonymous through exactly the window an
    /// operator is most likely to be looking at them. Two socket round trips
    /// at ~25 ms each ([ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md))
    /// is noise beside that.
    ///
    /// # Failure
    ///
    /// Warned about, never raised. Identity is decoration; a herdr that blips
    /// while recording it must not lose a task that is otherwise ready to run
    /// — the same rule [`apply_layout`](Self::apply_layout) follows.
    async fn report_identity(&self, params: &TaskDispatchParams, workspace: &NewWorkspace) {
        if !self.config.identity.enabled {
            return;
        }
        // `BTreeMap<String, Option<String>>` is herdr's own shape: a `null`
        // value **clears** that token rather than setting it to nothing. Every
        // value here is a `Some`; nothing in this plugin clears a token.
        let mut tokens: BTreeMap<String, Option<String>> = BTreeMap::from([
            ("task".to_string(), Some(token_value(&params.task.title))),
            (
                "mode".to_string(),
                Some(
                    match params.mode {
                        ExecutionMode::Plan => "plan",
                        ExecutionMode::Implement => "implement",
                    }
                    .to_string(),
                ),
            ),
        ]);
        // The machine identifier, and the one token that is **compared rather
        // than displayed** — so it goes across verbatim, never through
        // `token_value`. Collapsing whitespace or appending `…` would make a
        // pane that *is* ours fail its own check in `release`, and would have
        // `session/list` synthesise a label `doctor` cannot match against
        // `source_task_id`.
        //
        // An id longer than herdr keeps is **omitted**, not truncated: herdr
        // would cut it silently, and a cut machine identifier is worse than no
        // identifier at all — the label path is a correct fallback, a wrong id
        // is not.
        if !params.task.id.is_empty() && params.task.id.chars().count() <= TOKEN_VALUE_CHARS {
            tokens.insert(IDENTITY_TOKEN.to_string(), Some(params.task.id.clone()));
        } else {
            tracing::debug!(
                task_id = %params.task.id,
                "task id exceeds herdr's token limit; reporting no identity token \
                 (ownership falls back to the workspace label)"
            );
        }
        // Absent from an Orchestrator older than protocol 0.4.1. Omitted
        // rather than guessed: `$repo` renders empty, which is what the
        // sidebar snippet is written to tolerate.
        if let Some(repo) = &params.repo_name {
            tokens.insert("repo".to_string(), Some(token_value(repo)));
        }
        // The two reports take **different params types** (the pane one also
        // accepts `title` / `display_agent` / `state_labels`, which this plugin
        // never sets), so they are written out rather than shared through one
        // payload builder. What must not drift is the token map, and that is
        // the value both clone.
        let mut reported = true;
        match self
            .client
            .call(
                "workspace.report_metadata",
                to_params(
                    "workspace.report_metadata",
                    &request::WorkspaceReportMetadataParams {
                        workspace_id: workspace.id.clone(),
                        source: METADATA_SOURCE.to_string(),
                        tokens: tokens.clone(),
                        seq: None,
                        ttl_ms: None,
                    },
                )
                .unwrap_or(Value::Null),
            )
            .await
        {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(workspace_id = %workspace.id, error = %e, "could not report workspace identity");
                reported = false;
            }
        }
        match &workspace.root_pane_id {
            Some(pane_id) => {
                if let Err(e) = self
                    .client
                    .call(
                        "pane.report_metadata",
                        to_params(
                            "pane.report_metadata",
                            &request::PaneReportMetadataParams {
                                pane_id: pane_id.clone(),
                                source: METADATA_SOURCE.to_string(),
                                tokens: tokens.clone(),
                                agent: None,
                                applies_to_source: None,
                                clear_display_agent: None,
                                clear_state_labels: None,
                                clear_title: None,
                                display_agent: None,
                                seq: None,
                                state_labels: BTreeMap::new(),
                                title: None,
                                ttl_ms: None,
                            },
                        )
                        .unwrap_or(Value::Null),
                    )
                    .await
                {
                    tracing::warn!(pane_id, error = %e, "could not report pane identity");
                    reported = false;
                }
            }
            None => reported = false,
        }
        // **The gate is "is the marker readable back", not "did the calls
        // succeed".** Those come apart: a `task.id` too long (or empty) for a
        // token is skipped above while both reports still return `ok`. Renaming
        // then would produce the one container this whole design forbids — no
        // `totsuka ` label *and* no token — which `list_sessions` drops
        // entirely (so `doctor` can never see it) and `release` refuses (so its
        // pane leaks).
        let marked = tokens
            .get(IDENTITY_TOKEN)
            .and_then(Option::as_deref)
            .is_some_and(|task| !task.is_empty());
        // The marker that outlives a herdr restart (#432). Tokens do not: a
        // `herdr server live-handoff` keeps the workspace, its panes, the
        // running agent **and both labels**, and drops every metadata token.
        // Before this, a renamed workspace came out of a handoff with no token
        // and a human-readable label — no ownership marker at all — and
        // vanished from `session/list`.
        let pane_labeled = self.label_pane(params, workspace).await;
        if reported && marked && pane_labeled {
            self.rename_for_humans(params, workspace).await;
        }
    }

    /// Write the machine marker onto the **pane** as well, and report whether
    /// it is there (#432).
    ///
    /// # Why the pane and not just the workspace
    ///
    /// [`rename_for_humans`](Self::rename_for_humans) trades the workspace's
    /// machine label for a readable one, which is safe only while the tokens
    /// carry the identity instead. **A herdr restart breaks that trade**:
    /// `live-handoff` preserves labels and drops tokens (measured on herdr
    /// 0.7.5), so a renamed workspace loses its last marker and `doctor`'s
    /// orphan-pane detection goes blind to a pane that is still running.
    ///
    /// The pane label survives the same handoff (measured), and it is a
    /// *separate* field from the workspace label — `pane.rename` is the only
    /// thing that writes it, and until this function totsuka never called it,
    /// which is exactly the gap #416 found. Filling it lets the readable name
    /// and the machine marker stop competing for one field: the workspace
    /// label is for humans, the pane label is for us.
    ///
    /// [`list_sessions`](Self::list_sessions) already reads `PaneInfo.label`
    /// as one of its four ownership paths, so nothing there changes — the path
    /// simply stops being dead.
    ///
    /// # Why the rename waits on this
    ///
    /// Returning `false` keeps the workspace on its machine label. That is the
    /// same invariant #417 D4 already enforced, extended by one term: **never
    /// rename away the last marker.** A pane rename that herdr refuses costs
    /// the pretty name, not the ownership.
    ///
    /// Verbatim `task.id`, never through `token_value`: this label is compared
    /// (`strip_prefix` → `source_task_id`), not displayed. It does not reach
    /// the sidebar unless the operator puts the `pane` token in their `rows`,
    /// which the recommended snippet does not.
    async fn label_pane(&self, params: &TaskDispatchParams, workspace: &NewWorkspace) -> bool {
        let Some(pane_id) = &workspace.root_pane_id else {
            return false;
        };
        if params.task.id.is_empty() {
            return false;
        }
        let label = format!("{OWNED_LABEL_PREFIX}{}", params.task.id);
        match self
            .client
            .call(
                "pane.rename",
                to_params(
                    "pane.rename",
                    &request::PaneRenameParams {
                        pane_id: pane_id.to_string(),
                        label: Some(label),
                    },
                )
                .unwrap_or(Value::Null),
            )
            .await
        {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    pane_id, error = %e,
                    "could not label the pane; keeping the workspace's machine label so the \
                     marker survives a herdr restart"
                );
                false
            }
        }
    }

    /// Replace the machine label with one an operator can read: `{repo}: {task}`
    /// (#417 D4).
    ///
    /// # Why this is a third call and not just a better `workspace.create`
    ///
    /// `workspace.create` writes `totsuka {task.id}` — **byte-identical to
    /// before #417** — so the ownership marker exists from the workspace's
    /// first instant. Renaming afterwards, and **only when both reports
    /// succeeded**, means there is no moment where identity is missing from
    /// *both* the label and the tokens: a herdr that blips during the reports
    /// leaves the machine label in place and merely does not get prettier.
    ///
    /// That ordering is also what lets `release` trust the token over the
    /// label — a container with no token was never renamed, so its label is
    /// still the marker form the label path compares.
    ///
    /// # Why the sidebar still needs a label at all
    ///
    /// `rows` is global: the operator's own spaces get the same row layout,
    /// and `$repo` / `$task` are empty there. So the first row cannot drop the
    /// built-in `workspace` token — which means an opaque `workspace` leaves
    /// **both panels'** first row broken, tokens or no tokens.
    ///
    /// Skipped without a `repo_name` (an Orchestrator older than protocol
    /// 0.4.1): `: Fix the bug` is not an improvement on `totsuka 42`.
    async fn rename_for_humans(&self, params: &TaskDispatchParams, workspace: &NewWorkspace) {
        let Some(repo) = &params.repo_name else {
            return;
        };
        // `token_value` is reused for its whitespace collapsing, which a label
        // wants too. Its 80 is the measured limit on a metadata **token**
        // value, though — herdr's label limit is **not measured** and is not
        // in the API reference. Borrowed as a safe-side stand-in: if the real
        // ceiling is lower, herdr cuts further and the `…` stops meaning what
        // it says; if higher, this cuts sooner than it needs to.
        let label = token_value(&format!("{repo}: {}", params.task.title));
        if let Err(e) = self
            .client
            .call(
                "workspace.rename",
                to_params(
                    "workspace.rename",
                    &request::WorkspaceRenameParams {
                        workspace_id: workspace.id.clone(),
                        label,
                    },
                )
                .unwrap_or(Value::Null),
            )
            .await
        {
            tracing::warn!(
                workspace_id = %workspace.id, error = %e,
                "could not rename the workspace; the machine label stays, which is still ours"
            );
        }
    }

    /// Tear down a workspace a failed dispatch allocated, best-effort: the
    /// dispatch error is what the caller needs to see, so a cleanup that also
    /// fails is logged rather than raised.
    fn abandon(&self, workspace_id: &str) {
        let client = self.client.clone();
        let workspace_id = workspace_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = client
                .call(
                    "workspace.close",
                    to_params(
                        "workspace.close",
                        &request::WorkspaceTarget {
                            workspace_id: workspace_id.to_string(),
                        },
                    )
                    .unwrap_or(Value::Null),
                )
                .await
            {
                tracing::warn!(
                    workspace_id, error = %e,
                    "could not close the workspace of a failed dispatch; it may need closing by hand"
                );
            }
        });
    }

    /// Hand `prompt` to the agent and wait until it reacts.
    ///
    /// One call (protocol 17,
    /// [ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-5).
    /// `agent.prompt` types the text *and* submits it, and its `wait` is
    /// herdr's own version of the guarantee this method used to build by hand:
    /// it requires an observed state change within 5s of a submission from a
    /// non-working state, and answers `agent_prompt_stalled` when there is
    /// none.
    ///
    /// What that replaces, from #124 and #281: re-sending the text until it
    /// appeared on screen, re-pressing Enter until `agent_status` moved,
    /// matching the prompt's whitespace-squashed tail against a wrapped input
    /// box, and the `RetryPolicy` tuning all of it. herdr's `agent.send` — the
    /// write-without-submitting the dance was built around — no longer exists.
    ///
    /// **A successful `agent.start` does not mean the agent can take a prompt.**
    /// herdr can accept the launch and answer `launch_pending: true` with
    /// `agent_status: unknown`, and `agent.prompt` then refuses with
    /// `agent_not_ready` until the CLI is actually up — measured live at ~4s.
    /// That refusal is waited out here, but only for
    /// [`PROMPT_READY_WINDOW`], **not** for the whole dispatch budget.
    ///
    /// The short window is the fix for #387. `agent_not_ready` has two causes
    /// that look identical from here: a CLI that is merely slow (clears in
    /// seconds) and a CLI that was never launched at all, because
    /// `agent.start`'s keystrokes went into a shell that was not reading yet
    /// (never clears — measured live, it answered `agent_not_ready` for as
    /// long as it was asked). Spending the entire budget here served the first
    /// case and failed the second; returning the refusal lets
    /// [`start_agent`](Self::start_agent) re-issue `agent.start`, which is what
    /// actually clears it. `agent_not_found` is **not** retried: it is a pane
    /// that died, and on a resumed dispatch it has to surface as
    /// `SESSION_UNRESUMABLE`.
    ///
    /// **The contract is unchanged**: a prompt that cannot be confirmed as
    /// submitted fails the dispatch rather than leaving a pane that sits idle
    /// forever with the task unsent.
    async fn submit_prompt(
        &self,
        pane_id: &str,
        prompt: &str,
        deadline: tokio::time::Instant,
    ) -> Result<(), HerdrError> {
        let params = to_params(
            "agent.prompt",
            &request::AgentPromptParams {
                target: pane_id.to_string(),
                text: prompt.to_string(),
                wait: Some(request::AgentPromptWaitOptions {
                    until: SETTLED_OR_WORKING.to_vec(),
                    timeout_ms: Some(PROMPT_WAIT_MS),
                }),
            },
        )?;
        // Whichever comes first: this attempt's own patience, or the dispatch's
        // overall budget. Past the former the answer is "re-start the agent",
        // past the latter it is "give up" — and `start_agent` tells them apart
        // by re-checking the same deadline.
        let ready_deadline = (tokio::time::Instant::now() + PROMPT_READY_WINDOW).min(deadline);
        loop {
            match self.client.call("agent.prompt", params.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) if e.is_agent_not_ready() => {
                    if tokio::time::Instant::now() >= ready_deadline {
                        return Err(e);
                    }
                    tracing::debug!(
                        pane_id,
                        "the agent has not finished launching yet; retrying agent.prompt"
                    );
                    tokio::time::sleep(STARTUP_RETRY_POLL).await;
                }
                // Submitted, but herdr saw no reaction inside its 5s floor.
                // Confirm — never re-send (#380).
                Err(e) if e.is_prompt_stalled() => {
                    return self.confirm_submission(pane_id, e).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Decide whether a stalled `agent.prompt` actually landed, by watching the
    /// agent instead of prompting it again (#380).
    ///
    /// The prompt is already in the agent by the time `agent_prompt_stalled`
    /// comes back — herdr typed and submitted it, then failed to observe a
    /// state change within its own 5s floor. Re-sending would deliver the task
    /// twice, so the question "did it land?" is answered by `agent.wait`, which
    /// asks herdr the same thing with a window we choose.
    ///
    /// **A pane that vanished keeps its own error.** `agent.wait` answering
    /// `agent_not_found` means the CLI died, and on a resumed dispatch that has
    /// to reach the Orchestrator as `SESSION_UNRESUMABLE` (#261) — reporting
    /// the stall instead would bury it. Any other failure reports the stall,
    /// because "the agent never reacted" is the symptom worth showing.
    async fn confirm_submission(&self, pane_id: &str, stall: HerdrError) -> Result<(), HerdrError> {
        tracing::warn!(
            pane_id,
            "agent.prompt saw no state change inside herdr's 5s floor; confirming with \
             agent.wait rather than re-sending the prompt"
        );
        self.submit_if_left_unsent(pane_id).await;
        match self
            .client
            .call(
                "agent.wait",
                to_params(
                    "agent.wait",
                    &request::AgentWaitParams {
                        target: pane_id.to_string(),
                        until: SETTLED_OR_WORKING.to_vec(),
                        timeout_ms: Some(PROMPT_CONFIRM_MS),
                    },
                )?,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(
                    pane_id,
                    "the agent did react; the prompt had landed after all"
                );
                Ok(())
            }
            Err(e) if e.is_missing() => Err(e),
            Err(e) => {
                tracing::warn!(pane_id, error = %e, "agent.wait could not confirm the prompt either");
                Err(stall)
            }
        }
    }

    /// Press Enter when a stalled prompt is sitting in the agent's input box,
    /// unsent.
    ///
    /// **This is what a stall usually is.** `agent_prompt_stalled` was read as
    /// "typed and submitted, but the agent was slow to react" (#380), and the
    /// answer was to confirm with `agent.wait`. Measured live (#391): the text
    /// is typed, but the **Enter is not always delivered** — the pane shows it
    /// on the `❯` input line with `agent_status: idle`, and it stays that way
    /// (still `idle` 25s later, and for as long as it is watched). Nothing is
    /// coming, so `agent.wait` can only ever time out. Sending Enter to such a
    /// pane took it to `done` in ~10s.
    ///
    /// **Enter, never the text again.** #380's caution holds and is the reason
    /// this is not a re-send: the prompt IS in the box, so typing it a second
    /// time appends to it and garbles the task. Submitting what is already
    /// there delivers it exactly once.
    ///
    /// Only when the agent is `idle`. A pane that is `working`/`done`/`blocked`
    /// did receive its prompt, so there is nothing to submit and a stray Enter
    /// is noise. Best-effort throughout: this is a rescue attempt on a dispatch
    /// that is already failing, so every error just leaves `agent.wait` to give
    /// the verdict.
    async fn submit_if_left_unsent(&self, pane_id: &str) {
        match pane_status(&self.client, pane_id).await {
            Ok(AgentStatus::Idle) => {}
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(pane_id, error = %e, "could not read the pane before confirming");
                return;
            }
        }
        tracing::warn!(
            pane_id,
            "the prompt is sitting unsent in an idle agent; pressing Enter to submit what is \
             already there (never re-typing it)"
        );
        if let Err(e) = self
            .client
            .call(
                "agent.send_keys",
                match to_params(
                    "agent.send_keys",
                    &request::AgentSendKeysParams {
                        target: pane_id.to_string(),
                        keys: vec!["enter".to_string()],
                    },
                ) {
                    Ok(params) => params,
                    Err(e) => {
                        tracing::warn!(pane_id, error = %e, "could not build the Enter keypress");
                        return;
                    }
                },
            )
            .await
        {
            tracing::warn!(pane_id, error = %e, "could not press Enter on the stalled prompt");
        }
    }

    /// Re-attach to a dispatched session (F-37): confirm the pane is alive with
    /// `pane.get` and report its current mapped state. A vanished pane
    /// (`pane_not_found`) is reported as `attached: false` (the Orchestrator's
    /// recovery then defers to a human).
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        match pane_status(&self.client, &handle.pane_id).await {
            Ok(status) => Ok(SessionAttachResult {
                attached: true,
                state: map_agent_status(status, AgentState::Idle),
            }),
            // The pane is gone → not attached (F-37); the Orchestrator (#57)
            // routes this to human confirmation rather than auto-failing.
            Err(e) if e.is_missing() => Ok(SessionAttachResult {
                attached: false,
                state: AgentState::Failed,
            }),
            Err(e) => Err(e),
        }
    }

    /// Cancel a task: interrupt the agent (`ctrl+c`), then take down the
    /// workspace `dispatch` created for it. The interrupt is best-effort — even
    /// if it fails the pane is still closed — and anything already gone counts
    /// as success, so cancel is idempotent.
    pub async fn cancel(&self, session_id: &str) -> Result<(), HerdrError> {
        let handle = SessionHandle::decode(session_id);
        // Best-effort interrupt: a failure here must not prevent the close,
        // otherwise a stuck agent's pane would leak.
        if let Err(e) = self
            .client
            .call(
                "pane.send_keys",
                to_params(
                    "pane.send_keys",
                    &request::PaneSendKeysParams {
                        pane_id: handle.pane_id.clone(),
                        keys: vec!["ctrl+c".to_string()],
                    },
                )
                .unwrap_or(Value::Null),
            )
            .await
            && !e.is_missing()
        {
            tracing::warn!(error = %e, "pane.send_keys failed during cancel; closing anyway");
        }
        self.close_pane_and_workspace(&handle).await
    }

    /// Say whether the task still has a pane of ours, when the recorded pane id
    /// did not resolve to it (0.4.2, #485).
    ///
    /// A pane id is position-based, so "the id names something else now" says
    /// nothing about the task's own pane — it may be gone, or it may be sitting
    /// two positions over. The caller's real question is the second one: it is
    /// about to open a new pane for this task, and an existing one collides.
    ///
    /// Two pieces of evidence answer it, tried strongest-first:
    ///
    /// 1. **The agent conversation id** ([`SessionHandle::agent_session_id`]).
    ///    A live pane reporting the same conversation *is* this task's pane,
    ///    wherever its shell has `cd`'d to — and an agent that wandered out of
    ///    its worktree is exactly the case the cwd check below cannot see,
    ///    while its name registration collides all the same.
    /// 2. **The worktree path** (`expect_cwd`). Unique per task, and this
    ///    plugin can enumerate the panes it owns — both facts already
    ///    load-bearing elsewhere (`expect_cwd` is the identity guard;
    ///    `session/list` is how `doctor` finds orphans). This catches a pane
    ///    whose session report never landed, which the id check cannot.
    ///
    /// With no evidence either way — no conversation id recorded, no
    /// `expect_cwd`, or an enumeration that failed — the answer degrades to
    /// [`NotReleased::Gone`]: it is what a pre-0.4.2 plugin effectively said,
    /// and the caller treats it as "carry on". A guess must not be turned into
    /// a claim in either direction.
    async fn classify_unreleased(
        &self,
        handle: &SessionHandle,
        params: &SessionReleaseParams,
    ) -> NotReleased {
        if !handle.agent_session_id.is_empty() {
            match call_typed::<_, _, PaneListEnvelope>(
                &self.client,
                "pane.list",
                &request::PaneListParams { workspace_id: None },
            )
            .await
            {
                Ok(panes) => {
                    let alive = panes.panes.iter().any(|pane| {
                        agent_session_id(pane) == Some(handle.agent_session_id.as_str())
                    });
                    if alive {
                        return NotReleased::Refused;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "could not enumerate panes to classify an unreleased session; \
                         reporting it as gone"
                    );
                    return NotReleased::Gone;
                }
            }
        }
        let Some(cwd) = params.expect_cwd.as_deref() else {
            return NotReleased::Gone;
        };
        match self.list_sessions().await {
            Ok(list) => {
                if list.sessions.iter().any(|s| s.cwd.as_deref() == Some(cwd)) {
                    NotReleased::Refused
                } else {
                    NotReleased::Gone
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "could not enumerate panes to classify an unreleased session; \
                     reporting it as gone"
                );
                NotReleased::Gone
            }
        }
    }

    /// Release a **finished** session's pane (`session/release`, #210): close
    /// the pane and its workspace, without the interrupt `cancel` sends —
    /// there is nothing left to interrupt.
    ///
    /// Unlike cancel's blind close (which runs moments after dispatch),
    /// release can run days later under a retention policy, and herdr pane
    /// ids are position-based — the id may name a *different* pane by now. So
    /// the live pane is fetched and compared against the caller's `expect_*`
    /// fields first: any comparable pair that mismatches refuses the close
    /// (`released: false`); if no pair is comparable the close proceeds
    /// (degrade-open — refusing on missing data would leak a pane on every
    /// task to guard against a rare reused id).
    pub async fn release(
        &self,
        params: &SessionReleaseParams,
    ) -> Result<SessionReleaseResult, HerdrError> {
        let handle = SessionHandle::decode(&params.session_id);
        let pane = match pane_record(&self.client, &handle.pane_id).await {
            Ok(pane) => pane,
            // Nothing at that id. Usually the pane is simply gone (cancel
            // closed it), but pane ids are position-based, so it can also mean
            // the task's pane moved. `not_released` is only worth anything if
            // it answers which (0.4.2, #485).
            Err(e) if e.is_missing() => {
                return Ok(SessionReleaseResult {
                    released: false,
                    not_released: Some(self.classify_unreleased(&handle, params).await),
                });
            }
            Err(e) => return Err(e),
        };
        // The label lives on the *workspace*, not the pane (#416), so the
        // pane-label comparison below has never once been comparable against a
        // real herdr — every release fell through to the degrade-open branch.
        // Fetched only when there is something to compare it against.
        let (workspace_label, workspace_token) = match params.expect_label {
            Some(_) => self.workspace_identity(workspace_of(&handle.pane_id)).await,
            None => (None, None),
        };
        // What the caller's label says the task is (#417). The token is
        // reported as the bare task id; `expect_label` wraps it in the marker.
        let expect_task = params
            .expect_label
            .as_deref()
            .and_then(|l| l.strip_prefix(OWNED_LABEL_PREFIX));
        let token = identity_token(&pane.tokens).or(workspace_token.as_deref());
        let mut checks = vec![("cwd", params.expect_cwd.as_deref(), pane.cwd.as_deref())];
        // The token replaces the labels **only when both sides have one**.
        //
        // Preferring it is a correctness requirement, not a taste: #417 D4
        // renames the workspace to `{repo}: {title}`, so its label stops
        // matching `expect_label` while naming the very same task. Compared,
        // that would refuse every release of a renamed workspace.
        //
        // But `expect_task` is `None` for a caller whose `expect_label` is not
        // in our `totsuka {id}` form, and dropping the label checks *then*
        // would leave zero comparable pairs — degrade-open, closing a pane on
        // no evidence at all. Every caller sends the marker form today; this
        // is about not making that a silent precondition.
        //
        // The label path is not weakened by the swap, because D4 only renames
        // when **both** reports succeeded: a container with no token was never
        // renamed and still carries `totsuka {task}`.
        match (token, expect_task) {
            (Some(actual), Some(expected)) => {
                checks.push(("identity token", Some(expected), Some(actual)));
            }
            _ => checks.extend([
                (
                    "label",
                    params.expect_label.as_deref(),
                    pane.label.as_deref(),
                ),
                (
                    "workspace label",
                    params.expect_label.as_deref(),
                    workspace_label.as_deref(),
                ),
            ]),
        }
        let mut comparable = false;
        for (field, expected, actual) in checks {
            let (Some(expected), Some(actual)) = (expected, actual) else {
                continue;
            };
            comparable = true;
            if expected != actual {
                tracing::warn!(
                    pane_id = %handle.pane_id,
                    field,
                    expected,
                    actual,
                    "release refused: the pane id names a different pane now"
                );
                return Ok(SessionReleaseResult {
                    released: false,
                    not_released: Some(self.classify_unreleased(&handle, params).await),
                });
            }
        }
        if !comparable && (params.expect_cwd.is_some() || params.expect_label.is_some()) {
            tracing::debug!(
                pane_id = %handle.pane_id,
                "identity unverifiable (pane reports none of the expected fields); closing anyway"
            );
        }
        self.close_pane_and_workspace(&handle).await?;
        Ok(SessionReleaseResult {
            released: true,
            not_released: None,
        })
    }

    /// Enumerate the live panes this plugin owns (`session/list`, #211):
    /// `pane.list` joined to `workspace.list` on `PaneInfo.workspace_id`: a
    /// pane is ours when **its workspace's** label starts with `totsuka `.
    /// That is the ownership boundary — herdr serves
    /// human-opened panes too, and those must never be listed as release
    /// candidates.
    ///
    /// # Why not the pane's own label (#416)
    ///
    /// That is what this did until now, and it returned an empty array against
    /// every real herdr: totsuka writes the marker with
    /// `workspace.create { label }` and **nothing ever writes a
    /// `PaneInfo.label`** — only `pane.rename` does, which totsuka does not
    /// call. The integration tests passed because the fake staged pane labels
    /// by hand. Renaming panes instead was rejected: with
    /// `show_agent_labels_on_pane_borders = true` it puts an opaque id on the
    /// pane border, which is a visible regression for the operator.
    ///
    /// The pane's own label is still honoured, for the day herdr propagates
    /// one.
    ///
    /// # One session per workspace
    ///
    /// A totsuka workspace holds **two** panes: the agent's and the companion
    /// shell. Both match a workspace-level test, so the pane that reports an
    /// `agent` wins and the others are dropped — otherwise `doctor` would ask
    /// about the same task twice and the second `session/release` would answer
    /// `released: false`. When no pane in the workspace reports one (the agent
    /// has exited, which is exactly the orphan case), the first pane stands in.
    ///
    /// The returned `session_id` encodes the pane with an **empty** agent
    /// session id: `pane.list` does not say which Claude session runs inside,
    /// and `session/release` only needs the pane (`SessionHandle::decode`
    /// accepts the bare form).
    pub async fn list_sessions(&self) -> Result<SessionListResult, HerdrError> {
        // **A response this build cannot read is "nothing", not a failure.**
        // `session/list` is a best-effort diagnostic surface — `doctor`'s
        // orphan-pane detection stands on it — and #416 deliberately made a
        // shapeless answer mean "nothing owned" rather than a panic. Typing
        // the read must not quietly take that away, so the degrade moved here
        // from the `Value` digging that used to carry it.
        //
        // A *failed call* still propagates, exactly as before: not reaching
        // herdr is a different fact from herdr answering oddly.
        let panes: PaneListEnvelope = unreadable_is_empty(
            "pane.list",
            self.client
                .call(
                    "pane.list",
                    to_params("pane.list", &request::PaneListParams { workspace_id: None })?,
                )
                .await?,
        )
        .unwrap_or(PaneListEnvelope { panes: Vec::new() });
        let workspaces: WorkspaceListEnvelope = unreadable_is_empty(
            "workspace.list",
            self.client
                .call(
                    "workspace.list",
                    to_params("workspace.list", &request::EmptyParams {})?,
                )
                .await?,
        )
        .unwrap_or(WorkspaceListEnvelope {
            workspaces: Vec::new(),
        });
        let owned = owned_workspaces(&workspaces.workspaces);

        // `Vec`, not a map: `pane.list` order is the only stable ordering
        // there is, and a hash map would shuffle `session list` output between
        // runs.
        let mut chosen: Vec<(&str, bool, SessionInfo)> = Vec::new();
        for pane in &panes.panes {
            let workspace = pane.workspace_id.as_str();
            // Four ways a pane can be ours, in descending order of directness
            // (#417 D2). The token paths are the new evidence; the label paths
            // stay because a dispatch whose report failed, and every pane a
            // release before this one left behind, has only those.
            let by_token = identity_token(&pane.tokens)
                .or_else(|| owned.tokens.get(workspace).copied())
                .map(|task| format!("{OWNED_LABEL_PREFIX}{task}"));
            let by_label = pane
                .label
                .as_deref()
                .filter(|l| l.starts_with(OWNED_LABEL_PREFIX))
                .or_else(|| owned.labels.get(workspace).copied())
                .map(str::to_string);
            // Synthesised from the token when there is one, so `doctor`'s
            // `strip_prefix` → `source_task_id` match keeps working even after
            // the label becomes human-readable in PR-3.
            let Some(label) = by_token.or(by_label) else {
                continue;
            };
            let has_agent = looks_like_an_agent_pane(pane);
            let info = SessionInfo {
                session_id: SessionHandle::new(&pane.pane_id, "").encode(),
                label: Some(label),
                cwd: pane.cwd.clone(),
            };
            match chosen.iter().position(|(w, ..)| *w == workspace) {
                Some(i) if has_agent && !chosen[i].1 => chosen[i] = (workspace, has_agent, info),
                Some(_) => {}
                None => chosen.push((workspace, has_agent, info)),
            }
        }
        Ok(SessionListResult {
            sessions: chosen.into_iter().map(|(.., info)| info).collect(),
        })
    }

    /// One workspace's `(label, identity token)`, for `release`'s identity
    /// check (#416, #417).
    ///
    /// The label is **deliberately unfiltered**, unlike [`owned_workspaces`].
    /// Here the interesting answer is a label that is *not* ours: filtering to
    /// `totsuka `-prefixed labels would turn "this workspace belongs to
    /// someone else" into "cannot say", and the caller degrades open on
    /// "cannot say" — closing the operator's pane on a reused pane id, the
    /// exact accident this check exists to prevent.
    ///
    /// `None` means "cannot say", never "does not match": a transient herdr
    /// error must not read as a mismatch and leak the pane either.
    async fn workspace_identity(
        &self,
        workspace_id: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        let Some(workspace_id) = workspace_id else {
            return (None, None);
        };
        let response: WorkspaceListEnvelope = match call_typed(
            &self.client,
            "workspace.list",
            &request::EmptyParams {},
        )
        .await
        {
            Ok(response) => response,
            Err(e) => {
                tracing::debug!(workspace_id, error = %e, "workspace.list failed; identity unverifiable");
                return (None, None);
            }
        };
        let Some(ws) = response
            .workspaces
            .iter()
            .find(|ws| ws.workspace_id == workspace_id)
        else {
            return (None, None);
        };
        (
            Some(ws.label.clone()),
            identity_token(&ws.tokens).map(str::to_string),
        )
    }

    /// Close a session's pane and the task-private workspace `dispatch`
    /// created for it. `dispatch` gives every task its own workspace, so
    /// closing the pane alone would leave an empty one behind. Idempotent:
    /// anything already gone counts as success.
    async fn close_pane_and_workspace(&self, handle: &SessionHandle) -> Result<(), HerdrError> {
        ignore_missing(
            self.client
                .call(
                    "pane.close",
                    to_params(
                        "pane.close",
                        &request::PaneTarget {
                            pane_id: handle.pane_id.clone(),
                        },
                    )?,
                )
                .await,
        )?;
        if let Some(workspace_id) = workspace_of(&handle.pane_id) {
            ignore_missing(
                self.client
                    .call(
                        "workspace.close",
                        to_params(
                            "workspace.close",
                            &request::WorkspaceTarget {
                                workspace_id: workspace_id.to_string(),
                            },
                        )?,
                    )
                    .await,
            )?;
        }
        Ok(())
    }

    /// Bring the session's pane to the foreground (`session/focus`, F-94):
    /// confirm the pane is alive with `pane.get`, then focus outside-in —
    /// workspace, tab, pane — so herdr lands on the right pane whichever
    /// container was active. The GUI terminal itself is brought to the front
    /// separately by the notifier (`-activate`); this only moves herdr's
    /// focus. A vanished pane (or container) reports `focused: false` — a
    /// notification clicked after the task's pane closed is a normal path,
    /// not an error.
    pub async fn focus(&self, session_id: &str) -> Result<SessionFocusResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane = match pane_record(&self.client, &handle.pane_id).await {
            Ok(pane) => pane,
            Err(e) if e.is_missing() => return Ok(SessionFocusResult { focused: false }),
            Err(e) => return Err(e),
        };
        // Container ids come from the pane record. `workspace_id` and `tab_id`
        // are both `required` on a `PaneInfo`, so the old "a record without a
        // tab id skips the tab step" case cannot arise any more — a record
        // missing either does not deserialize, and `pane_record` reports that
        // above rather than quietly focusing less.
        //
        // The pane-id prefix (`w1:p2` lives in `w1`) is kept as the fallback
        // only for the shape where the two disagree; it costs nothing.
        let workspace_id = if pane.workspace_id.is_empty() {
            workspace_of(&handle.pane_id).map(str::to_string)
        } else {
            Some(pane.workspace_id.clone())
        };
        if let Some(workspace_id) = workspace_id
            && !self
                .focus_step(
                    "workspace.focus",
                    &request::WorkspaceTarget { workspace_id },
                )
                .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        if !self
            .focus_step(
                "tab.focus",
                &request::TabTarget {
                    tab_id: pane.tab_id,
                },
            )
            .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        let focused = self
            .focus_step(
                "pane.focus",
                &request::PaneTarget {
                    pane_id: handle.pane_id.clone(),
                },
            )
            .await?;
        Ok(SessionFocusResult { focused })
    }

    /// One focus call: `Ok(true)` on success, `Ok(false)` when the target is
    /// gone (the pane/tab/workspace closed between the liveness check and this
    /// call), and the error otherwise.
    async fn focus_step<P: serde::Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<bool, HerdrError> {
        // The result is deliberately untyped: `methods.json` records these
        // three as `result: null` because nothing here reads the answer, and
        // asserting a result type nobody reads would be a claim with no way to
        // check it (ADR-0055 D-8).
        match self.client.call(method, to_params(method, params)?).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_missing() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Capture the pane's screen for timeout/escalation diagnostics
    /// (`diagnostics/snapshot`, R-10). Reads the `recent` screen copy; a pane
    /// that is gone (or otherwise unreadable) is **not** an error — it reports
    /// `text: None`, so the Orchestrator's escalation path never fails on a
    /// snapshot it could not take.
    pub async fn snapshot(
        &self,
        session_id: &str,
    ) -> Result<DiagnosticsSnapshotResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let text = read_pane_text(
            &self.client,
            &handle.pane_id,
            request::ReadSource::Recent,
            SCREEN_LINES,
        )
        .await;
        Ok(DiagnosticsSnapshotResult { text })
    }

    /// Start the state stream for a session (F-38), reduced to a **deadman**
    /// since completion is now reported by Claude Code's hooks (#131 / R-07):
    /// subscribe to `pane.exited` only and report `Failed` on an abnormal exit.
    /// A normal exit (code 0) is silent — the SessionEnd hook already reported
    /// completion out-of-band. The stream ends at the first terminal event.
    pub async fn start_state_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane_id = handle.pane_id.clone();
        // Only the deadman subscription remains — no `pane.agent_status_changed`,
        // so screen-manifest status flicker can no longer drive task state.
        //
        // **This one is deliberately not built from
        // [`wire::request::Subscription`](crate::wire::request::Subscription).**
        // Typing it surfaced something worth writing down: herdr's schema makes
        // `pane.exited` a **unit** variant — it takes no `pane_id`, so the
        // subscription is global and the id below is a key herdr ignores
        // (nothing in its request schema sets `additionalProperties: false`).
        //
        // The id is still sent because *totsuka* reads it back:
        // `transport::subscribed_panes` derives the panes to notify on a dead
        // connection from these very params. Nothing here depends on herdr
        // filtering — `classify_exit` filters every envelope by `pane_id`
        // itself, precisely because the stream carries other panes' events.
        //
        // Sending the typed unit variant instead would silently drop that key
        // and take the subscription-closed deadman with it.
        let subscriptions = json!([
            { "type": "pane.exited", "pane_id": pane_id },
        ]);
        // Take the event receiver *before* subscribing, so events herdr pushes
        // immediately after the ACK are buffered rather than raced past.
        let mut events = self.client.events();
        self.client.subscribe_events(subscriptions).await?;

        let session_id = session_id.to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let signal = match events.recv().await {
                    Ok(event) => classify_exit(&event, &pane_id),
                    // Lagged past the buffer: the deadman only ever emits one
                    // terminal event, so a dropped batch cannot strand a
                    // healthy stream — keep waiting for the exit.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "herdr event stream lagged; deadman still waiting");
                        continue;
                    }
                    // The transport closed: end the stream.
                    Err(_) => break,
                };
                match signal {
                    ExitSignal::Failed => {
                        let _ = tx.send(StateNotification {
                            session_id: session_id.clone(),
                            state: AgentState::Failed,
                            log_chunk: None,
                        });
                        break;
                    }
                    // A clean exit needs no notification (the hook reported the
                    // normal end); the pane is gone, so end the stream.
                    ExitSignal::CleanExit => break,
                    ExitSignal::Ignore => continue,
                }
            }
        });

        Ok(rx)
    }
}

/// `workspace.create`'s answer, read down to **exactly the two ids the rest of
/// dispatch needs** rather than through the generated
/// [`WorkspaceCreatedEnvelope`](crate::wire::result::WorkspaceCreatedEnvelope).
///
/// # Why this one call is read loosely
///
/// The reason is ordering, not shape: **by the time this is read the workspace
/// already exists on the operator's screen.** Any hard failure here returns
/// before [`NewWorkspace`] exists, so nothing holds the id that
/// [`abandon`](HerdrAgent::abandon) needs — and the dispatch leaks a workspace
/// with a live pane in it.
///
/// A first attempt at this relaxed only `root_pane`'s *presence*, which was not
/// enough: a `root_pane` that is **there but malformed**, or a `workspace`
/// missing any of `WorkspaceInfo`'s other seven required fields, still failed
/// the whole read and leaked the same workspace. So nothing here is read that
/// is not used.
///
/// # This is not a hole in the design
///
/// [ADR-0055](../../../ai-docs/decisions/adr-0055-herdr-schema-typed-wire.md)
/// is "**runtime forgiving, CI strict**". The generated envelope stays the
/// record of what herdr promises, and the schema diff in CI is what reports a
/// change to it — before merge, where nothing is running. Being strict *here*
/// as well would buy no earlier warning and would cost a leaked workspace.
#[derive(Debug, serde::Deserialize)]
struct CreatedWorkspace {
    workspace: CreatedWorkspaceRef,
    /// Absent, or present but unreadable, both land as `None`: the caller
    /// treats "no root pane" as a dispatch failure it can still clean up after.
    #[serde(default)]
    root_pane: Option<CreatedPaneRef>,
}

/// The one field of `workspace.create`'s `workspace` that dispatch uses.
#[derive(Debug, serde::Deserialize)]
struct CreatedWorkspaceRef {
    workspace_id: String,
}

/// The one field of its `root_pane` that dispatch uses.
#[derive(Debug, serde::Deserialize)]
struct CreatedPaneRef {
    pane_id: String,
}

/// What `dispatch` needs out of a `workspace.create` response: the workspace
/// itself, and the root pane herdr opens it with.
///
/// The root pane is where the agent is started (protocol 17), and the response
/// is the only handle to it — the response names it (`root_pane`) and nothing
/// else does: `pane.list` cannot distinguish it from the agent's pane by label,
/// because a split pane's label is `null` on both.
struct NewWorkspace {
    id: String,
    /// `None` when the response carried no `root_pane` — a herdr this plugin
    /// does not know how to drive.
    ///
    /// Still an `Option` rather than a hard parse error because the two callers
    /// want different things from its absence: `dispatch` cannot proceed
    /// (protocol 17 starts the agent *in* this pane), but the workspace id has
    /// already been read by then and every teardown path needs it, so the
    /// failure has to happen after `NewWorkspace` exists — not instead of it.
    root_pane_id: Option<String>,
}

impl NewWorkspace {
    fn from_created(created: CreatedWorkspace) -> Self {
        Self {
            id: created.workspace.workspace_id,
            root_pane_id: created.root_pane.map(|pane| pane.pane_id),
        }
    }
}

/// Deserialize a list response, treating "this build cannot read it" as an
/// empty list rather than an error.
///
/// **Only for the best-effort surfaces.** Everywhere else an unreadable answer
/// is an error at the call site — that is the point of #519. Here the caller is
/// `session/list`, whose contract since #416 is that whatever herdr answers,
/// the worst outcome is "nothing owned".
fn unreadable_is_empty<T: serde::de::DeserializeOwned>(method: &str, result: Value) -> Option<T> {
    match serde_json::from_value(result) {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::warn!(
                method, error = %e,
                "herdr answered a shape this build cannot read; treating it as an empty list"
            );
            None
        }
    }
}

/// The pane record (`pane.get` nests it under `result.pane`).
async fn pane_record<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<PaneInfo, HerdrError> {
    let envelope: PaneInfoEnvelope = call_typed(
        client,
        "pane.get",
        &request::PaneTarget {
            pane_id: pane_id.to_string(),
        },
    )
    .await?;
    Ok(envelope.pane)
}

/// A string field off a pane record, `None` when absent or null (herdr's
/// `PaneInfo.cwd`/`label` are both optional-and-nullable).
/// The pane's current `agent_status`.
///
/// `agent_status` is `required` in every herdr this plugin supports, so there
/// is no "absent" case to default any more — a pane record without one does
/// not deserialize, and that is reported by [`pane_record`] rather than
/// silently becoming `unknown`.
async fn pane_status<T: HerdrTransport>(
    client: &T,
    pane_id: &str,
) -> Result<AgentStatus, HerdrError> {
    Ok(pane_record(client, pane_id).await?.agent_status)
}

/// The workspace a pane belongs to. herdr ids nest the workspace in the pane
/// (`w1:p2` lives in `w1`), which is the only handle back to it — the protocol
/// `session_id` carries the pane, not the workspace.
fn workspace_of(pane_id: &str) -> Option<&str> {
    pane_id.split_once(':').map(|(workspace, _)| workspace)
}

/// Whether a `pane.list` record looks like the pane an agent is running in,
/// rather than the companion shell beside it (#416).
///
/// All three signals are read because all three are on a live record. From a
/// dispatched totsuka workspace on herdr 0.7.5:
///
/// ```text
/// {pane_id: "w6E:p1", agent: "claude", agent_status: "idle"   }   ← agent
/// {pane_id: "w6E:p2",                  agent_status: "unknown"}   ← shell
/// ```
///
/// **`agent` is a plain string, and on the shell the key is simply absent**
/// (as `label` is on both — herdr omits these rather than sending `null`). An
/// earlier revision of this comment claimed the field lives only on
/// `agent.start`'s response; that was wrong, and it was wrong because it was
/// reasoned from a probe of a workspace with *no agent started in it* instead
/// of measured against a dispatched one. Any one signal would do today;
/// reading all three makes a future herdr dropping one a degradation rather
/// than a silent regression to "whichever pane came first", which is the
/// companion shell as often as not.
///
/// **What an *exited* agent's pane reports is not measured.** If `agent`
/// lingers, that pane keeps winning the tie-break — which changes nothing
/// that matters: the workspace still yields exactly one session, and the
/// orphan it represents is exactly the one `doctor` is looking for.
fn looks_like_an_agent_pane(pane: &PaneInfo) -> bool {
    // Set while herdr has an agent registered in the pane.
    pane.agent.as_deref().is_some_and(|agent| !agent.is_empty())
        // Reported by the agent's own herdr integration hook once it starts.
        || agent_session_id(pane).is_some()
        // A pane with nothing running in it reports `unknown`.
        || pane.agent_status != AgentStatus::Unknown
}

/// The metadata token that names the task a container belongs to (#417).
///
/// Reported by [`HerdrAgent::report_identity`] onto both the workspace and its
/// root pane. It is a *machine* identifier — never shown — so that renaming a
/// workspace for humans (#417 D4) cannot cost the plugin its ownership
/// evidence.
const IDENTITY_TOKEN: &str = "totsuka_task";

/// The metadata `source` slot every report from this plugin uses.
///
/// **A constant, not something per-task**: a workspace or pane accepts at most
/// 32 distinct `source` values *for its lifetime*, and neither clearing nor
/// expiry gives a slot back.
const METADATA_SOURCE: &str = "totsuka";

/// The [`IDENTITY_TOKEN`] on a `PaneInfo` / `WorkspaceInfo` record, if any.
///
/// `tokens` rides on both `list` and `get` responses (verified on 0.7.5), so
/// this works off `pane.list` without a second round trip.
fn identity_token(tokens: &BTreeMap<String, String>) -> Option<&str> {
    tokens
        .get(IDENTITY_TOKEN)
        .map(String::as_str)
        .filter(|task| !task.is_empty())
}

/// What a `workspace.list` response says about the workspaces this plugin owns
/// (#416, extended for tokens in #417).
///
/// Two maps rather than one because they answer different questions and a
/// workspace can be in either alone: `tokens` is the evidence a *current*
/// dispatch left, `labels` is what a dispatch whose report failed — or one
/// from before #417 — left instead. Absence from both is the answer "not
/// ours".
#[derive(Debug, Default)]
struct OwnedWorkspaces<'a> {
    /// `workspace_id` → the `totsuka ` label.
    labels: HashMap<&'a str, &'a str>,
    /// `workspace_id` → the reported task id.
    tokens: HashMap<&'a str, &'a str>,
}

fn owned_workspaces(workspaces: &[WorkspaceInfo]) -> OwnedWorkspaces<'_> {
    let mut owned = OwnedWorkspaces::default();
    for ws in workspaces {
        let id = ws.workspace_id.as_str();
        if ws.label.starts_with(OWNED_LABEL_PREFIX) {
            owned.labels.insert(id, ws.label.as_str());
        }
        if let Some(task) = identity_token(&ws.tokens) {
            owned.tokens.insert(id, task);
        }
    }
    owned
}

/// The agent's native session id from a pane record
/// (`pane.agent_session.value`), reported by the agent's herdr integration hook
/// during startup and carried in the [`SessionHandle`] for `claude --resume`.
/// Absent or empty → `None`.
fn agent_session_id(pane: &PaneInfo) -> Option<&str> {
    pane.agent_session
        .as_ref()
        .map(|session| session.value.as_str())
        .filter(|value| !value.is_empty())
}

/// What a single herdr event means to the deadman stream.
enum ExitSignal {
    /// An abnormal exit (nonzero/absent code) or a dead subscription → `Failed`.
    Failed,
    /// A clean exit (code 0) — the SessionEnd hook already reported the normal
    /// end, so the stream ends without a notification.
    CleanExit,
    /// Not for this pane, or not an exit event: ignore.
    Ignore,
}

/// Classify a herdr event envelope for the deadman stream.
///
/// Events arrive as `{event: "<kind>", data: {pane_id, …}}`. Every envelope —
/// herdr's own and the transport's synthetic close — is filtered by pane first:
/// one broadcast carries every subscription in the process, and herdr replays
/// other panes' history on connect, so an unfiltered event would fail a healthy
/// task.
///
/// A `pane_exited` with an explicit `exit_code: 0` is a clean exit (silent); any
/// other exit — nonzero, or no code at all (herdr 0.7.x carries none, so we
/// cannot confirm it was clean) — is `Failed`. In interactive mode Claude Code
/// does not exit on completion, so an unexplained exit really is abnormal.
fn classify_exit(event: &Value, pane_id: &str) -> ExitSignal {
    let Some(kind) = event.get("event").and_then(Value::as_str) else {
        return ExitSignal::Ignore;
    };
    let Some(data) = event.get("data") else {
        return ExitSignal::Ignore;
    };
    if data.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
        return ExitSignal::Ignore;
    }
    // herdr is inconsistent about the separator in event kinds
    // (`pane.agent_status_changed` but `pane_exited`), so compare normalized.
    match kind.replace('.', "_").as_str() {
        "pane_exited" => match data.get("exit_code").and_then(Value::as_i64) {
            Some(0) => ExitSignal::CleanExit,
            _ => ExitSignal::Failed,
        },
        // The subscription connection died; deliver a failure rather than hang
        // a stream that will never see another event. (The pane may still be
        // alive — recovery re-attaches, F-37.)
        SUBSCRIPTION_CLOSED_EVENT => ExitSignal::Failed,
        _ => ExitSignal::Ignore,
    }
}

/// `pane.read` helper: `result.read.text`, ANSI-stripped.
async fn read_pane_text<T: HerdrTransport>(
    client: &T,
    pane_id: &str,
    source: request::ReadSource,
    lines: u32,
) -> Option<String> {
    let envelope: PaneReadEnvelope = call_typed(
        client,
        "pane.read",
        &request::PaneReadParams {
            pane_id: pane_id.to_string(),
            source,
            lines: Some(lines),
            format: None,
            strip_ansi: Some(true),
        },
    )
    .await
    .ok()?;
    Some(envelope.read.text)
}

/// Compose the agent prompt: any extra context as a preamble, then the task
/// (body, or the title when there is no body).
///
/// The title is typed only when there is no body: sources truncate it to a
/// snippet (e.g. Slack's `TITLE_SNIPPET_CHARS`) and the body carries the full
/// task text, so prepending the title would just show a cut-off duplicate line
/// in the pane. A string `extra_context` is rendered as raw text (not JSON — no
/// surrounding quotes); non-string values keep their JSON rendering.
///
/// Hook-capable dispatches usually arrive with no `extra_context` at all: the
/// orchestrator delivers instructions invisibly via the `UserPromptSubmit`
/// hook (`TOTSUKA_PROMPT_CONTEXT` env). `extra_context` remains the VISIBLE
/// fallback for non-hook dispatches (e.g. the task's instructions when no
/// hook channel exists).
///
/// The extra context comes FIRST, and stays first. It originally had to:
/// `submit_prompt` confirmed arrival by matching the prompt's **tail** on
/// screen, and extra context repeats across dispatches, so as a suffix it made
/// every dispatch's tail identical — on a `claude --resume` pane the check
/// could match the PREVIOUS turn's prompt and submit before the new one was
/// typed. Protocol 17's `agent.prompt` removed that check, so the ordering is
/// no longer load-bearing; it is kept because a preamble-then-task prompt is
/// what the agent has been reading all along, and reordering it would change
/// every dispatch's input for no reason.
fn compose_prompt(params: &TaskDispatchParams) -> String {
    let task_text = params.task.body.as_ref().unwrap_or(&params.task.title);
    match &params.extra_context {
        Some(Value::String(s)) => format!("{s}\n\n---\n{task_text}"),
        Some(other) => format!("{other}\n\n---\n{task_text}"),
        None => task_text.clone(),
    }
}

/// How many characters of the readable prefix survive in an [`agent_name`].
///
/// The budget is herdr's 32: `t-` (2) + prefix + `-` (1) + 8 hex = 32.
const NAME_PREFIX_CHARS: usize = 21;

/// The `name` for `agent.start`: `t-<readable prefix>-<8 hex of the task id>`
/// ([ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-2).
///
/// Protocol 17 made `name` an **identifier**, not a label:
/// `[a-z][a-z0-9_-]{0,31}`, unique among live agents. Every task id this plugin
/// sees breaks that as-is — GitHub's is mixed case (`I_kwDOTrfAp88AAA…`) and
/// Slack's carries a colon (`C0BNAU8KKG8:1754…`) — and 32 characters is
/// shorter than either.
///
/// **The hash is what makes truncation safe.** A name that collides does not
/// merely read ambiguously any more; it names another task's agent. Eight hex
/// characters over the *full* id keep that out of reach while the sanitized
/// prefix keeps `herdr agent list` legible to whoever is debugging a run — the
/// only reason to carry a prefix at all.
fn agent_name(task_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(task_id.as_bytes());
    let digest = hash.finalize();

    let mut prefix = String::with_capacity(NAME_PREFIX_CHARS);
    let mut pending_dash = false;
    for c in task_id.chars() {
        if prefix.len() >= NAME_PREFIX_CHARS {
            break;
        }
        if c.is_ascii_alphanumeric() {
            // Only after something was kept, so a leading run of separators
            // cannot produce the `-` start herdr rejects.
            if pending_dash && !prefix.is_empty() {
                prefix.push('-');
            }
            pending_dash = false;
            prefix.push(c.to_ascii_lowercase());
        } else {
            // Collapsed rather than emitted: `a::b` is one separator, and a
            // trailing run leaves nothing behind because it is never flushed.
            pending_dash = true;
        }
    }

    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    if prefix.is_empty() {
        // No alphanumerics at all (an id that is punctuation, or empty): the
        // hash alone is still a valid, unique name — and hex cannot start with
        // a letter-less character, but it *can* start with a digit, which herdr
        // rejects. `t-` in front is what keeps every branch legal.
        format!("t-{hex}")
    } else {
        format!("t-{prefix}-{hex}")
    }
}

/// The longest metadata token value herdr keeps, in **characters**.
///
/// Measured on 0.7.5: `"あ"×100` comes back as 80 characters / 240 bytes.
/// Over-long values are **truncated silently**, not rejected.
const TOKEN_VALUE_CHARS: usize = 80;

/// Fit `value` into a herdr metadata token (#417).
///
/// Whitespace runs collapse to one space and the result is trimmed, because a
/// task title with an embedded newline would otherwise eat a sidebar row.
/// Over-length values are cut to 79 characters plus `…` — herdr would cut them
/// anyway, and doing it here is what makes the cut *visible*.
///
/// The cut walks `char_indices`: `&s[..80]` panics on a Japanese title, which
/// is the common case for this repository's tasks rather than an edge one.
fn token_value(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TOKEN_VALUE_CHARS {
        return collapsed;
    }
    let cut = collapsed
        .char_indices()
        .nth(TOKEN_VALUE_CHARS - 1)
        .map_or(collapsed.len(), |(i, _)| i);
    format!("{}…", &collapsed[..cut])
}

/// The herdr `kind` for `program`
/// ([ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-1).
///
/// Protocol 17 chooses the executable itself from this enum, so the plugin can
/// no longer pass `program` through: it translates the program's **file name**
/// into herdr's vocabulary, consulting `[kind_map]` first so a wrapper script
/// can say what it wraps.
///
/// Nothing is validated against herdr's 21 values here — an unknown `kind` is
/// rejected at `agent.start` with herdr's own message, and a copy of that enum
/// in this crate would only be one more thing that can fall behind (this whole
/// ADR exists because a copy of herdr's shape fell behind).
fn resolve_kind(config: &HerdrConfig, program: &str) -> String {
    let file_name = std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    config
        .kind_map
        .get(file_name)
        .cloned()
        .unwrap_or_else(|| file_name.to_string())
}

/// The `(program, args, env)` to launch in the pane. Since protocol 0.2.3
/// (#196) the Orchestrator sends a fully-resolved `tool_launch` (argv + env,
/// opaque to the plugin) which is used verbatim — no CLI-flag knowledge here.
///
/// Since 0.4.0 (#411) that is the *only* source. The plugin used to assemble an
/// argv of its own from `agent_command`/`plan_args` when `tool_launch` was
/// absent, for orchestrators predating 0.2.3; the manifest now declares
/// `>=0.2.3`, so such an orchestrator cannot launch this plugin at all (F-54)
/// and the fallback was unreachable before it was deleted.
///
/// An absent `tool_launch` therefore means the Orchestrator failed to resolve
/// one, not that it is old. Failing here is the honest answer: assembling a
/// substitute would launch the agent with **no `--settings`**, and a Claude
/// Code pane without the workflow's hooks never reports completion — the task
/// would look dispatched and then hang until it escalated on timeout.
fn resolve_launch(
    params: &TaskDispatchParams,
) -> Result<(String, Vec<String>, Option<Value>), HerdrError> {
    let tool = params
        .tool_launch
        .as_ref()
        .ok_or(HerdrError::MissingToolLaunch)?;
    Ok((
        tool.program.clone(),
        tool.args.clone(),
        // herdr injects this env into the launched process (`workspace.create`
        // applies it to the root pane, D-4). An empty map stays absent so no
        // `env` key is sent at all.
        (!tool.env.is_empty()).then(|| json!(tool.env)),
    ))
}

/// Classify a post-start dispatch failure: a pane that **vanished** while we
/// were handing it a prompt, on a dispatch that asked to resume a session,
/// becomes [`HerdrError::SessionUnresumable`] → the protocol's
/// `SESSION_UNRESUMABLE` (#242, #261).
///
/// This is the plugin absorbing its own backend's vocabulary: herdr says
/// `agent_not_found`, the protocol says "that session is not resumable", and
/// the Orchestrator acts on the latter (one retry without `resume_session_id`)
/// without knowing what a multiplexer or a `--resume` flag is (#196 keeps tool
/// knowledge out of here). The retry is safe to promise because a failed
/// dispatch already takes its workspace back down (`abandon`).
///
/// **It is a heuristic**, and deliberately a narrow one:
///
/// - Only *after* `agent.start` succeeded. A pane that never existed cannot
///   have died of the resume; that is a herdr problem and keeps its own error.
/// - Only when the pane is *gone* ([`HerdrError::is_missing`]) — the shape the
///   real bug had, where `claude --resume <id>` found no such conversation,
///   exited, and took its pane with it. A pane that is alive but slow keeps
///   its own error: the retry drops the session, and with it the conversation
///   the resume existed to preserve, so widening this trades a real cost for a
///   guess.
///
/// A false positive still costs only one extra launch, so the narrowness is
/// about **not** losing context, not about avoiding wasted work.
/// Whether a failed `agent.prompt` means the CLI never started — so the answer
/// is to re-issue `agent.start`, not to keep asking.
///
/// Two herdr codes carry that meaning, and one of them only conditionally:
///
/// - `agent_not_ready`: the start was accepted but the agent never became
///   addressable (#387). Always this.
/// - `agent_not_found`: no such agent. On a **resumed** dispatch that is a pane
///   that died with its session and has to surface as `SESSION_UNRESUMABLE`
///   (#261) — see [`resume_failure`]. On a **fresh** dispatch there is no
///   session that could have died: the only way to reach it is `agent.start`
///   having registered nothing, which is the same shell-readiness race (#391).
///   Measured live on 2026-08-07: two consecutive fresh dispatches failed this
///   way and a plain `tt task retry` cleared both.
///
/// Deliberately not `is_missing()`, which also covers `pane_not_found`. A pane
/// that is gone cannot be started into, so re-issuing would only fail again —
/// slower, and with the second error replacing the informative first one.
fn prompt_means_the_cli_never_started(params: &TaskDispatchParams, error: &HerdrError) -> bool {
    error.is_agent_not_ready() || (error.is_agent_missing() && params.resume_session_id.is_none())
}

fn resume_failure(params: &TaskDispatchParams, error: HerdrError) -> HerdrError {
    if params.resume_session_id.is_some() && error.is_missing() {
        return HerdrError::SessionUnresumable(error.to_string());
    }
    error
}

/// Treat a "missing pane" error as success (for idempotent teardown).
fn ignore_missing(result: Result<Value, HerdrError>) -> Result<(), HerdrError> {
    match result {
        Ok(_) => Ok(()),
        Err(e) if e.is_missing() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `workspace.list` response as herdr sends it. `WorkspaceInfo` requires
    /// `number` / `focused` / `pane_count` / `tab_count` / `active_tab_id` /
    /// `agent_status` alongside the two fields these tests care about, so the
    /// filler lives here rather than in every case.
    fn workspace_list(entries: &[Value]) -> WorkspaceListEnvelope {
        let workspaces: Vec<Value> = entries
            .iter()
            .map(|e| {
                let mut ws = json!({
                    "number": 1, "focused": false, "pane_count": 1, "tab_count": 1,
                    "active_tab_id": "t1", "agent_status": "unknown",
                });
                for (k, v) in e.as_object().expect("object").iter() {
                    ws[k] = v.clone();
                }
                ws
            })
            .collect();
        serde_json::from_value(json!({ "type": "workspace_list", "workspaces": workspaces }))
            .expect("the fixture must be a shape herdr could actually send")
    }

    #[test]
    fn only_totsuka_labelled_workspaces_are_owned() {
        let response = workspace_list(&[
            json!({ "workspace_id": "w1", "label": "totsuka 7" }),
            json!({ "workspace_id": "w2", "label": "scratch" }),
            // A near miss: the prefix includes the space, so this is not ours.
            json!({ "workspace_id": "w5", "label": "totsukaboard" }),
        ]);
        let owned = owned_workspaces(&response.workspaces);
        assert_eq!(owned.labels.get("w1").copied(), Some("totsuka 7"));
        assert_eq!(owned.labels.len(), 1, "{owned:?}");
    }

    /// The two cases this test used to carry — a workspace with **no** `label`
    /// and one with `label: null` — are shapes herdr does not send:
    /// `WorkspaceInfo.label` is `required` and non-nullable in every schema
    /// this plugin supports. They are pinned here as *rejections* instead of
    /// being defended against in the mapping code, which is the whole point of
    /// [ADR-0055](../../../ai-docs/decisions/adr-0055-herdr-schema-typed-wire.md):
    /// a shape herdr cannot produce should not have a branch of its own.
    #[test]
    fn a_response_herdr_cannot_send_is_rejected_rather_than_defended_against() {
        // Everything else stays valid, so what is being pinned is `label` and
        // nothing else: with `label: Option<String>` these would both parse.
        let full = serde_json::to_value(json!({
            "type": "workspace_list",
            "workspaces": [{
                "workspace_id": "w3", "number": 1, "label": "x", "focused": false,
                "pane_count": 1, "tab_count": 1, "active_tab_id": "t1",
                "agent_status": "unknown",
            }],
        }))
        .unwrap();
        serde_json::from_value::<WorkspaceListEnvelope>(full.clone())
            .expect("the control case must parse, or this test pins nothing");

        let mut absent = full.clone();
        absent["workspaces"][0]
            .as_object_mut()
            .unwrap()
            .remove("label");
        let mut null = full.clone();
        null["workspaces"][0]["label"] = Value::Null;
        // And a response that is not a `workspace_list` at all. `session/list`
        // degrades this to "nothing owned" (see `unreadable_is_empty`); what
        // is pinned here is only that it does not *parse*.
        let wrong_shape = json!({ "type": "ok" });

        for bad in [absent, null, wrong_shape] {
            let parsed: Result<WorkspaceListEnvelope, _> = serde_json::from_value(bad.clone());
            assert!(parsed.is_err(), "must not deserialize: {bad}");
        }
    }

    #[test]
    fn a_token_owns_a_workspace_whose_label_says_nothing() {
        // #417 D4 renames the workspace to `{repo}: {title}`, which no longer
        // starts with `totsuka `. The token is what keeps it ours.
        let response = workspace_list(&[
            json!({ "workspace_id": "w1", "label": "web: Fix the bug",
                    "tokens": { "totsuka_task": "42", "repo": "web" } }),
            // An empty token value is not a task id.
            json!({ "workspace_id": "w2", "label": "scratch",
                    "tokens": { "totsuka_task": "" } }),
            json!({ "workspace_id": "w3", "label": "scratch", "tokens": { "repo": "web" } }),
        ]);
        let owned = owned_workspaces(&response.workspaces);
        assert_eq!(owned.tokens.get("w1").copied(), Some("42"));
        assert_eq!(owned.tokens.len(), 1, "{owned:?}");
        assert!(
            owned.labels.is_empty(),
            "the rename left no `totsuka ` label"
        );
    }

    #[test]
    fn token_value_cuts_on_char_boundaries() {
        // `&s[..80]` panics here; most task titles in this repository are
        // Japanese, so this is the ordinary case rather than an edge one.
        let long: String = "あ".repeat(200);
        let cut = token_value(&long);
        assert_eq!(cut.chars().count(), TOKEN_VALUE_CHARS);
        assert!(cut.ends_with('…'), "the cut is visible: {cut}");

        // Exactly at the limit is not cut.
        let exact: String = "あ".repeat(TOKEN_VALUE_CHARS);
        assert_eq!(token_value(&exact), exact);

        // Whitespace runs collapse, so an embedded newline cannot eat a
        // sidebar row.
        assert_eq!(token_value("  a\n\tb  "), "a b");
        assert_eq!(token_value(""), "");
    }

    #[test]
    fn identity_token_names_satisfy_herdrs_identifier_rules() {
        // herdr: `^[A-Za-z0-9_-]{1,32}$`, at most 16 tokens per call. Pinned
        // the same way `agent_name_satisfies_herdrs_identifier_rules` is —
        // a name herdr rejects fails the whole report, silently, at dispatch.
        let names = [IDENTITY_TOKEN, "repo", "task", "mode"];
        assert!(names.len() <= 16);
        for name in names {
            assert!(
                (1..=32).contains(&name.len())
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name}"
            );
        }
    }

    #[test]
    fn workspace_is_read_off_the_pane_id() {
        assert_eq!(workspace_of("w1:p2"), Some("w1"));
        assert_eq!(workspace_of("w1T:p10"), Some("w1T"));
        // A hand-written session id without the herdr shape names no workspace
        // (the pane still closes; only the workspace sweep is skipped).
        assert_eq!(workspace_of("bare-pane"), None);
    }

    fn dispatch_params(title: &str, body: Option<&str>) -> TaskDispatchParams {
        TaskDispatchParams {
            task: plugin_protocol::task::Task {
                id: "C1:1.0".into(),
                source: "slack".into(),
                title: title.into(),
                body: body.map(str::to_string),
                repo_hint: None,
                labels: vec![],
                priority: 0,
                status: None,
                url: None,
                assignee: None,
                message_key: None,
                instructions: None,
            },
            worktree_path: "/wt".into(),
            mode: plugin_protocol::methods::ExecutionMode::Plan,
            extra_context: None,
            job_id: None,
            resume_session_id: None,
            tool_launch: None,
            repo_name: None,
        }
    }

    #[test]
    fn compose_prompt_skips_the_truncated_title_when_a_body_exists() {
        // Sources truncate the title to a snippet; the body carries the full
        // text. Typing both showed a cut-off duplicate first line in the pane.
        let params = dispatch_params(
            "Slack: tomoya in #dev: エイリアスはどのフ",
            Some("full task body"),
        );
        assert_eq!(compose_prompt(&params), "full task body");

        // Title-only tasks still get a prompt.
        let params = dispatch_params("bare title", None);
        assert_eq!(compose_prompt(&params), "bare title");
    }

    #[test]
    fn compose_prompt_puts_string_extra_context_first_as_raw_text() {
        // A string extra_context (e.g. core's marker self-report instruction) is
        // a PREAMBLE: the task text must stay last so the prompt's tail — what
        // submit_prompt matches on screen — stays unique per task (a constant
        // suffix would false-match the previous turn's prompt on a resume pane).
        // Raw text: quotes inside the instruction (e.g. reason="...") must come
        // through unescaped, with no JSON wrapping around the whole string.
        let mut params = dispatch_params("t", Some("unique task body"));
        params.extra_context = Some(Value::String(
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked".into(),
        ));
        let prompt = compose_prompt(&params);
        assert_eq!(
            prompt,
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked\n\n---\nunique task body"
        );
        assert!(
            prompt.ends_with("unique task body"),
            "the task text is the tail"
        );
        assert!(
            !prompt.contains("\\\""),
            "quotes are not JSON-escaped: {prompt}"
        );

        // Non-string values keep their JSON rendering (still as preamble).
        params.extra_context = Some(serde_json::json!({"base": "main"}));
        assert!(compose_prompt(&params).starts_with("{\"base\":\"main\"}\n\n---\n"));
    }

    #[test]
    fn resolve_launch_uses_tool_launch_verbatim() {
        // #196: the Orchestrator's fully-resolved argv/env is launched as-is.
        // `resume_session_id` is set here precisely because the plugin must
        // NOT act on it — the resume flag is already inside `args` if it
        // belongs there.
        let mut params = dispatch_params("t", None);
        params.resume_session_id = Some("sess-9".into());
        params.tool_launch = Some(plugin_protocol::methods::ToolLaunchSpec {
            program: "claude".into(),
            args: vec!["--resolved".into()],
            env: std::collections::BTreeMap::from([(
                "TOTSUKA_JOB_ID".to_string(),
                "job-1-2".to_string(),
            )]),
        });
        let (program, args, env) = resolve_launch(&params).unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resolved".to_string()]);
        assert_eq!(env, Some(serde_json::json!({"TOTSUKA_JOB_ID": "job-1-2"})));

        // An empty tool_launch env stays absent: no `env` key sent to herdr.
        params.tool_launch.as_mut().unwrap().env.clear();
        let (_, _, env) = resolve_launch(&params).unwrap();
        assert_eq!(env, None);
    }

    #[test]
    fn a_dispatch_without_tool_launch_fails_instead_of_improvising() {
        // #411: the local argv fallback is gone. Launching `claude` without
        // the workflow's `--settings` would produce a pane that runs and never
        // reports completion, so this must fail loudly at dispatch instead.
        let params = dispatch_params("t", None);
        let err = resolve_launch(&params).unwrap_err();
        assert!(matches!(err, HerdrError::MissingToolLaunch), "{err}");
    }

    /// The pieces of an [`agent_name`] that herdr's identifier rules constrain.
    /// Every assertion here is a rule `agent.start` enforces, verified live:
    /// `"totsuka probe"` was rejected as `invalid_agent_name`.
    #[test]
    fn agent_name_satisfies_herdrs_identifier_rules() {
        let legal = |name: &str| {
            let mut chars = name.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && name.len() <= 32
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        };

        // The two shapes that actually reach this plugin. Slack's colon and
        // GitHub's upper case are exactly what the old `totsuka <id>` name
        // failed on.
        for id in [
            "C0BNAU8KKG8:1754236800.123456",
            "I_kwDOTrfAp88AAAABLKoO_Q",
            "42",
        ] {
            let name = agent_name(id);
            assert!(legal(&name), "{id} produced an illegal name: {name}");
        }

        // Degenerate ids still have to produce something legal: an id that is
        // all punctuation leaves no prefix, and a hash-only name would start
        // with a digit half the time.
        for id in ["", ":::", "::9"] {
            let name = agent_name(id);
            assert!(legal(&name), "{id:?} produced an illegal name: {name}");
        }
    }

    /// Truncation is what makes a bare prefix unsafe, so the suffix has to
    /// separate ids that share their first 21 characters — the case that would
    /// otherwise point two tasks at one agent.
    #[test]
    fn agent_name_separates_ids_sharing_a_prefix() {
        let a = agent_name("C0BNAU8KKG8:1754236800.111111");
        let b = agent_name("C0BNAU8KKG8:1754236800.222222");
        assert_ne!(a, b);
        // …and is stable, because a re-dispatch of the same task has to
        // compute the same name.
        assert_eq!(a, agent_name("C0BNAU8KKG8:1754236800.111111"));
    }

    /// The prefix is only worth carrying if it is still readable, which is the
    /// whole reason the name is not just a hash.
    #[test]
    fn agent_name_keeps_a_readable_prefix() {
        let name = agent_name("C0BNAU8KKG8:1754236800.123456");
        assert!(
            name.starts_with("t-c0bnau8kkg8-"),
            "prefix was not preserved: {name}"
        );
    }

    /// `kind` comes from the program's file name, so an absolute path resolves
    /// the same as a bare command — the Orchestrator sends whichever the
    /// `[tools]` registry produced.
    #[test]
    fn resolve_kind_uses_the_programs_file_name() {
        let config: HerdrConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(resolve_kind(&config, "claude"), "claude");
        assert_eq!(
            resolve_kind(&config, "/Users/x/.local/bin/claude"),
            "claude"
        );
    }

    /// `[kind_map]` is the escape hatch for a wrapper whose name herdr does not
    /// know; without it such a program reaches `agent.start` as an unknown
    /// `kind` and is rejected there.
    #[test]
    fn resolve_kind_honours_the_kind_map() {
        let config: HerdrConfig =
            serde_json::from_value(serde_json::json!({ "kind_map": { "my-claude": "claude" } }))
                .unwrap();
        assert_eq!(resolve_kind(&config, "/opt/bin/my-claude"), "claude");
        // Unmapped names still pass through unchanged — this table overrides,
        // it does not gate.
        assert_eq!(resolve_kind(&config, "codex"), "codex");
    }
}
