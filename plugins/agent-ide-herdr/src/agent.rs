//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.
//!
//! Protocol facts this adapter is written against (herdr 0.7.5 / protocol 17,
//! verified live in [ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md),
//! mirrored in `docs/references/herdr-socket-api.md`):
//! - the agent CLI is launched with `agent.start {name, kind, pane_id, args}`
//!   into a pane the **caller** supplies. `kind` picks the executable, `name` is
//!   an identifier (`[a-z][a-z0-9_-]{0,31}`, unique among live agents), and
//!   `cwd`/`env`/`argv` are not accepted at all
//! - the hook environment (`TOTSUKA_JOB_ID`, … from a
//!   [`HookLaunchSpec`](plugin_protocol::methods::HookLaunchSpec)) therefore
//!   rides on `workspace.create`, whose `env` herdr applies to the root pane —
//!   which is the pane the agent is started in
//! - a freshly created pane is **not immediately usable**: its shell is still
//!   starting, and `agent.start` types the launch command into it regardless.
//!   There is no readiness signal to poll — `pane.process_info` shows the shell
//!   from the moment it is *spawned*, which is not the moment it starts
//!   *accepting input* — so the call is retried. The race surfaces in three
//!   shapes and all three are one bug (#387): `agent_pane_busy` and `timeout`
//!   on `agent.start`, and a start that is accepted while the agent never
//!   becomes addressable, which only `agent.prompt` can see
//!   (`agent_not_ready`). Keystrokes typed into a shell that was not reading
//!   are **lost, not queued**, so waiting longer never helps; only re-issuing
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
//! reduction is unconditional (it holds even when no hook spec is supplied); an
//! orchestrator older than 0.1.3 will therefore not learn of completion, which
//! `initialize` warns about.

use plugin_protocol::methods::{
    AgentState, DiagnosticsSnapshotResult, ExecutionMode, SessionAttachResult, SessionFocusResult,
    SessionInfo, SessionListResult, SessionReleaseParams, SessionReleaseResult, StateNotification,
    TaskDispatchParams, TaskDispatchResult,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, map_agent_status};
use crate::transport::{HerdrTransport, SUBSCRIPTION_CLOSED_EVENT};

/// How many screen lines are read when extracting text from a pane.
const SCREEN_LINES: u64 = 200;

/// How long `agent.prompt` is given to observe the agent reacting, in
/// milliseconds ([ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-5).
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
        let (program, args, env) = resolve_launch(&self.config, &params);

        // A workspace per task keeps agent panes out of the operator's own
        // workspaces (there is no "start session" method; the workspace is the
        // container).
        //
        // `env` goes here and **only** here (protocol 17): `agent.start` no
        // longer takes one. It still reaches the agent because herdr applies a
        // workspace's env to its root pane, which is the pane the agent is
        // started in (D-4). A pane made by `pane.split` inherits nothing, which
        // is why the companion shell never sees `TOTSUKA_HOOK_TOKEN`.
        let mut create_params = json!({
            "cwd": params.worktree_path,
            "label": format!("totsuka {}", params.task.id),
        });
        if let Some(env) = &env {
            create_params["env"] = env.clone();
        }
        let created = self.client.call("workspace.create", create_params).await?;
        let workspace = NewWorkspace::from_response(&created)?;

        // From here on the workspace exists, so every failure path has to take
        // it back down: a failed dispatch reports no session id, which leaves
        // the Orchestrator no handle to cancel with — the pane and its CLI
        // process would run until the operator noticed them (and `task retry`
        // would strand another one).
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
    /// [ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-4).
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

        let start_params = json!({
            "name": agent_name(&params.task.id),
            "kind": resolve_kind(&self.config, &program),
            "pane_id": pane_id,
            "args": args,
            "timeout_ms": AGENT_START_TIMEOUT_MS,
        });
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
                Err(e) if e.is_agent_not_ready() && tokio::time::Instant::now() < deadline => {
                    tracing::warn!(
                        pane_id,
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
            .and_then(|pane| agent_session_id(&pane))
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
    /// [ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-4).
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
    /// (`docs/security/hook-security.md`), and a pane made by `pane.split`
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
                json!({
                    "target_pane_id": agent_pane_id,
                    "direction": layout.direction.as_str(),
                    // herdr's `ratio` is the *split source's* share, and the
                    // source here is the agent's pane — so the configured
                    // agent share goes across unchanged.
                    "ratio": layout.ratio,
                    "cwd": cwd,
                    "focus": false,
                }),
            )
            .await
        {
            tracing::warn!(
                pane_id = agent_pane_id, error = %e,
                "could not add the companion shell pane; the agent runs full-screen"
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
                .call("workspace.close", json!({ "workspace_id": workspace_id }))
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
    /// [ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-5).
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
        let params = json!({
            "target": pane_id,
            "text": prompt,
            // `working` alone would be a race on a turn short enough to settle
            // before herdr samples again; the other two are the settled states
            // that also mean "it read the prompt".
            "wait": {
                "until": ["working", "blocked", "done"],
                "timeout_ms": PROMPT_WAIT_MS,
            },
        });
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
        match self
            .client
            .call(
                "agent.wait",
                json!({
                    "target": pane_id,
                    "until": ["working", "blocked", "done"],
                    "timeout_ms": PROMPT_CONFIRM_MS,
                }),
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

    /// Re-attach to a dispatched session (F-37): confirm the pane is alive with
    /// `pane.get` and report its current mapped state. A vanished pane
    /// (`pane_not_found`) is reported as `attached: false` (the Orchestrator's
    /// recovery then defers to a human).
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        match pane_status(&self.client, &handle.pane_id).await {
            Ok(status) => Ok(SessionAttachResult {
                attached: true,
                state: map_agent_status(&status, AgentState::Idle),
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
                json!({ "pane_id": handle.pane_id, "keys": ["ctrl+c"] }),
            )
            .await
            && !e.is_missing()
        {
            tracing::warn!(error = %e, "pane.send_keys failed during cancel; closing anyway");
        }
        self.close_pane_and_workspace(&handle).await
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
            // Already gone (e.g. cancel closed it): nothing to release. The
            // workspace is intentionally left alone too — with the pane
            // unverifiable, `workspace_of` might name someone else's.
            Err(e) if e.is_missing() => return Ok(SessionReleaseResult { released: false }),
            Err(e) => return Err(e),
        };
        let checks = [
            ("cwd", params.expect_cwd.as_deref(), pane_str(&pane, "cwd")),
            (
                "label",
                params.expect_label.as_deref(),
                pane_str(&pane, "label"),
            ),
        ];
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
                return Ok(SessionReleaseResult { released: false });
            }
        }
        if !comparable && (params.expect_cwd.is_some() || params.expect_label.is_some()) {
            tracing::debug!(
                pane_id = %handle.pane_id,
                "identity unverifiable (pane reports none of the expected fields); closing anyway"
            );
        }
        self.close_pane_and_workspace(&handle).await?;
        Ok(SessionReleaseResult { released: true })
    }

    /// Enumerate the live panes this plugin owns (`session/list`, #211):
    /// `pane.list` filtered to panes whose `label` carries the `totsuka `
    /// marker `dispatch` sets on `workspace.create`. The label filter is the
    /// ownership boundary — herdr serves human-opened panes too, and those
    /// must never be listed as release candidates. A pane without a label
    /// (or with someone else's) is simply not ours.
    ///
    /// The returned `session_id` encodes the pane with an **empty** agent
    /// session id: `pane.list` does not say which Claude session runs inside,
    /// and `session/release` only needs the pane (`SessionHandle::decode`
    /// accepts the bare form).
    pub async fn list_sessions(&self) -> Result<SessionListResult, HerdrError> {
        let result = self.client.call("pane.list", json!({})).await?;
        let sessions = result
            .get("panes")
            .and_then(Value::as_array)
            .map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        pane_str(pane, "label").is_some_and(|label| label.starts_with("totsuka "))
                    })
                    .filter_map(|pane| {
                        let pane_id = pane_str(pane, "pane_id")?;
                        Some(SessionInfo {
                            session_id: SessionHandle::new(pane_id, "").encode(),
                            label: pane_str(pane, "label").map(str::to_string),
                            cwd: pane_str(pane, "cwd").map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SessionListResult { sessions })
    }

    /// Close a session's pane and the task-private workspace `dispatch`
    /// created for it. `dispatch` gives every task its own workspace, so
    /// closing the pane alone would leave an empty one behind. Idempotent:
    /// anything already gone counts as success.
    async fn close_pane_and_workspace(&self, handle: &SessionHandle) -> Result<(), HerdrError> {
        ignore_missing(
            self.client
                .call("pane.close", json!({ "pane_id": handle.pane_id }))
                .await,
        )?;
        if let Some(workspace_id) = workspace_of(&handle.pane_id) {
            ignore_missing(
                self.client
                    .call("workspace.close", json!({ "workspace_id": workspace_id }))
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
        // Container ids come from the pane record; the workspace also falls
        // back to the pane-id prefix (`w1:p2` lives in `w1`). A record without
        // a tab id just skips the tab step — `pane.focus` still lands.
        let workspace_id = pane
            .get("workspace_id")
            .and_then(Value::as_str)
            .or_else(|| workspace_of(&handle.pane_id));
        if let Some(workspace_id) = workspace_id
            && !self
                .focus_step("workspace.focus", json!({ "workspace_id": workspace_id }))
                .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        if let Some(tab_id) = pane.get("tab_id").and_then(Value::as_str)
            && !self
                .focus_step("tab.focus", json!({ "tab_id": tab_id }))
                .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        let focused = self
            .focus_step("pane.focus", json!({ "pane_id": handle.pane_id }))
            .await?;
        Ok(SessionFocusResult { focused })
    }

    /// One focus call: `Ok(true)` on success, `Ok(false)` when the target is
    /// gone (the pane/tab/workspace closed between the liveness check and this
    /// call), and the error otherwise.
    async fn focus_step(&self, method: &str, params: Value) -> Result<bool, HerdrError> {
        match self.client.call(method, params).await {
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
        let text = read_pane_text(&self.client, &handle.pane_id, "recent", SCREEN_LINES).await;
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

/// What `dispatch` needs out of a `workspace.create` response: the workspace
/// itself, and the root pane herdr opens it with.
///
/// The root pane is where the agent is started (protocol 17), and the response
/// is the only handle to it — the response names it (`root_pane`) and nothing else
/// does: `pane.list` cannot distinguish it from the agent's pane by label,
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
    fn from_response(created: &Value) -> Result<Self, HerdrError> {
        let id = created
            .get("workspace")
            .and_then(|w| w.get("workspace_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HerdrError::InvalidResponse("workspace.create returned no workspace_id".into())
            })?
            .to_string();
        let root_pane_id = created
            .get("root_pane")
            .and_then(|p| p.get("pane_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self { id, root_pane_id })
    }
}

/// The pane record (`pane.get` nests it under `result.pane`).
async fn pane_record<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<Value, HerdrError> {
    let result = client
        .call("pane.get", json!({ "pane_id": pane_id }))
        .await?;
    Ok(result.get("pane").cloned().unwrap_or(result))
}

/// A string field off a pane record, `None` when absent or null (herdr's
/// `PaneInfo.cwd`/`label` are both optional-and-nullable).
fn pane_str<'a>(pane: &'a Value, field: &str) -> Option<&'a str> {
    pane.get(field).and_then(Value::as_str)
}

/// The pane's current `agent_status`.
async fn pane_status<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<String, HerdrError> {
    let pane = pane_record(client, pane_id).await?;
    Ok(pane
        .get("agent_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// The workspace a pane belongs to. herdr ids nest the workspace in the pane
/// (`w1:p2` lives in `w1`), which is the only handle back to it — the protocol
/// `session_id` carries the pane, not the workspace.
fn workspace_of(pane_id: &str) -> Option<&str> {
    pane_id.split_once(':').map(|(workspace, _)| workspace)
}

/// The agent's native session id from a pane record
/// (`pane.agent_session.value`), reported by the agent's herdr integration hook
/// during startup and carried in the [`SessionHandle`] for `claude --resume`.
/// Absent or empty → `None`.
fn agent_session_id(pane: &Value) -> Option<String> {
    let value = pane
        .get("agent_session")
        .and_then(|s| s.get("value"))
        .and_then(Value::as_str)?;
    (!value.is_empty()).then(|| value.to_string())
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
    source: &str,
    lines: u64,
) -> Option<String> {
    let result = client
        .call(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": source,
                "lines": lines,
                "strip_ansi": true,
            }),
        )
        .await
        .ok()?;
    result
        .get("read")
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
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
/// ([ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-2).
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

/// The herdr `kind` for `program`
/// ([ADR-0032](../../../docs/decisions/adr-0032-herdr-protocol-17.md) D-1).
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
/// The deprecated `hook`-driven [`HerdrConfig::launch_command`] path remains
/// as a fallback for older orchestrators only.
fn resolve_launch(
    config: &HerdrConfig,
    params: &TaskDispatchParams,
) -> (String, Vec<String>, Option<Value>) {
    match &params.tool_launch {
        Some(tool) => (
            tool.program.clone(),
            tool.args.clone(),
            (!tool.env.is_empty()).then(|| json!(tool.env)),
        ),
        None => {
            let plan = params.mode == ExecutionMode::Plan;
            let hook_settings = params.hook.as_ref().map(|h| h.settings_path.as_str());
            let resume = params.resume_session_id.as_deref();
            let (program, args) = config.launch_command(plan, hook_settings, resume);
            // herdr injects this env into the launched process
            // (workspace.create + agent.start both take `env`). Only set when
            // a hook spec is present.
            (program, args, params.hook.as_ref().map(|h| json!(h.env)))
        }
    }
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
            hook: None,
            tool_launch: None,
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
    fn resolve_launch_prefers_tool_launch_verbatim() {
        // #196: a 0.2.3 orchestrator's fully-resolved argv/env is launched
        // as-is — the plugin-local launch_command must NOT re-append
        // `--settings`/`--resume` even though hook/resume fields are set.
        let config: HerdrConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut params = dispatch_params("t", None);
        params.resume_session_id = Some("sess-9".into());
        params.hook = Some(plugin_protocol::methods::HookLaunchSpec {
            settings_path: "/hooks/orchestrator-wf.json".into(),
            env: std::collections::BTreeMap::from([(
                "TOTSUKA_JOB_ID".to_string(),
                "job-1-2".to_string(),
            )]),
        });
        params.tool_launch = Some(plugin_protocol::methods::ToolLaunchSpec {
            program: "claude".into(),
            args: vec!["--resolved".into()],
            env: std::collections::BTreeMap::from([(
                "TOTSUKA_JOB_ID".to_string(),
                "job-1-2".to_string(),
            )]),
        });
        let (program, args, env) = resolve_launch(&config, &params);
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resolved".to_string()]);
        assert_eq!(env, Some(serde_json::json!({"TOTSUKA_JOB_ID": "job-1-2"})));

        // An empty tool_launch env stays absent (parity with the old
        // hookless launch: no env key sent to herdr at all).
        params.tool_launch.as_mut().unwrap().env.clear();
        let (_, _, env) = resolve_launch(&config, &params);
        assert_eq!(env, None);
    }

    #[test]
    fn resolve_launch_falls_back_to_launch_command() {
        // Pre-0.2.3 orchestrator: no tool_launch — the deprecated hook path
        // assembles the argv exactly as before.
        let config: HerdrConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut params = dispatch_params("t", None); // mode: Plan
        params.resume_session_id = Some("sess-9".into());
        params.hook = Some(plugin_protocol::methods::HookLaunchSpec {
            settings_path: "/hooks/orchestrator-wf.json".into(),
            env: std::collections::BTreeMap::new(),
        });
        let (program, args, env) = resolve_launch(&config, &params);
        assert_eq!(program, "claude");
        assert_eq!(
            args,
            vec![
                "--permission-mode".to_string(),
                "plan".to_string(),
                "--settings".to_string(),
                "/hooks/orchestrator-wf.json".to_string(),
                "--resume".to_string(),
                "sess-9".to_string(),
            ]
        );
        assert_eq!(env, Some(serde_json::json!({})));
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
