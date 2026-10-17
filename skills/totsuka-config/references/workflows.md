# Workflows

A workflow says which tasks to pick up (`trigger`), what the agent may do (`profile`), and what to write back to the source (`on_*`). This file holds the rules shared by every source; the questions, recipes, trigger keys and ID lookups for each source are in its own file ([github.md](github.md), [notion.md](notion.md), [slack.md](slack.md), [discord.md](discord.md)). Read both before writing a workflow.

Build one entry in this order, asking only what the user has not already said, then write it with one `set /workflows/- '{...}'`:

1. `name`, `projects` (one or more project names, all from one source) and `agent` (`herdr` / `orca`).
2. `profile`: [Profiles](#profiles).
3. `trigger`: the source's file.
4. `on_start` / `on_success` / `on_failure`: [Write-backs](#write-backs-on_) and the source's file.
5. `timeout_secs` and `initial_prompt`: [Running unattended](#running-unattended), [`initial_prompt`](#initial_prompt).
6. `rubric`, `tool`, `cleanup`, and the explicit `mode` form only when the user asks: [Rarely needed keys](#rarely-needed-keys).

Workflows are matched in file order and the first match wins, so a narrower trigger goes before a broader one on the same board. A misspelt trigger key does not narrow the trigger, it widens it (the condition is dropped), and only the app's Start catches it; take key names from the schema.

## Profiles

A profile fixes what an agent may do and what counts as done. Ask the user what the run should produce, then map it:

| profile | produces | changes code | done when | replies to the source |
|---|---|---|---|---|
| `answer` | a reply to a question | no; shell is denied too | an LLM judges the reply | yes |
| `triage` | a filed issue or Notion page | no | an LLM finds the filed item's URL in the last message | yes |
| `design` | a detailed design posted as an issue comment or page | no | the user approves in the agent's pane | no |
| `implement` | code and a pull request | yes, the only one | the user approves in the agent's pane | no; add `"output": "source"` to reply |

`design` and `implement` need someone watching the pane; a run that should finish unattended uses `answer` or `triage`. The read-only profiles are not a sandbox: if the target repository's own rules tell agents to push and open PRs, the agent may still do so, and totsuka then fails the task. Ask whether the repository has such rules when choosing `answer` / `triage` / `design`.

What each source supports:

| source | answer | triage | design | implement |
|---|---|---|---|---|
| github | no | yes | yes | yes |
| notion | no | yes | yes | yes |
| slack | yes | yes | no | yes |
| discord | yes | yes | yes | yes |

- **GitHub and Notion cannot post a reply**, so `answer` has nowhere to go. `validate --offline` rejects it with "does not declare the `source` output capability".
- **Slack has no design instructions.** A Slack `design` workflow posts nothing, and validation still passes. Steer the user to `triage`.
- **Slack and Discord get a result back only with output `source`**: the default for `answer` and `triage`; add `"output": "source"` for `design` / `implement`. `output` is the one key that may sit next to a profile.

## Write-backs (`on_*`)

`on_start`, `on_success` and `on_failure` move a GitHub card or a Notion page (`{"status": "<column>"}`; GitHub also takes `"labels"`). Slack and Discord have nothing to move.

- **`on_start` comes with `on_failure`.** Otherwise a failed task leaves its card in the "running" column and the board stops telling the truth. When the user wants `on_start`, ask where failures should go in the same question. Failures that totsuka retries by itself (a failed publish) do not write back.
- **No loops.** A write-back (`on_failure` included) into a column some workflow on the same board triggers on starts that workflow with no human in between; around a cycle it runs forever, at real cost. `validate --offline` rejects a cycle and names the path. Route through a column no workflow triggers on, such as a review or `Blocked` column a person moves the card out of.

### Re-runs

| source | runs a task again when |
|---|---|
| github | the card re-enters a `status` trigger column (including after it finished); a different workflow's column continues the same conversation |
| notion | never: each page runs once, across all workflows |
| slack, discord | never: each message is one task; a new mention or reaction makes a new one |

## Running unattended

`timeout_secs` is how long the agent may stay silent before totsuka escalates. The default `0` turns the check off, so a stuck agent stays stuck unnoticed. For `answer` and `triage` (nobody watches the pane) suggest a value such as `1800`. For `design` and `implement` leave it unset: those runs pause for the user's approval and long silent stretches are normal.

## `initial_prompt`

Text put in front of the task, visibly in the pane, to steer how the agent works: `"Use the /grill-me skill to work out the design with me"`, or a repository convention to follow.

- It is sent only when a conversation starts. A thread reply, a retry that continues the conversation, and a GitHub pipeline handoff do not repeat it, so the second stage of a pipeline never sees its own `initial_prompt`. Put what the whole pipeline needs into the first stage's.
- It is literal text: `{` and `}` need no escaping.
- In a workflow nobody watches (`answer`, `triage`), avoid instructions that make the agent ask the user something: the run then waits forever, or until `timeout_secs`.
- Leave the output language out unless the user insists: by default the agent writes in the language of the thread, issue or page.

## Rarely needed keys

Offer these only when the user asks for what they do:

- `rubric`: the condition the LLM check uses to accept "done" (only with `answer` / `triage`, which verify with an LLM). Write it as a condition that is true for every acceptable result ("the reply links the relevant file"), not as an instruction to the checker.
- `tool`: pin the AI tool for this workflow (`claude`, `codex`, `opencode`, or a `[tools.<name>]` entry). The LLM check needs Claude; with another tool it falls back to human verification.
- `cleanup`: when the task's worktree and pane are removed. `"immediate"`, `"manual"`, `"keep_7d"`, `"keep_28d"` or `{"retention_days": n}`. Without it, `[worktree]` decides (implement work is kept, read-only work is removed at once).
- The explicit form: without a profile, `mode` (`plan` or `implement`) and `output` (`source` or `none`) are both required, and `verification` (`llm`, `human`, `none`) is optional. Use it only for what no profile gives, such as `"verification": "human"` (the user approves with `totsuka task verify`). `mode` and `verification` cannot sit next to `profile`. A plain `mode: "plan"` only warns when the agent makes a branch; a read-only profile fails the task, so prefer a profile.
