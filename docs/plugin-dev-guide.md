> 🌐 **English** · [日本語](plugin-dev-guide.ja.md)

<!-- generated-from: ai-docs/development/plugin-dev-guide.md sha256:05ed7a7e62f4bc2ddfe2075d9c3c5dd40cbcd7b64a3843c8411db9e5a8b285df -->

# Plugin development guide

How to write a totsuka plugin: the protocol, the manifest, the methods for each plugin kind, and the build-install loop.

## What a plugin is

A plugin is **a single executable that speaks JSON-RPC 2.0 over stdio, one message per line** (NDJSON). There are three kinds:

- `task_source` — supplies tasks
- `agent_ide` — drives an AI agent
- `notifier` — delivers notifications

The `plugin-protocol` crate is the single source of truth for the protocol and publishes every type you need.

## Dependency

```toml
[dependencies]
plugin-protocol = { git = "https://github.com/tomoya-k31/totsuka" }
```

It gives you `Task`, `InitializeParams` / `InitializeResult`, the params and result types for every method, `Manifest`, `Capabilities`, and JSON-RPC helpers. **The protocol version is independent of the application version.**

## The manifest

Every plugin ships a `plugin.toml` next to its binary.

```toml
name = "github"                     # must match the binary name
kind = "task_source"                # task_source | agent_ide | notifier
version = "0.1.0"                   # your plugin's own version
protocol_version = ">=0.1.6, <0.6"  # the orchestrator protocol range you support

[capabilities]                      # declare only what you actually implement
state_stream = true                 # agent: supports the state stream
pane_control = true                 # agent: can focus, release and list panes
hook_completion = true              # agent: reports completion via tool hooks
diagnostics_snapshot = true         # agent: answers diagnostics/snapshot
outputs = ["source"]                # task_source: supports publishing results
```

Before starting your plugin, the orchestrator checks `protocol_version` for compatibility and only asks for the capabilities you declared.

**Only keys the orchestrator actually reads exist.** Every capability field and error code is machine-checked to have a reader, so a key that does nothing cannot be added. Protocol 0.5.0 removed five that had none — `plan_mode`, `task_submit`, `resume_session`, and the error codes `-32001` / `-32002`. **An older manifest that still lists them starts fine**: unknown keys are ignored. But `resume_session` was *replaced* by `hook_completion`, so an agent that reports completion through hooks must declare it under the new name.

### Choosing the range

The **upper bound** goes at the next major or minor after the breaking change you want to stay below — currently `<0.6`. A manifest capping at `<0.3` is refused by a 0.3.0 orchestrator, one capping at `<0.4` is refused by 0.4.0, one capping at `<0.5` is refused by 0.5.0, and so on.

**The lower bound matters just as much, and it follows what you depend on — not your plugin's kind, and not whatever protocol version is newest.** The herdr plugin declares `>=0.2.3` because 0.2.3 is where the field it needs to launch tools was added, and it no longer has a fallback that builds the command line itself. Refusing older orchestrators is what makes that removed fallback **unreachable** rather than merely deprecated.

The orca plugin is the same kind and still declares `>=0.1.0`, because it drives the `orca` CLI and never reads that field. Raising its lower bound would reject orchestrators it works with perfectly well.

## Methods

**O→P** is an orchestrator-to-plugin call; **P→O** is plugin-to-orchestrator.

### Common to every kind

| Method | Direction | What it does |
|---|---|---|
| `initialize` | O→P | Passes resolved config and the protocol version; you return your version and capabilities |
| `config/validate` | O→P | Validates your plugin's configuration |
| `shutdown` | O→P | Asks you to exit, with a grace period |

`initialize` also hands a `task_source` several things it would otherwise have to configure twice. All are optional — ignore what you do not use.

- `repositories: [{name, summary?, path?}]` — the orchestrator's configured repositories, so a source that resolves repositories itself does not need its own copy
- `llm: {base_url, model, api_key?}` — the orchestrator's LLM settings with the key already resolved. If your plugin has its own LLM configuration, prefer that and treat this as the default
- `triggers: [{workflow, trigger}]` and `poll_interval_secs` — what to watch for and how often to look. Event-driven sources can ignore the interval

### task_source

**A task source is push-only.** When you find a task you send `task/submit` to the orchestrator yourself; there is no RPC where the orchestrator comes to fetch tasks. Event-driven sources (webhooks, sockets) submit on each event; sources that are naturally polled run their own timer from the `triggers` and `poll_interval_secs` you got in `initialize`. The `plugin-sdk` crate provides that timer as `poll_loop`.

| Method | Direction | What it does |
|---|---|---|
| `task/submit` | **P→O request** | Pushes a task you found. The orchestrator persists before acknowledging |
| `task/update_status` | O→P | Tells you the task moved, so you can reflect it in the source |
| `result/publish` | O→P | Hands you the result to write back to the source |

`task/submit` answers with one of three **final** outcomes, and you must not resend the same task because of any of them:

- `accepted` — persisted
- `duplicate` — the idempotency key collided; discard it
- `rejected` — permanently unprocessable, with a reason

Transport-level errors (`NOT_ACCEPTING`, `SUBMIT_OVERLOADED`, `INTERNAL_ERROR`) are different: submit is idempotent, so back off and retry those.

### agent_ide

| Method | Direction | What it does |
|---|---|---|
| `task/dispatch` | O→P | Start work in a worktree; return a session id |
| `task/cancel` | O→P | Cancel a running task |
| `session/attach` | O→P | Reattach to an existing session; return attached plus current state |
| `state/subscribe` | O→P | Subscribe to the state and log stream |
| `state/notification` | P→O | Report a state change or a log fragment |

**The worktree arrives on a detached HEAD.** Creating a branch, committing, pushing, and opening a pull request are all the agent's responsibility, not the orchestrator's.

`state` is one of `idle`, `running`, `waiting_input`, `done`, `failed`. The orchestrator maps these onto its own state machine — `running` starts the clock, `waiting_input` releases the concurrency slot, `done` moves to publishing — so map your tool's real state onto these five honestly.

### notifier

| Method | Direction | What it does |
|---|---|---|
| `notify` | O→P (no response) | Delivers an event: `waiting_input`, `done`, `failed`, or `pending` |

**A failed delivery must never affect task execution.**

## Building and installing

From a checkout, one command builds, installs, and enables.

```sh
totsuka plugin install --from-source github --enable      # just one
totsuka plugin install --from-source --all --enable       # everything
totsuka plugin install --from-source --all --profile dev  # debug build
```

The checkout is found by walking upwards from the current directory (or pass `--repo <dir>`). The test is "a Cargo workspace root that also has a `plugins/` directory" rather than asking git for the top level, which would happily answer inside an unrelated clone. The build runs cargo exactly once for all selected packages. Use `--print-plan` to see what would happen without invoking cargo.

### Doing it by hand

Each plugin is an ordinary member of the workspace under `plugins/{crate}/`, so build it from the workspace root.

```sh
cargo build --release -p task-source-github
```

Output lands in the shared `target/release/`, not in a per-crate directory.

**The binary is named after `plugin.toml`, not after the Cargo package.** Each plugin's `Cargo.toml` sets its `[[bin]] name` to the manifest's `name` — the `task-source-github` package produces a binary called `github` — and that is the name installation expects, so no renaming is needed. `scripts/arch-lint.sh` checks this automatically. If they do not match, installation fails with `plugin binary <name> not found in <dir> → expected a file named after the plugin`.

Installing from a directory requires the manifest and the binary to sit together.

```sh
mkdir -p dist/github
cp target/release/github plugins/task-source-github/plugin.toml dist/github/
totsuka plugin install ./dist/github
```

`--from-source` skips this staging step; it reads the manifest from the plugin's source directory and the binary straight out of `target/<profile>/`.

## Install and enable

- `totsuka plugin install <dir>` validates the directory (showing a SHA-256 for confirmation) and places it under `$XDG_DATA_HOME/totsuka/plugins/{name}/`
- `totsuka plugin enable {name}` sets `[plugins.{name}] enabled = true` in your config
- **Installing a binary and enabling it are deliberately separate steps**

Reinstalling **never overwrites the installed binary in place.** It writes a temporary file in the same directory and renames it over the old one, so the installed path gets a fresh inode every time. macOS caches code-signature verification per vnode, so rewriting the contents in place makes the next launch die silently with `SIGKILL`.

## Reference implementations

| Kind | Plugins |
|---|---|
| `task_source` | `task-source-github` (GraphQL), `task-source-notion` (REST with property mapping) |
| `agent_ide` | `agent-ide-herdr` (socket API adapter), `agent-ide-orca` (CLI wrapper) |
| `notifier` | `notifier-macos` (osascript) |

For a minimal skeleton, `crates/orchestrator-core/src/bin/mock_plugin.rs` plays every kind, driven by configuration.

## Checking it works

`totsuka config validate` delegates to your `config/validate` — unless you pass `--offline`, which keeps it to static checks and never launches a plugin. `totsuka doctor` probes your plugin live. Either will tell you whether your plugin starts and answers.

---

This page is generated from the internal document `ai-docs/development/plugin-dev-guide.md`, which carries the design decisions and measurements behind it.
