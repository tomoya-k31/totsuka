---
type: Spec
title: totsuka — Local AI-Agent Orchestrator Requirements (v1)
description: Requirements specification for the totsuka orchestrator CLI — task-source/agent-IDE/notifier plugins, git-worktree lifecycle, workflows, parallel execution control, and v1 scope.
tags: [orchestrator, requirements, plugin, worktree, cli, rust]
timestamp: 2026-07-26T12:00:00Z
status: draft
owner: tomoya-k31
---

> 🌐 **English** · [日本語](orchestrator-spec.ja.md)

# totsuka — Local AI-Agent Orchestrator Requirements Specification

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

### 3.2 Out of Scope (v1)

| Item | Reason |
|---|---|
| GUI / web dashboard | Terminal launch is assumed. A TUI may be considered later as P2 |
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
| F-01 | Task sources connect as plugins; task lists and details are retrieved in a normalized common schema (Task) | M |
| F-02 | GitHub Issues / Projects plugin (GraphQL API, reads Projects status columns) | M |
| F-03 | Notion plugin (database property mapping defined in configuration) | M |
| F-04 | Per-plugin output (field mapping, filter conditions) definable in the configuration file | M |
| F-05 | Write statuses such as task done / in progress back to the source (bidirectional sync) | S |
| F-08 | **Intake confirmation/control for multi-user usage is the task-source plugin's role** (strict mutual exclusion not required). E.g. check assignee presence / in-progress status so tasks another member is working on are not picked up | M |
| F-06 | Configurable polling interval (webhooks unsupported in v1 since this is a local app) | S |
| F-07 | **Result write-back (`result/publish` RPC)**: artifacts of detailed design (design documents etc.) can be written to the source — Issue comments, Notion page bodies, etc. Destination and formatting are the task-source plugin's responsibility | M |

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
| F-21 | Branch naming convention configurable (default: `agent/{source}-{task_id}`) | M |
| F-22 | Worktree location configurable (default: `{repo}/../.worktrees/{branch}` or under XDG_STATE_HOME) | M |
| F-23 | Worktree cleanup on task completion/cancellation configurable as a policy (immediate / retention period / manual) | M |
| F-24 | Detect orphan worktrees at startup (those with no corresponding task in the state DB) and offer cleanup via the `doctor` command | S |
| F-25 | Run `git fetch` immediately before worktree creation and branch from `origin/{default_branch}` (prevents branching from stale local branches). Base branch overridable per repository | M |

### 4.4 Agent IDE integration (Agent IDE plugins)

| ID | Requirement | Priority |
|---|---|---|
| F-30 | Abstract Agent IDEs as plugins; the agent to use is switchable per task type and per repository via configuration | M |
| F-31 | Dispatch interface: pass worktree path, task body, execution mode (`plan` / `implement`), and extra context | M |
| F-32 | Agent state (idle / running / waiting_input / done / failed) is retrievable. herdr uses its Socket API; orca hides its own mechanism inside the plugin. **Note:** herdr + Claude Code completion detection is replaced by the hook mechanism (§4.11, F-100–F-107); the herdr state stream is retained only for `pane.exited` deadman detection | M |
| F-33 | **Capability negotiation**: plugins declare their supported features (`plan_mode`, `design_preview`, `pane_control`, `state_stream`, etc.) and the orchestrator only requests supported features | M |
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
| F-56 | **Separate install (binary presence) from enabled (config declaration)**. Install to `$XDG_DATA_HOME/totsuka/plugins/{name}/`; enable/disable is declared via the `[plugins.{name}] enabled` flag in config.toml | M |
| F-57 | `plugin enable / disable` are editing helpers that rewrite `enabled` in config.toml (toml_edit preserves comments/formatting). Direct edits to the config file must produce identical results | M |
| F-58 | Disabled plugins are never started. Configuration referring to a disabled plugin (a repository's default agent, etc.) is an error in `config validate` | M |
| F-59 | Validation of plugin-specific config is delegated via a mandatory `config/validate` RPC method on the plugin. `config validate` briefly starts all enabled plugins to let them validate (enables checks not expressible in a schema, e.g. socket connectivity) | M |

**Plugin configuration design policy (declarative + CLI as editing helper)**

The configuration file is the single source of truth; a config distributed via git to the team determines the working state as-is.

| Location | Responsibility |
|---|---|
| `$XDG_DATA_HOME/totsuka/plugins/{name}/` | Binary + manifest (target of install / uninstall) |
| `[plugins.{name}]` in `config.toml` | Enable/disable roster + common fields (`kind`, `max_concurrency`, `timeout_secs`, `log_level`, etc. — interpreted by the orchestrator) |
| `plugins/{name}.toml` | Plugin-specific config. The orchestrator does not interpret it; it is passed verbatim as JSON-RPC initialize params |

```toml
# config.toml
[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3
timeout_secs = 120
```

```toml
# plugins/herdr.toml (plugin-specific)
socket_path = "${XDG_RUNTIME_DIR}/herdr.sock"
design_preview = "side_pane"
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
| F-64 | Plugin-specific configuration is separated into `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml` (common fields in config.toml, specific fields in individual files; see F-56) | M |
| F-65 | Secret-reference resolution (`${ENV_VAR}` / `keychain:` prefix) happens **in the orchestrator**; resolved values are passed to plugins in initialize params. Plugins get no Keychain access | M |
| F-66 | Configuration precedence: CLI flags > environment variables > `plugins/{name}.toml` > defaults in `config.toml` | M |

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
| F-81 | Triggers are specified via Issue / Projects status columns or labels, Notion property values, etc. One task must match at most one workflow at a time (multiple matches produce a `config validate` warning; precedence is definition order) | M |
| F-82 | `mode = "plan"` (detailed design): a worktree IS created (for codebase reference) but **no push and no PR creation**. The agent runs in plan mode and returns a design document as the artifact | M |
| F-83 | Output policy `output`: `pull_request` (push + create PR) / `source` (write to Issue comment, Notion page, etc. via the task-source plugin's `result/publish`) / `none`. Task-source plugins declare supported outputs as capabilities; realization is plugin-side | M |
| F-84 | `on_success` / `on_failure`: transition the source-side status on completion (e.g. "awaiting design → awaiting design review"). This **source-side status transition is the handoff mechanism for plan → human review → implement**, naturally inserting human review between design and implementation | M |
| F-85 | Worktree cleanup policy for plan mode configurable separately from implement (immediate cleanup is the default for design-only) | S |
| F-86 | With `output = "pull_request"`, push and PR creation are the **orchestrator's responsibility** (gh CLI or GitHub API). The agent's responsibility ends at committing; this boundary is stated in the plugin protocol spec | M |

**Configuration example**

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"                        # write results to an Issue comment
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
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

Claude Code has no lifecycle authority, so herdr's screen-manifest completion detection is structurally lossy (latency, missed transitions, false positives). Completion is therefore reported **deterministically through Claude Code hooks**: a herdr pane runs `claude --settings <hooks_dir>/orchestrator-<workflow>.json [--resume <sid>]`, and command-type `Stop` / `Notification` / `SessionStart` / `SessionEnd` hooks POST to the orchestrator over a Unix domain socket (a `verification = "llm"` workflow additionally gets a prompt-type `Stop` hook that applies the rubric in-session). This subsection is the requirements home for that mechanism; the end-to-end flow is diagrammed in `architecture/hook-signal-flow.md`, the placement decision is recorded in ADR-0004, and the config surface is `[hooks]` (`auth_token_ref` / `socket_path` / `spool_dir` / `block_retry_limit`) plus the per-workflow `verification` / `timeout_secs` / `rubric` keys.

| ID | Requirement | Priority |
|---|---|---|
| F-100 | **UDS receive**: the orchestrator receives completion signals on a Unix domain socket (mode `0600`) via a core driving adapter (`adapters::hook_uds`, a hand-rolled `UnixListener` + minimal HTTP/1.1). `POST /agent-events` with `Authorization: Bearer` constant-time compared to `[hooks].auth_token_ref`; body capped at 1 MiB; `job_id` required (else `400`). The receiver answers `200` immediately and processes asynchronously, normalizing the JSON body to `domain::signal::AgentSignal` via `ports::SignalPort` | M |
| F-101 | **Status-marker convention**: completion is self-reported by a marker on the last line of the assistant response (last-occurrence wins): `<<STATUS:COMPLETED>>` / `<<STATUS:NEEDS_INPUT reason="...">>` / `<<STATUS:FAILED reason="...">>` (the canonical form is doubled brackets, but the parser also accepts a single pair `<STATUS:...>` since real agents normalise the delimiters). Marker missing with `stop_hook_active=false` ⇒ the `Stop` hook `block`s to make Claude re-emit; `stop_hook_active=true` ⇒ post `UNKNOWN` without blocking. A non-empty `background_tasks` yields a heartbeat only (intermediate Stop, never a completion) | M |
| F-102 | **Verification** (`verification = "llm"` (default) / `"human"` / `"none"`): `llm` runs an in-session prompt-type `Stop` hook (rubric) — on a `COMPLETED` receipt the engine goes straight to Publishing; `human` parks the task in `Verifying` awaiting `totsuka task verify --pass/--fail`; `none` publishes directly | M |
| F-103 | **Escalation**: 3 consecutive `UNKNOWN` stops (recomputed from the DB — the hook self-report is never trusted; `[hooks].block_retry_limit`, default 3) OR 30 min of silence since the last signal (workflow `timeout_secs` override) OR a correlation anomaly ⇒ the task enters `Escalated` (non-terminal) with a notifier notification and a `diagnostics/snapshot` (herdr `pane.read`) | M |
| F-104 | **Spool + at-least-once + idempotency**: on POST failure the hook retries twice, then appends an NDJSON line under `spool_dir`; the engine's `replay_spool()` re-submits it on `recover()` and each cycle. `hook_events UNIQUE(job_id, tool_session_id, prompt_id, event)` drops duplicate / out-of-order POSTs (multi-fire, spool resend, curl retry). A corrupt spool entry is quarantined (renamed `.corrupt`), not deleted | M |
| F-105 | **Conversation continuity**: a conversation *is* a task. `Task.id` identifies the conversation (Slack: `channel:thread_ts`) and `Task.message_key` one delivery within it, so a follow-up mention appends to the SAME task — sharing its worktree, branch and agent session — instead of correlating two tasks. Unprocessed messages are concatenated into one dispatch; a terminal task reopens (`Reopen`). An unusable session is reported as `SESSION_UNRESUMABLE` and retried once without it. A signal is routed by its `job_id`'s task, never guessed from a session id (E-09). Supersedes the `thread_key` correlation of #140, removed in protocol 0.3.0 (#242/#264) | M |
| F-106 | **Deadman**: the herdr `events.subscribe` stream is reduced to `pane.exited` deadman detection only; a herdr process crash surfaces as `Failed` | M |
| F-107 | **Pane post-processing**: `Done` panes auto-close (idempotent `task/cancel`); `Failed` / `Escalated` panes are retained for diagnosis | M |

## 5. Non-Functional Requirements

### 5.1 Launch / CLI

- Launched from the terminal as a single binary. No daemonization (foreground execution; a TUI-like summary during `run` is a future consideration).
- Startup time: within 1 second for read-only commands like `status`.

**CLI command structure (proposal)**

| Command | Purpose |
|---|---|
| `init` | Generate configuration scaffolding, environment check |
| `run [--watch]` | Main loop from task intake (push, `task/submit`) to dispatch (one-shot by default; `--watch` stays up receiving pushes until shutdown — see Open Question #2, resolved) |
| `status [--json]` | List running / queued / waiting tasks and worktrees |
| `task list / show <id> / cancel <id> / retry <id>` | Individual task operations |
| `plugin list / install / uninstall / enable / disable` | Plugin management |
| `config validate / show [--redacted]` | Config validation/display (secrets masked) |
| `doctor` | Environment diagnosis (git version, orphan worktrees, plugin connectivity, API key connectivity) |
| `logs [-f] [--task <id>]` | Log viewing / tailing |
| `completion <shell>` | Shell completion generation |
| Common flags | `--debug`, `--json`, `--dry-run`, `--config <path>` |

`--json` is available on all read-only commands to enable use from other tools (jq, CI, a future TUI). `--dry-run` is a zero-side-effect no-op as of protocol 0.2.0: since every task_source pushes rather than being fetched on demand, there is nothing to preview ahead of time — run without `--dry-run` to see live ingestion.

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

- No GUI. CLI output quality is defined as the UX.
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

- **Universal binaries (arm64 / x86_64) on GitHub Releases (recommended)** + `cargo install`. Package managers (Homebrew, etc.) are out of scope for v1.
- macOS Gatekeeper: ad-hoc signing + a procedure document suffices for internal distribution. For public release, plan Developer ID signing + notarization (a decision needed for v1).

### 10.2 Versioning / compatibility

- The app itself uses semver. **The plugin protocol is versioned independently**; manifests declare the compatible range. Breaking changes bump the major version, and the previous protocol is supported for one generation.
- The config schema carries a version key. A mismatch is an error raised by startup validation (`totsuka config validate`, and the same check on `totsuka run`); the config is never rewritten automatically. No migration is offered — the schema is still at v1, so there is nothing to migrate. See [config reference](/development/config-reference.md) for what has to be decided before v2 can be cut.

### 10.3 Updates / operations

- `--version` and a pointer to release notes. self-update is out of scope for v1 (re-download the binary or `cargo install` again).
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
| `initialize` | O→P | common | Exchange plugin-specific config (including resolved secrets) and capabilities. For `task_source`, also carries `triggers`/`poll_interval_secs` (protocol 0.1.6) |
| `shutdown` | O→P | common | Termination request |
| `config/validate` | O→P | common | Validate plugin-specific config (F-59) |
| `task/submit` | **P→O request** | task_source | Push a task the plugin found (persist-before-ack, protocol 0.1.6). Replaces the removed `tasks/fetch` as of protocol 0.2.0 — every task_source is push-only |
| `task/update_status` | O→P | task_source | Source-side status transition (F-84) |
| `result/publish` | O→P | task_source | Write back design results etc. (F-07) |
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
