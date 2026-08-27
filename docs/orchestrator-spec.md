> 🌐 **English** · [日本語](orchestrator-spec.ja.md)

<!-- generated-from: ai-docs/product/orchestrator-spec.md sha256:d1ed93a99f3427fbe38fb856bab73d9334ee035238bf39895ab7f9dffe7ea1b1 -->

# What totsuka is

totsuka is a command-line orchestrator that connects your task tracker to AI coding agents running against your local repositories.

It reads tasks from sources like GitHub Issues and Projects or Notion, decides which repository each one belongs to, creates a git worktree for it, and hands the work to an agent such as herdr or orca. Detailed design and implementation are the agent's job; totsuka handles everything around them.

It is a local, single-machine tool. There is no server, no event bus, and no resident daemon — you run it from a terminal and it exits when you stop it.

- Platform: macOS 14 or newer, git 2.40 or newer
- Written in Rust, run as a single binary from the terminal

## The model

**One task, one repository, one worktree.** That normalization is what makes conflict-free parallel work on the same repository possible. Several tasks can run against one repository at once because each has its own worktree.

**A task's output is not necessarily a pull request.** What "done" means is decided by the workflow you define — replying in the source, writing a design into an issue comment, opening a pull request, or nothing at all.

**Agents own the git side.** The worktree is handed over on a detached `HEAD`. The agent names and creates the branch following the repository's own conventions, commits, pushes, and opens the pull request. totsuka learns the branch by reading it back afterwards, and never generates one — a generated name cannot follow a convention written inside the repository.

## What it does

### Getting tasks

Task sources connect as plugins and hand totsuka a normalized task: id, source, title, body, repository hint, labels, priority, status, URL, and assignee. Bundled sources cover GitHub (Issues and Projects, including Project status columns) and Notion (with the database property mapping defined in your configuration).

Sources push tasks as they find them. Field mapping and filter conditions are configurable per plugin, and statuses can be written back to the source as the task progresses.

**Deciding whether to pick a task up at all is the source plugin's job**, not totsuka's — checking assignees or in-progress status so that work another person has started is left alone. There is no strict mutual exclusion between people, but a source that supports claiming is asked to claim the task right before it runs: if a teammate's instance got there first, your copy of the task steps aside as `skipped` instead of running the same work twice (`totsuka task retry` re-enters it deliberately). Sources without claim support behave as before. The plugin also decides *which* of your workflows a task belongs to, and says so when it hands the task over.

**Where a new item gets filed is your configuration.** A `[[projects]]` entry names a tracker — a GitHub Project, a Notion database — and each repository points at one of them with `project = "…"`. One repository files into one tracker, so a request that arrives through Slack and turns into an issue has exactly one place to go.

Some sources write the result back for you. Where the agent can write the deliverable itself — a `gh` comment, a Notion page — it does, and the source stays out of it; the write-back path exists for sources where totsuka has to mediate, like a Slack reply that goes out under your own name — after your approval by default, or immediately for a workflow you configure with `publish = "direct"`.

### Choosing the repository

If the task says which repository it belongs to, that wins. Otherwise totsuka classifies it with an LLM, using the summaries you configured plus the first few lines of each repository's README.

LLM calls go through an OpenAI-compatible API, so pointing `base_url` at a gateway such as OpenRouter or LiteLLM is all it takes to switch providers. A cheap, fast model is assumed. The model reports a confidence alongside its choice; when candidates are close, totsuka asks you rather than guessing.

### Managing worktrees

A worktree is created when a task starts. Immediately before, totsuka fetches and checks out the remote default branch detached, so work never starts from a stale local branch. The base branch is overridable per repository, and the starting commit is recorded so cleanup can tell your branches from the task's.

Where worktrees live is configurable, and the directory name is derived from the task rather than the branch — the branch does not exist yet at that point.

Cleanup on completion or cancellation follows a policy you choose: immediately, after a retention period, or manually. Worktrees left behind with no matching task are detected by `doctor`, which offers to clean them up.

### Driving agents

Agent IDEs are plugins too, and which one runs can be switched per task type and per repository. Dispatching hands over the worktree path, the task body, the mode (`plan` or `implement`), and any extra context, and gets back a session.

Agents report their state as one of idle, running, waiting for input, done, or failed. Completion itself is detected through a hook the agent CLI fires, which makes it deterministic rather than inferred from output.

In workflows where a human approves completion at the pane, questions and the completion confirmation arrive through the tool's native question picker — claude's `AskUserQuestion`, opencode's `question` dialog. While the dialog is open totsuka treats the task as waiting for input, releases its slot, and notifies you with the question text. Tools without a picker ask with a numbered list instead.

**Plugins declare what they support and totsuka only asks for that.** A plugin that does not implement plan mode is never asked to run one.

### Running things in parallel

Concurrency is limited globally, per repository, and per agent plugin. A task waiting for input releases its slot, so a conversation that is waiting on you does not hold up the queue.

### Notifications

Notifier plugins deliver events — waiting for input, done, failed, pending. A macOS notification plugin is bundled. **A failed notification never affects task execution.**

### Showing status in the menu bar

`totsuka menu` renders a view for a menu-bar host such as SwiftBar. It has two channels: the **glyph** is availability (`○` running and healthy, `⚠` running but degraded, `✕` not running), and the **number** is how many tasks are waiting on you.

That number counts five states — `pending`, `waiting_input`, `verifying`, `escalated`, and `queued` with a recorded reason — and nothing else. Finished tasks are never counted, so the number returns to zero once you have dealt with everything. Clicking a task row brings its pane to the front; nothing in the menu changes a task's state.

The glyph has a third state, `⚠`: totsuka is running but cannot do its whole job. It covers four things it can re-check every cycle — the hook receiver failed to bind (nothing can report completion for that run), a plugin is down, hook signals are stuck in the spool, or the LLM gateway rejected the API key. Each clears on its own once fixed. There is a fifth case totsuka cannot report about itself: if it stops publishing altogether for two minutes while its process is still alive, that shows as `⚠` too — a wedged run cannot tell you it is wedged. `totsuka status` shows the same reasons under `degraded:`.

The default output is SwiftBar's plugin format; `--json` gives you the same view as data. It always exits 0 — a plugin that does not renders as a broken item — so failures appear as a row instead. Task titles come from whoever filed the task, so totsuka escapes them before they reach SwiftBar: neither a `|` nor a newline in a title can add parameters to a row or split it in two. Setup is two lines of shell, in the operations guide.

## The command line

A single binary, run in the foreground.

| Command | Purpose |
|---|---|
| `init` | Generate configuration scaffolding and check the environment |
| `setup` | Interactive first-time setup, from a recipe |
| `run [--watch] [--json]` | The main loop, from intake to dispatch. `--watch` stays up until you stop it |
| `status [--json]` | Running, queued, and waiting tasks, plus worktrees, and anything the running orchestrator cannot currently do |
| `menu [--json]` | The menu-bar view: availability, plus how many tasks are waiting on you |
| `task list / show <id> / cancel <id> / retry <id>` | Working with individual tasks |
| `task export` | Stream the audit log to stdout as NDJSON |
| `plugin list / install / uninstall / enable / disable` | Plugin management |
| `config validate / show [--redacted]` | Validate or print configuration, with secrets masked |
| `doctor` | Diagnose the environment |
| `logs [-f] [--task <id>]` | Read or follow logs |
| `completion <shell>` | Shell completions |

Common flags are `--debug`, `--json`, `--dry-run`, and `--config <path>`. `--json` is available on the commands that print a document — `status`, `menu`, `task list`, `task show`, `plugin list`, `doctor` — so other tools can consume them. `task export` needs no such flag, since NDJSON is the only thing it prints. `menu` is the one command that always exits 0: it is read by a menu-bar host, and a plugin that exits non-zero renders as a broken item, so failures become a row in the menu instead.

Whenever you ask for machine-readable output, stdout carries the document and nothing else; anything advisory goes to stderr.

`run --json` prints the run summary as a single JSON document on stdout and nothing else, so you can act on a run instead of reading it:

```bash
totsuka run --json | jq -e '.stats.failed == 0'
```

The document has `stats` (`submitted` / `dispatched` / `done` / `failed` / `skipped`), the task ids left in `waiting`, `pending`, and `queued`, and `interrupted`. **The exit code does not follow it** — a run that correctly recorded a failing task still exits 0, so decide from the document. `--json` cannot be combined with `--dry-run`, which has nothing to preview.

`task export` writes the audit log — every state change every task has been through — to stdout as NDJSON, one event per line, oldest first:

```bash
totsuka task export | jq -r 'select(.to == "failed") | .task.source_task_id'
totsuka task export --since 4213 > today.ndjson   # only what is new
```

The log is append-only, so `--since <event_id>` is all you need to resume from a previous export; a cursor past the end succeeds with no output. Add `--task <id>` for one task — an id that does not exist is an error, not an empty archive. `--no-detail` leaves out the `detail` field, which carries the agent's full output on publish transitions and can be large; rows then omit the key entirely, so an archive taken that way stays distinguishable from one where a transition simply recorded nothing (`"detail": null`). Piping into `head` is fine: the command stops quietly when the reader goes away.

Read-only commands like `status` start in under a second.

## What it guarantees

**Errors always say what to do next**, not just what went wrong — `config not found → run 'totsuka init'`.

**Secrets are masked unconditionally** in the logging layer: API keys, tokens, and authorization headers. Prompt bodies are written only at debug level or above, and can be turned off entirely. Logs are structured JSON Lines, rotated daily with a configurable retention count, and the `logs` command formats them for reading.

**Configuration, state, and logs follow the XDG Base Directory specification**, so you can relocate them with the usual environment variables.

**Output respects `NO_COLOR` and non-interactive terminals.**

**Recovery is explicit.** After an abnormal exit, totsuka restores sessions from its state database and tries to reattach. Tasks it cannot reattach are not failed automatically — they wait for you to retry or cancel them.

## What it deliberately does not do

| Not included | Why |
|---|---|
| A web dashboard or cloud UI | State stays local. Text for a local menu bar is supported; the drawing is the host's job |
| Pull request review, merge decisions, merge tracking | Human review territory. Nothing is tracked after the pull request is opened |
| Guaranteed Linux and Windows support | The abstractions are there; the implementation and testing are not |
| A resident daemon or server | The lifecycle is bounded by the process you launched |
| Cloud sync or shared state across a team | State is local. Sharing is delegated to GitHub and Notion |
| The agents themselves | Code generation is entirely the agent's |
| Cloning repositories or managing git credentials | Repositories are assumed already cloned, and git uses your existing authentication |

## What it assumes

- Your repositories are already cloned locally and registered by path in the configuration
- Your agent IDE — herdr, orca, or another — is installed separately

---

This page is generated from the internal document `ai-docs/product/orchestrator-spec.md`, which carries the full requirements with identifiers, priorities, and open questions.
