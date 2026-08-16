> 🌐 **English** · [日本語](orchestrator-spec.ja.md)

<!-- generated-from: ai-docs/product/orchestrator-spec.md sha256:fbed6eebe061d9521689ed66df272e1b6b643cc2456e1efacb225f075a348db6 -->

# What totsuka is

totsuka is a command-line orchestrator that connects your task tracker to AI coding agents running against your local repositories.

It reads tasks from sources like GitHub Issues and Projects or Notion, decides which repository each one belongs to, creates a git worktree for it, and hands the work to an agent such as herdr or orca. Detailed design and implementation are the agent's job; totsuka handles everything around them.

It is a local, single-machine tool. There is no server, no event bus, and no resident daemon — you run it from a terminal and it exits when you stop it.

- Platform: macOS 14 or newer, git 2.40 or newer
- Written in Rust, distributed as a single binary

## The model

**One task, one repository, one worktree.** That normalization is what makes conflict-free parallel work on the same repository possible. Several tasks can run against one repository at once because each has its own worktree.

**A task's output is not necessarily a pull request.** What "done" means is decided by the workflow you define — replying in the source, writing a design into an issue comment, opening a pull request, or nothing at all.

**Agents own the git side.** The worktree is handed over on a detached `HEAD`. The agent names and creates the branch following the repository's own conventions, commits, pushes, and opens the pull request. totsuka learns the branch by reading it back afterwards, and never generates one — a generated name cannot follow a convention written inside the repository.

## What it does

### Getting tasks

Task sources connect as plugins and hand totsuka a normalized task: id, source, title, body, repository hint, labels, priority, status, URL, and assignee. Bundled sources cover GitHub (Issues and Projects, including Project status columns) and Notion (with the database property mapping defined in your configuration).

Sources push tasks as they find them. Field mapping and filter conditions are configurable per plugin, and statuses can be written back to the source as the task progresses.

**Deciding whether to pick a task up at all is the source plugin's job**, not totsuka's — checking assignees or in-progress status so that work another person has started is left alone. There is no strict mutual exclusion between people.

Results can be written back to the source too: a design document into an issue comment, a Notion page body, and so on. How it is formatted and where it goes is the source plugin's decision.

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

**Plugins declare what they support and totsuka only asks for that.** A plugin that does not implement plan mode is never asked to run one.

### Running things in parallel

Concurrency is limited globally, per repository, and per agent plugin. A task waiting for input releases its slot, so a conversation that is waiting on you does not hold up the queue.

### Notifications

Notifier plugins deliver events — waiting for input, done, failed, pending. A macOS notification plugin is bundled. **A failed notification never affects task execution.**

## The command line

A single binary, run in the foreground.

| Command | Purpose |
|---|---|
| `init` | Generate configuration scaffolding and check the environment |
| `setup` | Interactive first-time setup, from a recipe |
| `run [--watch]` | The main loop, from intake to dispatch. `--watch` stays up until you stop it |
| `status [--json]` | Running, queued, and waiting tasks, plus worktrees |
| `task list / show <id> / cancel <id> / retry <id>` | Working with individual tasks |
| `plugin list / install / uninstall / enable / disable` | Plugin management |
| `config validate / show [--redacted]` | Validate or print configuration, with secrets masked |
| `doctor` | Diagnose the environment |
| `logs [-f] [--task <id>]` | Read or follow logs |
| `completion <shell>` | Shell completions |

Common flags are `--debug`, `--json`, `--dry-run`, and `--config <path>`. Every read-only command supports `--json` so other tools can consume it.

Read-only commands like `status` start in under a second.

## What it guarantees

**Errors always say what to do next**, not just what went wrong — `config not found → run 'totsuka init'`.

**Secrets are masked unconditionally** in the logging layer: API keys, tokens, and authorization headers. Prompt bodies are written only at debug level or above, and can be turned off entirely. Logs are structured JSON Lines, rotated daily with a configurable retention count, and the `logs` command formats them for reading.

**Configuration, state, and logs follow the XDG Base Directory specification.**

**Output respects `NO_COLOR` and non-interactive terminals.**

**Recovery is explicit.** After an abnormal exit, totsuka restores sessions from its state database and tries to reattach. Tasks it cannot reattach are not failed automatically — they wait for you to retry or cancel them.

## What it deliberately does not do

| Not included | Why |
|---|---|
| A GUI or web dashboard | It is a terminal tool |
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
