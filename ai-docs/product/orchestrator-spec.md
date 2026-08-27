---
type: Spec
title: totsuka — Local AI-Agent Orchestrator Requirements (v1)
description: Requirements specification for the totsuka orchestrator CLI — task-source/agent-IDE/notifier plugins, git-worktree lifecycle, workflows, parallel execution control, and v1 scope.
tags: [orchestrator, requirements, plugin, worktree, cli, rust]
generated: { by: claude-code/opus-5, at: 2026-08-28T06:50:00+09:00 }
status: draft
owner: tomoya-k31
---

> 🌐 **English** · [日本語](orchestrator-spec.ja.md)

# totsuka — Local AI-Agent Orchestrator Requirements Specification

> **This file is the source for the human-facing `docs/orchestrator-spec.md`.** After changing it, regenerate that page with the `human-docs` skill; `scripts/docs-freshness.sh` checks this in CI.
<!-- generates: docs/orchestrator-spec.md -->

- Status: Draft v0.2 (imported from `orchestrator-requirements.md`)
- Product name: totsuka (十束剣)
- Created: 2026-07-12
- Target platform: macOS (Linux / Windows in the future)
- Implementation language: Rust

---

## 1. Overview

An orchestrator CLI application that connects task-management tools — such as Notion tasks or GitHub Issues tied to GitHub Projects — as **task-source plugins**, and issues instructions to **Agent IDE plugins** such as herdr / orca against locally cloned repositories based on that information. Conflict-free parallel execution on the same repository is achieved via git worktree; detailed design and implementation are delegated to agents.

As a basic principle it adopts the **1 task = 1 repo = 1 worktree** normalization (following the Nanatsusaya model). Output is not necessarily a PR; per the workflow definition (§4.9) it branches into PR creation, write-back to the task source, and so on. The definition of "done" for each task also follows its workflow definition. This application is positioned as the local, single-machine edition: it has no server-side event bus and no resident service.

## 2. Goals

| Goal | Success metric (example) |
|---|---|
| Let humans focus on requirements definition, design review, and implementation review | Ratio of tasks where human involvement is limited to "review only" |
| Increase development throughput via parallel execution | Number of concurrent tasks (target: 3–5 parallel per developer) |
| Freedom to swap tools | Agent IDEs / task sources can be added without modifying the core |
| Team rollout | Other members can onboard with configuration-file distribution alone |

## 3. Scope

> Answer to "what needs to be decided": scope fixes the three points **In / Out / Assumptions**. If this is vague the functional requirements balloon without bound, so making explicit what will NOT be built in v1 is the top priority.

### 3.1 In Scope (v1)

- Task-source plugins: GitHub Issues / Projects and Notion (2 kinds)
- Agent IDE plugins: herdr and orca (2 kinds)
- Notifier plugin (official macOS notification plugin bundled)
- git worktree lifecycle management (creation, branch naming, cleanup)
- Automatic repository selection (rule-based + LLM fallback via an AI gateway)
- Parallel execution control (global / per-repository limits)
- Plugin install / uninstall / enable / disable
- XDG Base Directory–compliant configuration, state, and log management
- Status inspection and operations via CLI
- Status display on a local GUI host (a menu bar, for one): totsuka emits text, the host does the drawing (F-109)

### 3.2 Out of Scope (v1)

| Item | Reason |
|---|---|
| Web dashboard / cloud UI | State stays local. Text output for a local GUI host is in scope per §3.1; a TUI may still be considered later as P2 |
| PR review automation, merge decisions, merge tracking | Human review territory. No tracking after PR creation |
| Guaranteed Linux / Windows support | Abstraction only; implementation and testing are out of scope |
| Resident daemon / server operation | Limited to a locally launched lifecycle |
| Cloud sync / cross-team state sharing | State is local-only. Sharing is delegated to GitHub / Notion |
| Implementing the agents themselves (code-generation logic) | Fully delegated to Agent IDE plugins |
| Repository cloning / credential management | Repositories are assumed pre-cloned; git auth uses the existing environment |

### 3.3 Assumptions

- Target repositories are already cloned locally and registered by path in the configuration file
- Agent IDEs such as herdr / orca are installed separately by the user
- macOS 14+, git 2.40+

## 4. Functional Requirements

Priorities use MoSCoW (M: Must / S: Should / C: Could / W: Won't in v1).

### 4.1 Task acquisition (task-source plugins)

| ID | Requirement | Priority |
|---|---|---|
| F-01 | Task sources connect as plugins; task lists and details are retrieved in a normalized common schema (Task). A source names the workflow each task belongs to on `task/submit` (0.6.0, #554) — it has already run first-match over the workflows it was given, and the Orchestrator verifies only that the name exists and belongs to that source | M |
| F-02 | GitHub Issues / Projects plugin (GraphQL API, reads Projects status columns) | M |
| F-03 | Notion plugin (database property mapping defined in configuration) | M |
| F-04 | Per-plugin output (field mapping, filter conditions) definable in the configuration file | M |
| F-05 | Write statuses such as task done / in progress back to the source (bidirectional sync) | S |
| F-08 | **Intake confirmation/control for multi-user usage is the task-source plugin's role** (strict mutual exclusion not required). E.g. check assignee presence / in-progress status so tasks another member is working on are not picked up. On top of the read-side gates, sources declaring the `task_claim` capability are asked to **claim the task via `task/claim` right before dispatch**; a lost claim steps aside as `skipped` instead of running (optimistic exclusion; undeclaring sources behave as before) | M |
| F-06 | Configurable polling interval (webhooks unsupported in v1 since this is a local app) | S |
| F-07 | **Result write-back (`result/publish` RPC)**: a task source *may* write the orchestrator's artifact back — the destination and formatting are the plugin's responsibility, and a plugin that implements it declares the `source` output capability. **Not every source does.** Where the agent can write the deliverable itself (a `gh` comment, a Notion page), it does, and the plugin declares nothing; the RPC is for sources where the orchestrator has to mediate — Slack, whose reply goes out under the operator's own name, gated by approval unless the workflow's `publish = "direct"` opts that gate out (#548) | M |

**Task common schema (proposal)**: `id, source, title, body, repo_hint, labels, priority, status, url, assignee`

### 4.2 Repository selection

| ID | Requirement | Priority |
|---|---|---|
| F-10 | If the task specifies a repo (Notion property / the Issue's repository, etc.), that takes precedence | M |
| F-11 | Otherwise, classify with an LLM using the configured repository summaries + the head N lines of each repository root README | M |
| F-12 | LLM calls use an OpenAI-compatible API; swapping `base_url` selects an AI gateway such as OpenRouter / LiteLLM | M |
| F-13 | Model name, max_tokens, and timeout configurable (cheap models assumed, e.g. haiku-class) | M |
| F-14 | The LLM returns `{repo, confidence, reason}` via structured output. Confidence is treated as a self-reported reference value; when multiple candidates are close, ask a human (put the task into pending) | S |
| F-15 | Cache README summaries (XDG_CACHE_HOME); regenerate only when the README hash changes | C |

### 4.3 Worktree management

| ID | Requirement | Priority |
|---|---|---|
| F-20 | Create a worktree in the target repository when a task starts (1 task = 1 worktree = 1 branch) | M |
| F-21 | **The agent names and creates the task's branch**, following the repository's own convention. The worktree is handed over on a detached `HEAD`; the orchestrator learns the branch by reading `HEAD` back, and never generates a name — a generated name cannot follow a convention written inside the repository | M |
| F-22 | Worktree location configurable (default: `<state dir>/worktrees/{repo_name}/{worktree_name}`, where `{worktree_name}` renders `{source}-{task_id}`). The directory name is derived from the task, never from the branch — the branch does not exist yet when the worktree is created | M |
| F-23 | Worktree cleanup on task completion/cancellation configurable as a policy (immediate / retention period / manual) | M |
| F-24 | Detect orphan worktrees at startup (those with no corresponding task in the state DB) and offer cleanup via the `doctor` command | S |
| F-25 | Run `git fetch` immediately before worktree creation and check out `origin/{default_branch}` detached (prevents starting from stale local branches). Base branch overridable per repository. The commit is recorded so cleanup can tell this task's branch from the operator's | M |

### 4.4 Agent IDE integration (Agent IDE plugins)

| ID | Requirement | Priority |
|---|---|---|
| F-30 | Abstract Agent IDEs as plugins; the agent to use is switchable per task type and per repository via configuration | M |
| F-31 | Dispatch interface: pass worktree path, task body, execution mode (`plan` / `implement`), and extra context | M |
| F-32 | Agent state (idle / running / waiting_input / done / failed) is retrievable. herdr uses its Socket API; orca hides its own mechanism inside the plugin. **Note:** herdr + Claude Code completion detection is replaced by the hook mechanism (§4.11, F-100–F-107); the herdr state stream is retained only for `pane.exited` deadman detection | M |
| F-33 | **Capability negotiation**: plugins declare their supported features (`pane_control`, `state_stream`, `hook_completion`, etc.) and the orchestrator only requests supported features | M |
| F-36 | In `plan` mode, the plugin maps to each agent's plan / read-mostly mode and runs it. Artifacts (design documents) are returned to the orchestrator as structured results (used for write-back per the workflow's output policy) | M |
| F-37 | **Session management**: on dispatch, obtain the agent's session identifier (conversation history ID), associate it with the task, and persist it in the state DB. `session/attach` is a required method of agent_ide plugins; on orchestrator restart / task resume, re-attach to the existing session | M |
| F-38 | The plugin carries agent execution logs as fragments in `state/subscribe` notifications; the orchestrator persists them tagged with task_id (source for `logs --task <id>`). **Note:** herdr + Claude Code completion detection is replaced by the hook mechanism (§4.11, F-100–F-107); the herdr state stream is retained only for `pane.exited` deadman detection | M |
| F-34 | In detailed-design mode, request supporting plugins to show a design preview (separate pane / side screen). The display mechanism is the plugin's responsibility | S |
| F-35 | Detect questions from the agent to a human (waiting_input), show them in `status`, and deliver the event to notifier plugins (§4.10). **Note:** herdr + Claude Code completion detection is replaced by the hook mechanism (§4.11, F-100–F-107); the herdr state stream is retained only for `pane.exited` deadman detection | M |

### 4.5 Parallel execution control

| ID | Requirement | Priority |
|---|---|---|
| F-40 | Configurable global concurrency limit | M |
| F-41 | Per-repository concurrency limit (worktrees don't conflict, but this moderates CI / review load) | M |
| F-42 | Per-Agent-IDE-plugin limit (matches tool-side session-count constraints) | S |
| F-43 | Queueing and priority control (respect task priority, FIFO fallback) | S |
| F-44 | Individual cancel/retry of running tasks. On retry, recreate the worktree if missing; if it exists, keep it and resume the conversation using the task's previous agent session ID (F-37) | M |
| F-45 | Only the states `dispatched → running → verifying → publishing` count toward concurrency limits (`verifying` = human-verification pending, agent work done but output not yet confirmed, so it keeps its slot). Waiting states such as `waiting_input` and `escalated` (awaiting human intervention) release their slot and reacquire one on resume (prevents effective deadlock by waiting) | M |

### 4.6 Plugin system

| ID | Requirement | Priority |
|---|---|---|
| F-50 | Plugin kinds: `task_source` / `agent_ide` / `notifier` (design allows future kinds) | M |
| F-51 | Plugins run as **separate processes communicating over JSON-RPC 2.0 via stdio (Unix socket in the future)** | M |
| F-52 | Provide install / uninstall / enable / disable / list as subcommands | M |
| F-53 | A plugin manifest (`plugin.toml`: name, kind, version, supported protocol version, capabilities) is mandatory | M |
| F-54 | Protocol-version compatibility check (explicit error on mismatch) | M |
| F-55 | Distribution: v1 supports local path / binary download from GitHub Releases. A registry is W | S |
| F-56 | **Separate install (binary presence) from enabled (config declaration)**. Install to `$XDG_DATA_HOME/totsuka/plugins/{name}/`; enable/disable is declared via the `[plugins.{name}] enabled` flag in config.toml. The roster is also what makes a top-level `[<name>]` settings table legitimate (F-64) | M |
| F-57 | `plugin enable / disable` are editing helpers that rewrite `enabled` in config.toml (toml_edit preserves comments/formatting). Direct edits to the config file must produce identical results | M |
| F-58 | Disabled plugins are never started. Configuration referring to a disabled plugin (a repository's default agent, etc.) is an error in `config validate` | M |
| F-59 | Validation of plugin-specific config is delegated via a mandatory `config/validate` RPC method on the plugin. `config validate` briefly starts all enabled plugins to let them validate (enables checks not expressible in a schema, e.g. socket connectivity). Since 0.6.0 the call also carries the workflows, projects and repositories the plugin was given, so it validates what it is asked about rather than what it remembered | M |

**Plugin configuration design policy (declarative + CLI as editing helper)**

The configuration file is the single source of truth; a config distributed via git to the team determines the working state as-is.

| Location | Responsibility |
|---|---|
| `$XDG_DATA_HOME/totsuka/plugins/{name}/` | Binary + manifest (target of install / uninstall) |
| `[plugins.{name}]` in `config.toml` | Enable/disable roster + common fields (`kind`, `max_concurrency`, `timeout_secs`, `log_level`, etc. — interpreted by the orchestrator) |
| `[<name>]` in `config.toml` | Plugin-specific config. The orchestrator does not interpret it; it is passed verbatim as JSON-RPC initialize params. Legitimate only when `<name>` is in the roster above |

```toml
# config.toml — the roster (interpreted by the orchestrator)
[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3
timeout_secs = 120

# …and the plugin's own settings, in the same file (uninterpreted)
[herdr]
socket_path = "${XDG_RUNTIME_DIR}/herdr.sock"
request_timeout_secs = 30
```

**Plugin mechanism comparison (decision rationale)**

| Approach | ABI stability | Language freedom | Fault isolation | Notes |
|---|---|---|---|---|
| dylib (cdylib) | ✗ Rust ABI unstable, depends on abi_stable | Effectively Rust only | ✗ crashes take the host down | Rejected |
| WASM (extism etc.) | ○ | ○ | ○ | Host I/O such as sockets/process spawning is heavy; ill-suited to driving Agent IDEs |
| **Subprocess + JSON-RPC** | ◎ process boundary | ◎ any language | ◎ | **Adopted**. Isomorphic to MCP / LSP; high affinity with the herdr Socket API |

### 4.7 Configuration

| ID | Requirement | Priority |
|---|---|---|
| F-60 | Configuration is TOML at `$XDG_CONFIG_HOME/totsuka/config.toml` (fallback `~/.config/totsuka/`) | M |
| F-61 | Repository definitions: path, summary text (for LLM selection), default agent, concurrency limit | M |
| F-62 | Secrets (Notion / GitHub / AI-gateway API keys) must not be plaintext in config. Support env-var references (`${ENV_VAR}` expansion) and macOS Keychain references | M |
| F-63 | `config validate` performs static validation (schema, path existence, plugin consistency) plus delegated validation to enabled plugins (F-59). `--offline` skips validation requiring plugin startup/connectivity and runs static checks only (for CI) | M |
| F-64 | Plugin-specific configuration lives in **config.toml**, in a top-level `[<name>]` table held uninterpreted by the Orchestrator. A table whose name is not in the `[plugins.*]` roster is a validation error, which catches a mistyped core key and a mistyped plugin name alike. Keys a plugin defines on a *core* structure (`[[workflows]]`, `[[projects]]`) are written flat and resolved by asking the plugins which ones they consume — see [ADR-0058](/decisions/adr-0058-config-ownership-boundary.md). Until #554 this requirement mandated a separate `plugins/{name}.toml`, which expressed ownership through file location and therefore could not reach inside a core structure | M |
| F-65 | Secret-reference resolution (`${ENV_VAR}` / `keychain:` prefix) happens **in the orchestrator**; resolved values are passed to plugins in initialize params. Plugins get no Keychain access | M |
| F-66 | Configuration precedence: CLI flags > environment variables > the plugin's own `[<name>]` table > defaults in `config.toml` | M |
| F-67 | Trackers a repository files into are declared as top-level `[[projects]]` entries (`name`, `source`, plus that plugin's own keys held uninterpreted), and a repository names **one** of them with `[[repositories]].project`. Optional — a repository with no tracker is the normal state. Writing `source` out is what makes the chain `repositories.project` → `projects.name` → `plugins.<source>` resolvable without launching a plugin. Replaces the reverse `repos = [...]` list each source used to keep (#554, [ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)) | M |

### 4.8 State management

| ID | Requirement | Priority |
|---|---|---|
| F-70 | Persist task execution state to SQLite (`$XDG_STATE_HOME/totsuka/state.db`); running-task state is restored after an app restart | M |
| F-71 | Implement state transitions as an explicit state machine. Common transitions: `queued → dispatched → running → publishing → done / failed / cancelled`. What `running` means (plan / implement) and what `publishing` means (PR creation / source write-back) is decided by the workflow definition (§4.9) | M |
| F-72 | Record each transition as an event log (audit/debugging) | S |
| F-73 | Intake idempotency: a unique constraint on `(source, source_task_id)` prevents double intake of the same task | M |
| F-74 | Prevent concurrent `run` instances: a lock file + PID under `$XDG_STATE_HOME/totsuka/`. `status` checks process liveness and clearly reports "orchestrator not running" with stale state when run is stopped | M |

### 4.9 Workflow definitions (trigger × mode × output policy)

On top of the same plugin binaries, any number of **named configurations — workflows — combining "which tasks to pick up, in which mode to run them, and where to send the result"** can be defined. Example: for the same GitHub Issue plugin, a "detailed design workflow" and an "implementation workflow" coexist. How many to define is up to the user.

| ID | Requirement | Priority |
|---|---|---|
| F-80 | Workflow = a named configuration of `source (task-source instance) × trigger (intake condition) × mode (plan / implement) × agent × output (output policy)`. Any number definable as `[[workflows]]` in config.toml | M |
| F-81 | Triggers are specified via Issue / Projects status columns or labels, Notion property values, etc. One task belongs to at most one workflow; **the source plugin decides which**, running first-match over the workflows supplied at `initialize` in definition order, and names it on `task/submit`. The Orchestrator holds the trigger as an opaque table and matches no task with it (0.6.0, #554). One key inside it is the Orchestrator's own: `status` names the source's status column, and it is read lexically to build the column graph the cycle check walks (#575) | M |
| F-82 | `mode = "plan"` (detailed design): a worktree IS created (for codebase reference) but the pane cannot run git at all, so it never branches, commits, pushes or opens a PR. The agent runs in plan mode and returns a design document as the artifact | M |
| F-83 | Output policy `output`: `source` (write to Issue comment, Notion page, etc. via the task-source plugin's `result/publish`) / `none`. Task-source plugins declare supported outputs as capabilities; realization is plugin-side. A `pull_request` policy existed until push and PR creation became the agent's responsibility (F-86) | M |
| F-84 | `on_success` / `on_failure`: transition the source-side status on completion (e.g. "awaiting design → awaiting design review"). This **source-side status transition is the handoff mechanism for plan → human review → implement**, naturally inserting human review between design and implementation | M |
| F-85 | Worktree cleanup policy for plan mode configurable separately from implement (immediate cleanup is the default for design-only) | S |
| F-86 | **Push and PR creation are the agent's responsibility**, following the repository's own conventions (which is where those procedures are written down). The orchestrator owns the worktree and the task lifecycle, and never pushes. `output = "pull_request"` was retired with this boundary — see [ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) for what this gives up | M |
| F-87 | `[[workflows]].initial_prompt`: extra instructions the operator writes in the config, prepended to the task body in the pane the **first** time a conversation starts (never on a resume, which would restart a skill mid-conversation). Literal text — no placeholder substitution. It is the first instruction channel scoped to a workflow rather than to a task source, so a flow no longer has to depend on its source plugin having a key for it (the Slack plugin's `reply_instructions` and friends still exist and are unchanged) | M |
| F-88 | A plugin may define its own keys on `[[workflows]]`, written **flat** beside the Orchestrator's. Ownership is resolved by asking: the leftover keys go to the workflow's `source` and `agent` at `initialize`, each answers which it consumes (`claimed_options`), and **exactly one** claimant is required — zero is a typo and fails startup, two is an ambiguity the Orchestrator refuses to settle. `run` and `config validate` both enforce it; `--offline` cannot (#554) | M |

**Configuration example**

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { status = "Ready to design" }
profile = "design"                       # resolves mode / output / verification
agent = "herdr"
on_success = { status = "Design review" }

[[workflows]]
name = "implement"
source = "github"
trigger = { status = "Ready to implement" }
profile = "implement"
agent = "herdr"
on_success = { status = "In review" }
```

### 4.10 Notifications (notifier plugins)

| ID | Requirement | Priority |
|---|---|---|
| F-90 | Notifier plugins receive orchestrator events (`waiting_input` / `done` / `failed` / `pending` (awaiting human confirmation of repository selection)) as JSON-RPC notifications and implement the delivery mechanism | M |
| F-91 | Bundle an official notifier for macOS Notification Center in v1 | M |
| F-92 | Notifications can be enabled/disabled per workflow and per event kind | S |
| F-93 | Notification failures must not affect task execution (fire-and-forget; errors logged only) | M |
| F-94 | **Clickable notifications → pane focus (click-to-focus)**: clicking a notification brings the GUI terminal to the front and focuses the pane of the task that raised it. Delivery uses the `terminal-notifier` backend (`-activate <bundle-id>` + `-execute 'totsuka focus <task_id>'` + `-group totsuka-<task_id>`; `-sender` is never combined with `-activate` — broken on Sequoia 15.x+). The focus path is `totsuka focus` → control UDS `POST /focus` → the task's agent_ide plugin's `session/focus` (protocol 0.1.4, gated on `pane_control`; the session id stays opaque, F-37). Every degradation is quiet: missing terminal-notifier falls back to osascript, a stopped orchestrator or vanished pane leaves only the app activation (see ADR-0005) | S |

### 4.11 Deterministic completion signal (Claude Code hooks)

Claude Code has no lifecycle authority, so herdr's screen-manifest completion detection is structurally lossy (latency, missed transitions, false positives). Completion is therefore reported **deterministically through Claude Code hooks**: a herdr pane runs `claude --settings <hooks_dir>/orchestrator-<workflow>.json [--resume <sid>]`, and command-type `Stop` / `Notification` / `SessionStart` / `SessionEnd` hooks POST to the orchestrator over a Unix domain socket (a `verification = "llm"` workflow additionally gets a prompt-type `Stop` hook that applies the rubric in-session, and a design/implement profile a `PreToolUse` hook for `AskUserQuestion`, F-108). This subsection is the requirements home for that mechanism; the end-to-end flow is diagrammed in `architecture/hook-signal-flow.md`, the placement decision is recorded in ADR-0004, and the config surface is `[hooks]` (`auth_token_ref` / `socket_path` / `spool_dir` / `block_retry_limit`) plus the per-workflow `verification` / `timeout_secs` / `rubric` keys.

| ID | Requirement | Priority |
|---|---|---|
| F-100 | **UDS receive**: the orchestrator receives completion signals on a Unix domain socket (mode `0600`) via a core driving adapter (`adapters::hook_uds`, a hand-rolled `UnixListener` + minimal HTTP/1.1). `POST /agent-events` with `Authorization: Bearer` constant-time compared to `[hooks].auth_token_ref`; body capped at 1 MiB; `job_id` required (else `400`). The receiver answers `200` immediately and processes asynchronously, normalizing the JSON body to `domain::signal::AgentSignal` via `ports::SignalPort` | M |
| F-101 | **Status-marker convention**: completion is self-reported by a marker on the last line of the assistant response (last-occurrence wins): `<<STATUS:COMPLETED>>` / `<<STATUS:NEEDS_INPUT reason="...">>` / `<<STATUS:FAILED reason="...">>` (the canonical form is doubled brackets, but the parser also accepts a single pair `<STATUS:...>` since real agents normalise the delimiters). Marker missing with `stop_hook_active=false` ⇒ the `Stop` hook `block`s to make Claude re-emit; `stop_hook_active=true` ⇒ post `UNKNOWN` without blocking. A non-empty `background_tasks` yields a heartbeat only (intermediate Stop, never a completion) | M |
| F-102 | **Verification** (`verification = "llm"` (default) / `"human"` / `"none"`): `llm` runs an in-session prompt-type `Stop` hook (rubric) — on a `COMPLETED` receipt the engine goes straight to Publishing; `human` parks the task in `Verifying` awaiting `totsuka task verify --pass/--fail`; `none` publishes directly. **Tool-capability degradation**: the prompt-type `Stop` hook only exists on Claude-kind tools (`ToolCapabilities.prompt_verification`), so for a task whose resolved tool is codex/opencode the effective mode degrades from `llm` to `human` when the completion arrives — it parks in `Verifying` rather than publishing unverified output, and the run log carries one `warn`. `config validate` warns upfront so the pin can be made explicit (#301) | M |
| F-103 | **Escalation**: 3 consecutive `UNKNOWN` stops (recomputed from the DB — the hook self-report is never trusted; `[hooks].block_retry_limit`, default 3) OR 30 min of silence since the last signal (workflow `timeout_secs` override) OR a correlation anomaly ⇒ the task enters `Escalated` (non-terminal) with a notifier notification and a `diagnostics/snapshot` (herdr `pane.read`) | M |
| F-104 | **Spool + at-least-once + idempotency**: on POST failure the hook retries twice, then appends an NDJSON line under `spool_dir`; the engine's `replay_spool()` re-submits it on `recover()` and each cycle. `hook_events UNIQUE(job_id, tool_session_id, prompt_id, event, status)` drops duplicate / out-of-order POSTs (multi-fire, spool resend, curl retry). `status` is part of the key so that the two consecutive `Stop`s of a block → re-completion cycle (`UNKNOWN`, then `COMPLETED`) stay distinct; without it the second is dropped as a duplicate and the completion is lost (#154, state.db v3 — SQLite cannot alter a UNIQUE constraint in place, so the migration rebuilds the table). A corrupt spool entry is quarantined (renamed `.corrupt`), not deleted | M |
| F-105 | **Conversation continuity**: a conversation *is* a task. `Task.id` identifies the conversation (Slack: `channel:thread_ts`) and `Task.message_key` one delivery within it, so a follow-up mention appends to the SAME task — sharing its worktree, branch and agent session — instead of correlating two tasks. Unprocessed messages are concatenated into one dispatch; a terminal task reopens (`Reopen`). An unusable session is reported as `SESSION_UNRESUMABLE` and retried once without it. A signal is routed by its `job_id`'s task, never guessed from a session id (E-09). Supersedes the `thread_key` correlation of #140, removed in protocol 0.3.0 (#242/#264) | M |
| F-106 | **Deadman**: the herdr `events.subscribe` stream is reduced to `pane.exited` deadman detection only; a herdr process crash surfaces as `Failed` | M |
| F-107 | **Pane post-processing**: a pane's lifetime tracks its worktree's cleanup policy, not the task's state ([ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)). The pane is closed (`session/release`) only where `decide_cleanup` answers `Remove` — so under the **default `manual`** it is never closed, which is deliberate: the pane is the review surface for committed-but-unpushed work. `Retain` / `Dirty` keep it for the same reason, and `Failed` / `Escalated` panes are retained for diagnosis. The one exception is a **re-dispatch** of the same task, which closes a retained pane first (#481) — a second pane for one task is not a review surface. Panes left behind are `totsuka doctor`'s job (#211). **This supersedes the original wording** (`Done` panes auto-close via idempotent `task/cancel`): nothing has ever called `task/cancel` on completion — its only caller is the `state/subscribe`-failure rollback in `dispatch_one` | M |
| F-108 | **Question-pending park** (#487, ADR-0050): design / implement (attended) profiles ask the human through the tool's native question UI — claude `AskUserQuestion` (a `PreToolUse` hook rendered only into those profiles' settings), opencode `question` (the plugin's `tool.execute.before`, which also suppresses idle judgement while the dialog waits); codex has no default-mode question tool and keeps `NEEDS_INPUT` with a numbered choice list. While the dialog waits the turn does not end and no `Stop` arrives, so the hook POSTs `QuestionPending` (per-question `prompt_id` — required, or a second question dedups away) and the engine parks the task in `waiting_input` exactly like `Stop{NEEDS_INPUT}`: slot released, operator notified with the question text; a *new* question while parked re-notifies. Markers stay the completion wire signal (ADR-0020) and `NEEDS_INPUT` + numbered list remains the in-prompt fallback when the question tool is unavailable | M |

### 4.12 Menu-bar display

The path to a surface that is **always in view**, such as the macOS menu bar. Notifications (§4.10) are transient — miss one and it is gone — whereas this stays. totsuka draws no GUI of its own; it only emits text a GUI host can read (§3.1).

| ID | Requirement | Priority |
|---|---|---|
| F-109 | **Menu-bar display (`totsuka menu`)**: two channels — the **glyph** is availability (`○` = healthy / `⚠` = running but degraded (F-110) / `✕` = stopped or a stale lock) and the **number** is the **attention** count (`glossary/attention.md`: `pending` / `waiting_input` / `verifying` / `escalated` / `queued` + `wait_reason`; terminal states are never counted). The dropdown has two sections, attention and working; clicking a task row runs `totsuka focus <id>`, and the rest hand off to a terminal through SwiftBar's `terminal=true`. The default output is SwiftBar's plugin format; `--json` emits the display model itself. **Row rendering belongs to the Rust side**: SwiftBar's format is `text \| key=value`, where `\|` is a metacharacter, so a single `\|` in a source-controlled title would let its author append parameters to the row (the same class of problem as #280). **Always exits 0** — a menu-bar plugin that exits non-zero renders as a broken item, so a missing state DB, pending migrations, or XDG paths that will not resolve all become a row in the menu instead; it never reads `config.toml`. The contract reaches outside the command's own body: `main` dispatches it ahead of the shared `Cx::resolve`, and it writes through a path that does not panic (see ADR-0065) | S |
| F-110 | **Runtime health (`health.json`)**: each cycle, `run` replaces `$XDG_STATE_HOME/totsuka/health.json` via temp+rename, and deletes it on a graceful shutdown. It carries **only degradations that can be re-asked every cycle** — a failed hook-receiver bind, a stopped plugin (`abandoned` changes what the operator should do), a spool backlog (`*.jsonl` only; quarantined `.corrupt` files are never collected automatically, so counting them would pin the warning on forever), and a 401/403 from the LLM gateway (a latch any success clears). One-off failures (an undelivered notification, a cleanup that could not run) cannot be re-asked and so would never clear; they stay in the log. Prose is not stored — the reader builds it — and an unknown `kind` still gets a row. `totsuka status` shows a `degraded:` block (`health` in `--json`) and `totsuka menu` shows `⚠`. **`run.lock` decides first**: a document left behind by a stopped run is ignored, and the pid is cross-checked, because that document describes a process that no longer exists. Readers also judge **freshness**: a document whose pid is alive but which has not been republished for 120 s is treated as stale and surfaced as a degradation of its own rather than dropped (dropping it would report "healthy" about a run that has gone quiet). Staleness is the reader's judgement, not something the run publishes, and `status --json` exposes both `health.recorded_at` and `health.stale`. It is a file rather than a table because raising the schema version would stop an older totsuka from opening the database at all (ADR-0017) | S |

## 5. Non-Functional Requirements

### 5.1 Launch / CLI

- Launched from the terminal as a single binary. No daemonization (foreground execution; a TUI-like summary during `run` is a future consideration).
- Startup time: within 1 second for read-only commands like `status`.

**CLI command structure (proposal)**

| Command | Purpose |
|---|---|
| `init` | Generate configuration scaffolding, environment check |
| `setup` | Interactive first-time setup from a recipe (added after this table was first written; see the setup playbook) |
| `run [--watch] [--json]` | Main loop from task intake (push, `task/submit`) to dispatch (one-shot by default; `--watch` stays up receiving pushes until shutdown — see Open Question #2, resolved) |
| `status [--json]` | List running / queued / waiting tasks and worktrees, plus what the live run cannot currently do (F-110) |
| `menu [--json]` | The menu-bar view (F-109). SwiftBar plugin format by default, the display model with `--json`. Always exits 0 |
| `task list / show <id> / cancel <id> / retry <id>` | Individual task operations |
| `task export [--since <event_id>] [--task <id>] [--no-detail]` | Stream the audit log (`events`) to stdout as NDJSON. The state of record lives in SQLite, so this is the way out in a form other tools can read; the table is append-only, which makes `--since` a complete incremental cursor |
| `plugin list / install / uninstall / enable / disable` | Plugin management |
| `config validate / show [--redacted]` | Config validation/display (secrets masked) |
| `doctor` | Environment diagnosis (git version, orphan worktrees, plugin connectivity, API key connectivity) |
| `logs [-f] [--task <id>]` | Log viewing / tailing |
| `completion <shell>` | Shell completion generation |
| Common flags | `--debug`, `--json`, `--dry-run`, `--config <path>` |

`--json` is available on the read-only commands that print a document — `status`, `task list`, `task show`, `plugin list`, `doctor` — to enable use from other tools (jq, CI, a future TUI). **It is a flag, not a universal rule**: `task export` (#463) is read-only and machine-readable but has **no** `--json`, because NDJSON is its only output format and a flag implying an alternative would be a lie. `run` (#462) is the mirror case — not read-only, but it carries `--json` because a caller that cannot parse the result cannot put `totsuka run` in a pipeline; it does not change the exit code either (a run that correctly recorded a failing task did its job, so `failed > 0` still exits 0), so the caller decides with e.g. `jq -e '.stats.failed == 0'`. The invariant that actually holds is the stdout contract: **with machine-readable output selected, stdout carries the document and nothing else.**

`--dry-run` is a zero-side-effect no-op as of protocol 0.2.0: since every task_source pushes rather than being fetched on demand, there is nothing to preview ahead of time — run without `--dry-run` to see live ingestion. **`run --dry-run --json` is refused at parse time** (exit 2): with nothing to preview, a JSON envelope would promise a machine-readable preview that does not exist.

### 5.2 Logging

- Output to `$XDG_STATE_HOME/totsuka/logs/`. Daily rotation + configurable retention count.
- Structured logs (JSON Lines) using the `tracing` crate. Human-friendly formatting via the `logs` command.
- Levels: error / warn / info / debug / trace. `--debug` outputs debug and above.
- **Sensitive-data masking is mandatory**: API keys, tokens, and Authorization headers are unconditionally redacted in the logging layer. Prompt bodies are output only at debug and above, and can be disabled in configuration.

### 5.3 Reliability / recovery

- After abnormal termination (including SIGKILL), a restart restores running tasks and agent session IDs (F-37) from the state DB and attempts reconnection via `session/attach`. Only when reconnection is impossible is "continue confirmation / mark failed" left to a human.
- API calls to task sources and the AI gateway retry with exponential backoff.
- Detect plugin-process crashes and transition the task to failed (the orchestrator itself is not taken down).

### 5.4 Security

- Secrets live only in the Keychain or environment variables. Never written to the state DB, logs, or caches.
- Plugins are arbitrary code execution: at install time, show the source and checksum and ask for confirmation.
- External transmission is limited to task-source APIs / AI gateway / Agent IDEs. No telemetry is collected.

### 5.5 Performance

- `status` responds within 500 ms at the scale of 100 tasks / 10 repositories.
- Worktree creation must not become a bottleneck up to the parallel limit (worktree creation is not serialized).

### 5.6 Portability (future-proofing)

- Abstract paths, Keychain, and process management behind traits; isolate implementations in a `platform::macos` module.
- Respect XDG even on macOS (explicitly adopt XDG compliance rather than the `dirs` crate's macOS defaults) to lower the Linux migration cost.

## 6. Technical Requirements

| Item | Content |
|---|---|
| Language | Rust (edition 2024, stable toolchain) |
| Main crates (proposal) | tokio, clap, serde, toml, toml_edit, tracing, rusqlite, reqwest, keyring |
| Dependency policy | Not minimalist, but avoid bloat. Anything likely to be swapped (JSON-RPC layer, persistence, secret store) must sit behind ports traits so crate choices can change later. The JSON-RPC layer starts as a thin hand-rolled serde_json + tokio implementation, migrating to a library as requirements dictate |
| Architecture | Hexagonal. `core` (domain, state machine) / `ports` (TaskSource, AgentIde, LlmRouter, SecretStore traits) / `adapters` (JSON-RPC plugin bridge, SQLite, Keychain) |
| Workspace layout | `orchestrator-core` / `orchestrator-cli` / `plugin-protocol` (type-definition crate published for plugin developers) / each official plugin crate |
| Plugin management | Binary + manifest in `$XDG_DATA_HOME/totsuka/plugins/{name}/`. enable/disable via config-side flags |
| AI gateway | Assumes OpenAI-compatible `/chat/completions`; `base_url` / `model` / `api_key_ref` configurable |

## 7. UI/UX Requirements

- totsuka draws no GUI of its own. CLI output quality is defined as the UX — and a text format meant to be fed to a GUI host (a menu bar, for one) is treated as one more CLI output under that same definition (F-109).
- Error messages always include "cause + next action" (e.g. `config not found → run 'app init'`).
- `--debug` outputs information needed during development (RPC payloads, state transitions, LLM decision rationale). Sensitive data follows the §5.2 masking policy.
- Output respects the NO_COLOR environment variable and non-TTY.

## 8. Content Requirements

> Answer to "what should be written": for a CLI tool, content means **all text the user reads**. The following are defined as deliverables.

| Content | Description |
|---|---|
| CLI help / error messages | Establish writing conventions (tone, terminology, English-only or bilingual). v1 recommends English UI + Japanese README |
| README | Overview, installation, quick start (run one task in 5 minutes) |
| Configuration reference | Every config.toml key with its default value |
| Plugin development guide | Protocol spec (JSON-RPC method list), manifest spec, sample plugin |
| Operations guide | Reading doctor output, worktree cleanup, troubleshooting FAQ |
| CHANGELOG | Keep a Changelog format, tied to semver |

Define a glossary (Task / Source / Agent / worktree / dispatch, etc.) and use it consistently across logs, documentation, and code.

## 9. Testing and Quality Assurance

| Layer | Content |
|---|---|
| Unit tests | State-machine transitions, repository-selection logic (rule part), config parsing, masking. cargo test |
| Automated integration tests | Provide **mock plugins** (fake task_source / fake agent_ide binaries for testing) and verify the JSON-RPC boundary with real processes. Test the worktree lifecycle on real git repositories in tempdirs |
| E2E | Run the full path "task fetch → worktree → dispatch → done → cleanup" in CI using a GitHub test repository + fake agent. LLM calls stubbed via recording (VCR style) |
| Manual integration tests | Connectivity with real herdr / orca, design-preview display, waiting_input detection. Pre-release checklist |
| Quality gates | clippy (deny warnings), rustfmt, cargo-audit / cargo-deny (dependency vulnerabilities & licenses), coverage measurement (llvm-cov) |

## 10. Deployment and Maintenance

### 10.1 Distribution

- **A Homebrew tap (recommended once live)**, the universal-binary tarball on GitHub Releases, and `cargo install`. The tap is not the working path yet — a formula fetches its URL with unauthenticated curl, so it stays inert until this repository is public; the tarball flow is what the README documents until then. An earlier revision put package managers out of scope for v1; [ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md) reverses that — the tarball path costs five `sudo` commands and offers no upgrade path at all, which is the first gate a new user hits.
- macOS Gatekeeper: ad-hoc signing + a procedure document suffices for internal distribution. For public release, plan Developer ID signing + notarization (a decision needed for v1).

### 10.2 Versioning / compatibility

- The app itself uses semver. **The plugin protocol is versioned independently**; manifests declare the compatible range. Breaking changes bump the major version, and the previous protocol is supported for one generation.
- The config schema carries a version key. A mismatch is raised by startup validation — shared by `totsuka config validate`, `totsuka run`, and `totsuka doctor` — and the config is never rewritten automatically. The first two stop with an error; `doctor` reports it as a failed check and carries on with the remaining diagnostics. No migration is offered — the schema is still at v1, so there is nothing to migrate. See [config reference](/development/config-reference.md) for what has to be decided before v2 can be cut.

### 10.3 Updates / operations

- `--version` and a pointer to release notes. self-update is out of scope for v1 — `brew upgrade totsuka` for a tap install, otherwise re-download the tarball or `cargo install` again.
- State-DB schema migrations (embedded migrations, auto-applied at startup + backup).
- Watching for changes in dependent APIs (Notion / GitHub / herdr Socket API) is defined as a maintenance task. Plugin separation allows tracking without a core release.
- Issue template + attaching `doctor --json` output as the reporting flow.

### 10.4 Team rollout

- Distribute configuration templates via an internal repository (secrets are per-person Keychain / env).
- Onboarding: keep it to 5 steps — install (download / `cargo install`) → `init` → set keys → `doctor` → `run`.

---

## 11. Appendix A: Minimal plugin-protocol method set (v0)

> This method set is an initial version. **Continuous tuning is assumed** as AI-IDE-side specs and handling requirements evolve; changes are managed via the protocol versioning of F-54.

| Method | Direction | Kind | Purpose |
|---|---|---|---|
| `initialize` | O→P | common | Exchange plugin-specific config (including resolved secrets) and capabilities. `workflows` (the `[[workflows]]` naming this plugin as `source` or `agent`) goes to every kind; `projects` / `repositories` go to `task_source` (renamed from `triggers` in 0.6.0, #554; `poll_interval_secs` moved into the plugin's own `[<name>]` table) |
| `shutdown` | O→P | common | Termination request |
| `config/validate` | O→P | common | Validate plugin-specific config (F-59) |
| `task/submit` | **P→O request** | task_source | Push a task the plugin found (persist-before-ack, protocol 0.1.6). Replaces the removed `tasks/fetch` as of protocol 0.2.0 — every task_source is push-only |
| `task/update_status` | O→P | task_source | Source-side status transition (F-84) |
| `result/publish` | O→P | task_source | Write the artifact back, for the sources that implement it (F-07) |
| `task/dispatch` | O→P | agent_ide | Pass worktree, task, and mode; start execution. Returns a session ID |
| `task/cancel` | O→P | agent_ide | Cancel execution |
| `session/attach` | O→P | agent_ide | Reconnect to an existing session (F-37) |
| `state/subscribe` → notification | P→O | agent_ide | Stream of state transitions and log fragments (F-38) |
| `notify` (notification) | O→P | notifier | Event delivery (F-90) |

O = Orchestrator, P = Plugin

## 12. Open Questions

| # | Topic | Decider |
|---|---|---|
| 1 | Whether design-review / implementation-review approval happens in this app's CLI or only via GitHub / Notion operations (the trigger releasing the `waiting_review` transition) | Product owner |
| 2 | ~~Whether `run` is one-shot or `--watch` resident by default~~ → **Resolved (2026-07-12)**: one-shot is the default; `--watch` enables resident polling | Resolved |
| 3 | Whether herdr pane control (design preview) is a required v1 capability or a herdr-plugin-only extension | Architect |
| 4 | ~~Whether one task auto-chains design → implement~~ → **Resolved**: the workflow model (§4.9) is adopted. plan and implement are separate workflows; the source-side status transition (a human operation) is the handoff | Resolved |
| 5 | Whether public release is planned (affects signing / notarization / license selection) | Business decision |
| 6 | Future integration / role division with Nanatsusaya (the server edition) — whether protocol unification is needed | Architect |
