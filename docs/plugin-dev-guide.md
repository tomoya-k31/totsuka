> 🌐 **English** · [日本語](plugin-dev-guide.ja.md)

<!-- generated-from: ai-docs/development/plugin-dev-guide.md sha256:2dc6a357d9a7ae259c164297c8a4652307c96402c0648a7cde43058524cde352 -->

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
protocol_version = ">=0.6.0, <0.8"  # the orchestrator protocol range you support

[capabilities]                      # declare only what you actually implement
state_stream = true                 # agent: supports the state stream
pane_control = true                 # agent: can focus, release and list panes
hook_completion = true              # agent: reports completion via tool hooks
diagnostics_snapshot = true         # agent: answers diagnostics/snapshot
outputs = ["source"]                # declare it only if you implement
                                   # result/publish; without it, a workflow
                                   # asking for output = "source" is rejected
```

Before starting your plugin, the orchestrator checks `protocol_version` for compatibility and only asks for the capabilities you declared.

**Only keys the orchestrator actually reads exist.** Every capability field and error code is machine-checked to have a reader, so a key that does nothing cannot be added. Protocol 0.5.0 removed five that had none — `plan_mode`, `task_submit`, `resume_session`, and the error codes `-32001` / `-32002`. **An older manifest that still lists them starts fine**: unknown keys are ignored. But `resume_session` was *replaced* by `hook_completion`, so an agent that reports completion through hooks must declare it under the new name.

### Choosing the range

The **upper bound** goes at the next major or minor after the breaking change you want to stay below — currently `<0.8`. A manifest capping at `<0.3` is refused by a 0.3.0 orchestrator, one capping at `<0.4` is refused by 0.4.0, `<0.5` by 0.5.0, `<0.6` by 0.6.0, `<0.7` by 0.7.0, and so on.

**The lower bound matters just as much, and it follows what you depend on — not your plugin's kind, and not whatever protocol version is newest.**

In 0.6.0 every bundled plugin ended up at `>=0.6.0`, but that is because they all depend on the same thing: `initialize` was renamed, and every plugin of every kind reads that call. Do not read the uniform result as a rule.

0.7.0 is the counter-example in the same release. It added the field that tells a workflow which of a source's domains to scan, and only the two plugins that serve several domains — github and notion — moved to `>=0.7.0`: a build that predates the field ignores it and scans **every** board it owns, so a narrowing an operator wrote silently does not happen. A source with one domain filters an identity, and agents and notifiers never read the field, so those five kept `>=0.6.0` and only widened the ceiling.

The cases that *don't* line up show the rule better. In 0.4.0 only the herdr plugin was raised, to `>=0.2.3`, because 0.2.3 is where the field it needs to launch tools was added and it no longer has a fallback that builds the command line itself — refusing older orchestrators is what makes that removed fallback **unreachable** rather than merely deprecated. The orca plugin is the same kind and stayed at `>=0.1.0`, because it drives the `orca` CLI and never reads that field; raising its lower bound would reject orchestrators it works with perfectly well. (The orca plugin has since been rebuilt to launch that same field, so it now depends on it for the same reason herdr does; its lower bound is 0.6.0, which already includes 0.2.3.)

## Methods

**O→P** is an orchestrator-to-plugin call; **P→O** is plugin-to-orchestrator.

### Common to every kind

| Method | Direction | What it does |
|---|---|---|
| `initialize` | O→P | Passes resolved config and the protocol version; you return your version and capabilities |
| `config/validate` | O→P | Validates your plugin's configuration. The same workflows, projects and repositories from `initialize` come with it, so you validate what you are being asked about rather than what you remembered. **`warnings` is the channel for "the config is fine, but you should know this"**: it does not affect `valid`, `totsuka doctor` renders it as an advisory check, and it appears in `--json`. Write it in the same "cause → next action" shape as `errors` — a warning nobody can act on is noise, and noise is how a diagnostic stops being read. It is optional, so a plugin that sends none produces exactly the `doctor` output it always did |
| `shutdown` | O→P | Asks you to exit, with a grace period |

`initialize` also hands a `task_source` several things it would otherwise have to configure twice. All are optional — ignore what you do not use.

- `repositories: [{name, summary?, path?}]` — the orchestrator's configured repositories, so a source that resolves repositories itself does not need its own copy
- `llm: {api?, base_url, endpoint?, model, api_key?}` — the orchestrator's classifier settings with the key already resolved. If your plugin has its own LLM configuration, prefer that and treat this as the default. `api` absent or `"chat"` means an OpenAI-compatible API at `base_url`; `"decisions"` means a Decisions API at `endpoint`, and then **`base_url` is an empty string** — deliberately, so plugins that do not know `api` read it as "nothing supplied". Do not use an `llm` with an empty `base_url` as a chat API, or one with an `api` value you do not know
- `workflows: [{workflow, trigger, instructions_kind?, task_id_prefix?, options}]` — every workflow that names you, as its `source` or its `agent`, in the order they appear in the configuration. `trigger` is what a source watches for, passed through exactly as the operator wrote it (an agent gets an empty object); `instructions_kind` and `task_id_prefix` are derived by the orchestrator from the workflow's `profile`; `options` holds the keys on that workflow the orchestrator does not understand. **Reject `trigger` keys you do not read.** Pass-through means nobody else checks them, so a key you ignore is silently dropped and the condition goes away — a typo does not narrow the trigger, it *widens* it. `plugin_sdk::unknown_trigger_keys(&init.workflows, TRIGGER_KEYS)` returns one message per unknown key, each listing the keys you do read; fail `initialize` with `CONFIG_INVALID` if it is not empty. **If your source has assignees, use `plugin_sdk::AssigneeFilter` for the `assignee` condition** — it owns the vocabulary (`@me` / `@none` / `@any` / a name / a list) and the matching, and you supply only what those are matched against: who "me" is, and where the assignees come from. `check_assignee_triggers` in the same module is the `initialize` half: it refuses conditions that cannot be evaluated (an `@me` with no identity configured) and warns about an `assignee` trigger with no `status` beside it, which can only ever run once per task — you declare whether that warning applies, because a source that keys no delivery on its status column would not be helped by adding one
- `projects: [{name, options}]` — the projects you own, from `[[projects]]` entries whose `source` is you. The repositories bound to each come from `[[repositories]].project`

How often a polling source fetches is its own business: put `poll_interval_secs` in your plugin's `[<name>]` table and read it from `config`.

### task_source

**A task source is push-only.** When you find a task you send `task/submit` to the orchestrator yourself; there is no RPC where the orchestrator comes to fetch tasks. Event-driven sources (webhooks, sockets) submit on each event; sources that are naturally polled run their own timer from the `workflows` you got in `initialize` and the `poll_interval_secs` in their own `[<name>]` table. The `plugin-sdk` crate provides that timer as `poll_loop`.

| Method | Direction | What it does |
|---|---|---|
| `task/submit` | **P→O request** | Pushes a task you found, **naming the workflow it belongs to**. You ran first-match over the workflows you were given, so you already know; the orchestrator only checks that the name exists and is yours. The orchestrator persists before acknowledging |
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

`state` is one of `idle`, `running`, `waiting_input`, `done`, `failed`. The orchestrator maps these onto its own state machine — `running` starts the clock, `waiting_input` parks the task (it keeps its concurrency slot), `done` moves to publishing — so map your tool's real state onto these five honestly.

### notifier

| Method | Direction | What it does |
|---|---|---|
| `notify` | O→P (no response) | Delivers an event: `waiting_input`, `done`, `failed`, or `pending` |

**A failed delivery must never affect task execution.**

## Logging and stderr

Write your logs to **stderr** — stdout is reserved for JSON-RPC. The orchestrator reads
them line by line and re-emits each one in its own log **with your level, target and
fields intact**, tagged with your plugin name.

- **With the SDK there is nothing to do.** Call `plugin_sdk::runtime::init_tracing()`
  first thing in `main`. When stderr is a pipe (running under the orchestrator) it
  writes JSON Lines at every level; when stderr is a terminal (running it by hand) it
  prints human-readable lines and honours `RUST_LOG` (default `info`).
- **Levels are filtered only by the orchestrator's `[log] level`.** Do not filter in
  the plugin. To see your plugin's debug output, set `[log] level = "debug"` (or pass
  `--debug`).
- **Without the SDK**, write one JSON object per line with `level` (`ERROR` to
  `TRACE`), `target`, `message` and any fields, and it is handled the same way. Any
  other line is forwarded verbatim as `INFO`; a `thread '…' panicked at` line and
  the non-JSON lines following it become `ERROR` (SDK lines keep their own level
  even after a panic).

Each field passes through the orchestrator's redaction on its own, so a field named
like `api_token` is masked as `***`. **A secret embedded in the message is only masked
if it matches a known token shape** (`Bearer …`, `ghp_…`, and so on) — the orchestrator
does not know what your plugin considers secret, so keeping secrets out is your job.

Forwarding is rate-limited to **100 lines per 10 seconds**; anything beyond that is
collapsed into a single "suppressed N lines" warning. A plugin stuck in a failure
loop can emit stderr faster than anything reads it, and the cap keeps it from burying
everything else. The suppressed count is still reported, so the noise stays visible
as a number. Only lines that pass `[log] level` count, so debug output that is
discarded anyway never uses up the budget.

Calls the orchestrator makes to your plugin are timed and counted per method on its
side. `totsuka run --json` reports them under `plugins`, with call counts, a
breakdown by outcome, and recent p50/p95 latency. Your plugin does not have to do
anything for this.

## Writing it with the SDK handlers

You do not have to write the JSON-RPC line handling yourself (parse errors, silence on notifications, `shutdown`, unknown methods, type-checking params). Implement one of the `plugin-sdk` typed handlers and the SDK takes care of the rest.

| Kind | Trait to implement | How to put it on stdio |
|---|---|---|
| `task_source` | `TaskSourceHandler` (initialize / config_validate / update_status / result_publish, optionally task_claim) | `serve(TaskSourceServer(handler), &stdio)`. If your server is itself the handler, implement `LineHandler` with `plugin_sdk::dispatch::handle_line(self, line)` |
| `agent_ide` | `AgentIdeHandler` (initialize / config_validate / task_dispatch / session_attach / task_cancel / state_subscribe, optionally session_focus / session_release / session_list / diagnostics_snapshot) | `serve(AgentIdeServer::new(handler, stdio.writer.clone()), &stdio)` |

- Each method receives its params type and returns its result type or a `plugin_protocol::jsonrpc::Error`. Return `plugin_sdk::not_initialized()` for calls that arrive before `initialize`. Override `initialized()` as well, so that a request with malformed params arriving before `initialize` gets the same "initialize first" answer instead of `INVALID_PARAMS`.
- **Methods gated on a capability answer `METHOD_NOT_FOUND` by default** (`task_claim`, `session_focus` / `session_release` / `session_list`, `diagnostics_snapshot`). If you override one, declare its capability; if you declare the capability, override the method.
- **`state_subscribe` only has to return a receiver of state changes.** `AgentIdeServer` guarantees the order: the reply first, then the `state/notification`s.
- For `{placeholder}` substitution in configurable prompts and instructions, use `plugin_sdk::template::render`. It substitutes in a single pass, so `{...}` written inside external content is never expanded. An agent_ide can build the prompt it hands the agent with `plugin_sdk::compose_prompt`.

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

## Where your configuration lives

Your plugin's own settings are a top-level `[<name>]` table in `config.toml`, where `<name>` is the roster name from `[plugins.<name>]` — the same as your binary name. The orchestrator holds it uninterpreted, resolves any secret references, and hands it to you as `initialize`'s `config`. A top-level table whose name is not in the roster is a configuration error, so a typo is reported rather than silently ignored.

You can also define keys on the orchestrator's own structures:

| Where | How ownership is decided | What you implement |
|---|---|---|
| `[[workflows]]` | **Asked.** Leftover keys go to the workflow's task source (the owner of its `projects`) and its `agent`, and exactly one must claim each | Return `{workflow, key}` pairs in `claimed_options`. **Never claim a key you ignore** — that turns a typo into silence |
| `[[projects]]` | **Settled by `source`.** An entry names exactly one plugin | Deserialize into a `deny_unknown_fields` struct. No handshake needed |

A workflow key nobody claims fails startup, and so does one two plugins claim.

## Conformance tests

The `plugin-conformance` crate checks whether your plugin follows the protocol's rules. It starts your plugin's **binary** and talks to it over stdio, so it works the same whether or not you use `plugin-sdk`. All seven official plugins run it from their `tests/conformance.rs`.

```toml
[dev-dependencies]
plugin-conformance = { git = "https://github.com/tomoya-k31/totsuka" }
serde_json = "1"  # the example below builds its params with it
```

```rust
#[test]
fn the_binary_conforms_to_the_protocol() {
    // The smallest initialize params your plugin accepts; a task_source also
    // needs one workflow with a valid trigger.
    let init = serde_json::from_value(serde_json::json!({
        "protocol_version": plugin_protocol::PROTOCOL_VERSION,
        "config": { "token": "test" },
        "workflows": [{ "workflow": "w", "trigger": { "status": "todo" } }]
    }))
    .unwrap();
    let violations = plugin_conformance::check(
        env!("CARGO_BIN_EXE_mytool"),                       // your [[bin]] name
        concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"), // source of kind and capabilities
        &init,
    );
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
```

The kit never sends `init` as it is: a successful `initialize` would reach real services, so it only sends broken copies. Test what your plugin does after a successful `initialize` in your own tests.

It checks the nine rules below and reports every violation at once. **If you write your plugin in another language, this is the list of rules to follow.**

| # | Applies to | Rule |
|---|---|---|
| 1 | every kind | Before `initialize`, refuse each kind-specific request the host may send you (filtered by your capabilities) with `INVALID_REQUEST` (-32600). A notifier answers nothing to an early `notify` |
| 2 | every kind | Answer a line that is not JSON with `PARSE_ERROR` (-32700) and `id: null` |
| 3 | every kind | Answer an unknown method with `METHOD_NOT_FOUND` (-32601) |
| 4 | every kind | Answer nothing to a blank line or a notification (no `id`) |
| 5 | every kind | Answer malformed `initialize` params with `INVALID_PARAMS` (-32602) |
| 6 | every kind | `config/validate` on a config with an unknown key returns `valid: false`, with an error naming the key |
| 7 | every kind | Answer `shutdown` with a result, then exit with status 0 |
| 8 | every kind | Exit when stdin reaches EOF |
| 9 | task_source | `initialize` with an unknown key in a trigger fails with `CONFIG_INVALID` (-32003), and the message names the key |

Only error **codes** are checked, never the message wording. The exception is the key name in rules 6 and 9: it is the one thing an operator needs from the error to fix their config.

## Checking it works

`totsuka config validate` delegates to your `config/validate` — unless you pass `--offline`, which keeps it to static checks and never launches a plugin. `totsuka doctor` probes your plugin live. Either will tell you whether your plugin starts and answers.

---

This page is generated from the internal document `ai-docs/development/plugin-dev-guide.md`, which carries the design decisions and measurements behind it.
