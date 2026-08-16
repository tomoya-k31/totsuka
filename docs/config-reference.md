> 🌐 **English** · [日本語](config-reference.ja.md)

<!-- generated-from: ai-docs/development/config-reference.md sha256:70214eb4c7cc4648bc84830cf3d52292f1547f74364392ed99d84813748c4bd2 -->

# Configuration reference

Every key in `config.toml` and in the per-plugin `plugins/{name}.toml`, with its type, default, and meaning.

## Where the files live

- Shared configuration: `$XDG_CONFIG_HOME/totsuka/config.toml` (by default `~/.config/totsuka/config.toml`)
- Per-plugin configuration: `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml`. totsuka keeps this uninterpreted and passes it to the plugin once secrets are resolved
- `--config <path>` overrides the location of `config.toml`

`totsuka init` writes a template. `totsuka config validate` checks it; `totsuka config show [--redacted]` prints it.

## Secret references

Never write a plain secret into your configuration. Any string value can instead be one of:

| Form | Resolves from |
|---|---|
| `keychain:<service>/<account>` | The macOS Keychain |
| `op://<vault>/<item>/<field>` | 1Password |
| `cmd:<command>` | The standard output of a command |
| A string containing `${ENV_VAR}` | Environment variables |

`~` and `${ENV}` are also expanded in paths.

**`op://`** shells out to the 1Password CLI and assumes you have already run `op signin`. It works on **any string value** in either config file, and because the CLI is cross-platform this is the only backend that works outside macOS. A missing CLI, a missing item, and a missing sign-in each produce a specific, actionable error. `totsuka doctor` only probes 1Password when your configuration actually contains an `op://` reference.

**`cmd:`** runs the command through `/bin/sh -c` and uses its standard output as the secret, with the trailing newline stripped. It is meant for credentials another tool already manages and rotates — `token = "cmd:gh auth token"` — because it fetches the current value every time rather than keeping a copy that can silently go stale. A non-zero exit or empty output is a startup error, quoting the first line of stderr; standard output is never quoted anywhere. The command runs only when `totsuka run` resolves secrets, never during parsing or `config show`.

**Do not put a secret inside the command string.** Reference strings are part of your configuration and can be quoted in error messages. The rule against plaintext secrets applies here too — the point of this form is to make the command *fetch* the secret.

## Top-level keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `version` | int | 1 | Configuration schema version. A mismatch fails validation at startup |
| `max_concurrency` | int? | 4 | Global limit on tasks running at once |
| `[[repositories]]` | array | — | Repositories to work in |
| `[plugins.{name}]` | table | — | Which plugins exist and their shared settings |
| `[[workflows]]` | array | — | Workflow definitions |
| `[llm]` | table | none | AI gateway settings. Without it, repository selection that needs an LLM falls back to `pending` |
| `[worktree]` | table | — | Worktree placement and cleanup |
| `[log]` | table | — | Logging |
| `[hooks]` | table | — | Receiving agent CLI hook events |
| `default_tool` | string? | `"claude"` | Default AI tool when neither the workflow nor the repository pins one |
| `[tools.{name}]` | table | — | AI tool registry; overrides and extends the built-ins |
| `[prompts]` | table | — | Overrides for the prompt text injected into AI tools |

## Schema versioning

The current schema is **v1**, and it has never been bumped.

A `config.toml` whose `version` does not match is rejected at startup validation, and **totsuka never rewrites your configuration**. `config validate`, `run`, and `doctor` share the same validation, so all three notice the same mismatch, but they treat it differently: `config validate` and `run` stop with an error, while `doctor` reports it as a failing `config` check and carries on with the other checks.

The guidance depends on which side is behind:

- `version` is newer than totsuka expects → **totsuka is old.** Update to a version that understands that schema
- `version` is older → **your configuration is old.** Bring `config.toml` up to the current shape and change `version`

**There is no `totsuka config migrate`.**

## `[[repositories]]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Stable id used in branch names and logs |
| `path` | string | required | Path to the local clone (`~` and `${ENV}` expand) |
| `summary` | string? | none | Description used when an LLM picks the repository |
| `tool` | string? | `default_tool` | Default AI tool for tasks dispatched to this repository. A workflow's `tool` wins |
| `max_concurrency` | int? | unlimited | Per-repository limit on tasks running at once |
| `worktree_location` | string? | `[worktree].location` | Overrides the worktree placement template for this repository |

## `[plugins.{name}]`

`{name}` is the instance name a workflow refers to with `source` or `agent`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enabled` | bool | false | Whether it is active. Also toggled by `totsuka plugin enable/disable` |
| `kind` | enum | required | `task_source`, `agent_ide`, or `notifier` |
| `max_concurrency` | int? | unlimited | Per-agent-plugin limit on tasks running at once |
| `timeout_secs` | int? | 120 | Timeout for a single call to the plugin |
| `log_level` | string? | none | The plugin's log level |
| `poll_interval_secs` | int? | 60 | Task sources only. Push-style sources are never polled by totsuka; this value is forwarded to the plugin and becomes its own internal fetch interval |

## `[[workflows]]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Workflow name |
| `source` | string | required | Task source instance name |
| `trigger` | table | `{}` (matches everything) | Match conditions. See below |
| `profile` | enum? | none | One of `answer`, `triage`, `design`, `implement`. Decides `mode`, `output`, and `verification` together |
| `mode` | enum | required without `profile` | `plan` or `implement` |
| `agent` | string | required | Agent instance name |
| `output` | enum | required without `profile` | `source` or `none` |
| `on_success` | `{ set_status = "..." }`? | none | Update the status in the source on success |
| `on_failure` | `{ set_status = "..." }`? | none | Update the status in the source on failure. Retryable failures do not write back |
| `verification` | enum | `llm` | How a completion claim is checked: `llm` (checked in session), `human` (waits for `totsuka task verify`), or `none`. Cannot be combined with `profile` |
| `timeout_secs` | int? | 1800 | Seconds of silence after the last signal before escalating. **`0` opts this workflow out of the timeout sweep entirely** |
| `rubric` | string? | none | The criteria used for `llm` verification |
| `[workflows.prompts]` | table | — | Prompt overrides for this workflow only; the strongest layer |
| `tool` | string? | none | Pins the AI tool. Workflow beats repository beats `default_tool` |
| `initial_prompt` | string? | none | Extra instructions prepended for this workflow's agent. See below |

Workflows are matched in definition order, first match wins. Overlapping triggers within one source produce a warning. **A workflow defined after a catch-all (`trigger = {}`) for the same source is unreachable**, and you get a warning.

Setting `timeout_secs = 0` is for attended workflows where a human is watching the pane. A genuinely hung agent stops being detected too, so do not set it on unattended workflows.

If `verification = "llm"` may resolve to a non-Claude tool, you get a warning suggesting `tool = "claude"` — in-session verification needs Claude's stop hook.

### Reserved trigger keys

These are re-checked by totsuka against the normalized task. Anything else is passed through to the plugin as an opaque value for it to interpret.

| Key | Matched against |
|---|---|
| `status` / `project_status` | The task's status |
| `label` (string) / `labels` (array) | The task's labels; an array requires all of them |
| `reaction` | A `reaction:<emoji name>` label |

### `reaction` — pick a workflow with an emoji

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }     # you react with :hammer: → implementation task
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"                  # mentions: catch-all, must be last
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- **Define reaction workflows before the catch-all.** After it they are unreachable and the emoji does nothing (you get a warning)
- The emoji name is a **string** in the form Slack reports, without colons. Writing `":eyes:"` works — the colons are stripped. Note that 👀 is `eyes` and 👁 is `eye`, which are different
- **A non-string value such as `reaction = 123` is a startup error.** An unreadable reserved key is skipped at match time, which would break things in two opposite directions at once: the workflow would match every task (and, sitting before the catch-all, swallow your mentions) while the plugin registered no emoji at all. Neither half reports an error on its own
- **Using the same emoji in two workflows is a configuration error**, rather than letting first-match silently pick one
- Combining this with the older `trigger_reactions` key in `plugins/slack.toml` is also an error. A configuration using only the old form still works, with a deprecation warning
- Only your own reactions start a task. There is no setting that relaxes this

**Mixed versions:** with a new plugin against an older core, the core has no `reaction` reserved key and the reaction workflow swallows every task. Upgrade the core before the plugin. When rolling back, remove reaction workflows from your configuration.

### `initial_prompt`

```toml
[[workflows]]
name = "github-design"
source = "github"
trigger = { project_status = "Design" }
profile = "design"
agent = "herdr"
on_success = { set_status = "Design Review" }
initial_prompt = "Use the /grill-me skill and produce a detailed design."
```

| Property | Behaviour |
|---|---|
| **Visible** | It appears in the pane. These instructions can change how the whole task is approached, so they stay auditable afterwards |
| **First** | It goes before the task body |
| **New conversations only** | It is not added when resuming a conversation. An opening instruction re-entered on the third turn would restart the skill and wreck the context. Tools that cannot resume start a new conversation every time, so they get it every time |
| **Literal** | No placeholder expansion, so `{` is safe to write |
| **Unset means unchanged** | An empty or whitespace-only value is treated as unset, and workflows without one are byte-for-byte identical to before |

**If you write instructions that make the agent ask a human something, an unattended pane will hang** until `timeout_secs` escalates it — nothing fires while a tool waits for an answer. totsuka does not append a caveat automatically, because that could contradict what you wrote.

### `profile` — the four archetypes

A profile names a combination of `mode`, `output`, and `verification` that fits together.

| profile | mode | output | verification | For |
|---|---|---|---|---|
| `answer` | `plan` | `source` | `llm` | Answering a question and replying in the source |
| `triage` | `plan` | `source` | `llm` | Filing an issue in GitHub or Notion |
| `design` | `plan` | `none` | `llm` | Writing a detailed design into an issue comment or page |
| `implement` | `implement` | `none` | `llm` | Implementing and opening a pull request |

```toml
[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "Ready for design" }
profile = "design"
agent = "herdr"
on_success = { set_status = "Designed" }
```

| Combination | Result |
|---|---|
| `profile` plus `mode` or `verification` | **Error.** The profile decides these, so writing them would leave dead settings that look alive |
| `profile` plus `output` | **Allowed**, and `output` wins. This is a wiring choice rather than a permission, and a Slack-triggered implement workflow needs it to return the pull request URL to the thread |
| No `profile` and no `mode` / `output` | **Error.** Either name a profile or write both |
| `profile` plus `rubric`, `[workflows.prompts]`, `tool`, `timeout_secs`, `on_success`, `on_failure` | Allowed |

Profiles are optional. Combinations they cannot express — `verification = "human"`, for instance, since all four resolve to `llm` — are written out explicitly.

**When rolling back:** a configuration using `profile` fails to parse on an older binary. Roll your configuration back along with totsuka.

A profile also decides several behaviours beyond those three keys:

| Behaviour | Profiles |
|---|---|
| Injects a `permissions.deny` set into Claude's settings | answer, triage, design |
| Denies `Bash` as a whole, so no command can run | answer |
| Does **not** pass Claude's `--permission-mode plan` | answer, triage, design |
| Fails the task if the worktree ends up on a branch, instead of treating it as success | answer, triage, design |
| Injects `permissions.defaultMode = "auto"` into Claude's settings | all |
| Tells the source plugin which kind of instructions to attach | triage, design, implement |
| Replaces the verification criteria with "the result URL really exists" | triage |
| Replaces the completion instructions with the confirmation protocol below | design, implement |
| Replaces the verification criteria with "a human explicitly approved" | design, implement |
| Tells the source plugin to file the task under a separate id prefix | implement (`impl:`), triage (`books:`) |
| Waits before dispatch if a required external tool is missing | implement |

### design and implement completions are approved by a human

`design` and `implement` assume an attended pane, and **a human makes the final call**:

1. When the agent thinks it is done it does **not** claim completion. It summarises what it did, asks for confirmation, and stops with "needs input"
2. totsuka parks the task as waiting for input — exempt from the timeout sweep, its concurrency slot released, a notification sent
3. Once you approve explicitly in the pane, the agent claims completion and the task finishes

Verification criteria change to match: the judge, which can see the conversation, checks whether a human approved before the claim. **An agent that skips the confirmation and claims completion is blocked by the same layer that catches a missing marker.** Stopping to ask is not a completion claim, so it is never blocked.

Pair this with `timeout_secs = 0` if you want to avoid spurious escalation during a long unattended stretch.

A known limitation: a second "needs input" while already waiting — you send corrections, the agent asks again — does not send another notification. In an attended pane you are part of the conversation anyway, so the impact is small.

### The verification-criteria ladder

From strongest to weakest:

1. `[[workflows]].prompts.verification_rubric`
2. `[[workflows]].rubric`
3. `[prompts].verification_rubric` (global)
4. The profile's default
5. The generic default

**Layer 3 beats layer 4**, so if you have set a global `verification_rubric`, a `triage` workflow does **not** get the result-URL check. The symptom is that a task claiming it "wrote the design" passes without having posted anything. If you use profiles, either drop the global rubric or set `[[workflows]].rubric` explicitly. The same ladder applies to the completion instructions: a global `marker_self_report` means `design` and `implement` do not get the confirmation protocol.

### Waiting for a missing external tool

An `implement` task opens a pull request, so it needs `gh`. If that is missing the task is **not dispatched and stays queued**, with one notification. Fix the environment and it starts on its own within a few minutes; no action is needed.

Because notifications scroll away, `totsuka status` also shows the reason:

```text
not starting yet:
  task 12 (2026-08-11T09:00:00Z): gh unavailable in the orchestrator's environment → …
```

With `--json` this appears as `wait_reason` on the task. **The display reflects what totsuka recorded; `status` does not re-check the tool** — it runs in your shell, where `gh` being visible says nothing about whether totsuka can see it. The message clears once the task dispatches, but **fixing your environment while `totsuka run` is stopped will not clear it** until `run` comes back around.

**This check can be wrong.** It runs in totsuka's process while the agent runs in a pane with your shell environment loaded, so **a setup where `gh` is only visible from the pane is reported as missing**. Because of that, `doctor` reports it as a warning rather than a failure, and dispatch waits rather than failing. If you know your setup, ignore the warning.

**What is not checked:** `triage` and `design` also write externally, but *where* depends on the source, and totsuka cannot tell from a plugin instance name. Guessing wrong would block tasks that would have worked, so it does not guess. `doctor` says so explicitly with a skipped check. The check also only asks whether the tool is configured — it never runs `gh auth status`, so an expired token passes here and fails in the pane as before.

### Starting an implementation task from a reaction

Rather than widening a running task's permissions, react to it and start a separate task.

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
output = "source"                 # so the PR URL goes back to the thread
agent = "herdr"

[[workflows]]
name = "slack-reply"              # catch-all, must be last
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- The task id is distinct from the thread's answer task, so they do not collide
- **What the agent sees depends on where you react.** On the first message of a thread (or a standalone message) it gets the whole conversation; on one reply inside a thread it gets only that message
- The repository is inherited from the conversation. If the answer task already resolved it, no LLM call and no picker
- The report goes through the approval gate, since a mistaken implementation report is expensive

**Limitations.** Thread history is clamped at 200 messages, so longer threads lose the oldest. And reacting while the answer task is still running gives you two tasks in parallel — they use separate worktrees so nothing breaks, but implementation starts before the approach is settled.

### Source plugin instructions

`plugins/github.toml` and `plugins/notion.toml` accept a `[prompts]` table with the instructions a plugin attaches when a profile tells it what kind of task this is.

| Key | Used when | Placeholders |
|---|---|---|
| `triage_instructions` | `profile = "triage"` | github: `{issue_number}`, `{repo}` / notion: `{page_url}`, `{title}` |
| `design_instructions` | `profile = "design"` | as above |
| `implement_instructions` | `profile = "implement"` | as above |

All are optional. **Without profiles these keys are never used** and task instructions stay empty as before.

The Slack source reads the same signal and picks from its own three keys. **The choice is made on the kind, not on the task id prefix** — both `triage` and `implement` have prefixes, so branching on the prefix hands implementation instructions to a triage task. When the kind is unknown it falls back to reply instructions rather than guessing.

**Setting `profile = "design"` on a Slack source does nothing visible.** The Slack plugin has no design instructions, and `design` outputs nothing, so the agent works and the result goes nowhere. Configuration validation passes, so the plugin logs a warning at dispatch. Use `triage` if you want Slack to file something.

Expansion is single-pass: an issue title or page name is written by someone else, so a `{placeholder}` in it is inserted as text and never becomes an instruction.

## `mode = "plan"` does not structurally stop git

Plan mode is defined as "create a worktree but do not push or open pull requests", and the implementation was written assuming permission modes and sandboxes enforced that. **In practice that assumption broke** — a plan-mode task created a branch, committed, pushed, and opened a pull request, because the target repository's own instructions told it to. Claude's `--permission-mode plan` has been measured writing files while still in plan mode, so **do not count it as a write barrier**.

**Plain `mode = "plan"` without a profile still only detects.** A branch appearing in the worktree makes `run` warn, naming the branch. This stays a warning deliberately so upgrades do not silently tighten existing setups.

**A workflow with a profile fails instead.** Note that **a read-only profile is not a guarantee**: an OS-level sandbox was measured as feasible but deliberately not implemented, and writes via `cat >` or git and gh behind `&&` or a pipe get past the deny list. When a read-only profile's worktree is on a branch, the task fails without publishing, and the worktree and commits are kept for inspection. **This is not prevention** — once there is a branch, a push may already have happened and cannot be taken back. Failing only avoids calling it a success. To recover, detach the worktree and then `totsuka task retry` (retrying as-is fails the same check), or `totsuka task cancel`.

If you are choosing plan mode because you want no side effects, check whether the target repository's own conventions tell agents to push or open pull requests.

## `[tools.{name}]`

Defines the AI tool CLI launched inside the pane. `claude`, `codex`, and `opencode` always exist as built-ins and can be overridden by an entry of the same name. You can also define a second profile of the same kind, such as `claude-fast`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `kind` | enum | required | Adapter: `claude`, `codex`, or `opencode`. Decides how the command line is built and how completion is detected |
| `command` | string? | the kind's name | Space-separated command line: the program plus base arguments, e.g. `"claude --model haiku"` |
| `mode_args` | string[]? | per kind | Extra arguments in implement mode. codex: `["--sandbox", "workspace-write", "--ask-for-approval", "never"]`; opencode: `["--auto"]`; claude: none |
| `plan_args` | string[]? | per kind | Extra arguments in plan mode. claude: `["--permission-mode", "plan"]`; codex: `["--sandbox", "read-only", "--ask-for-approval", "never"]`; opencode: `["--agent", "totsuka-plan", "--auto"]` |

Using `kind = "codex"` needs a one-time trust setup in the tool itself. `kind = "opencode"` needs no trust step but degrades in more places.

The adapters differ in how they resume and how they receive hook configuration. Claude takes a settings file and resumes with a flag; codex registers hooks globally and resumes with a subcommand; opencode also registers globally and resumes with a flag. opencode has no invisible injection, so task instructions and the marker convention reach it as visible context in the pane.

### Not stopping at approval prompts

**There is nobody in the pane to answer**, so all three tools are launched configured not to ask.

| Tool | Setting | Where |
|---|---|---|
| claude | `permissions.defaultMode = "auto"` | The settings file, for workflows with a profile |
| codex | `--ask-for-approval never` | Default arguments in both modes |
| opencode | `--auto` | Default arguments in both modes |

**This does not widen what an agent may do.** The boundaries are held by separate mechanisms and this setting does not loosen them: Claude's deny list applies in every permission mode, codex's `--sandbox` is a different flag from its approval policy, and opencode's `--auto` auto-approves everything *except* what is explicitly denied, so the plan agent's denials stand.

What changes is only whether a human is asked about things the boundary does not reject.

Left alone, an unconfigured claude launches in its manual mode and stops dead on `Do you want to proceed?` before any command not on its allowlist. codex asks whenever the model decides it should, and opencode asks about a couple of categories.

**Setting `mode_args` or `plan_args` replaces the defaults wholesale**, including these flags. Add them back yourself if you run unattended.

Tool resolution at dispatch is workflow pin, then repository default, then `default_tool`, then the built-in `claude`.

## `[prompts]`

Overrides for the prompt text injected into the AI tools. The built-in defaults are embedded in the binary, and this table overrides **per key** — anything you do not set stays built-in. Values are inline strings only; there is no form that points at a file.

| Key | Default | What it is | Placeholders |
|---|---|---|---|
| `marker_self_report` | built-in, varies by profile | The completion self-report instruction injected into every dispatch. For `design` and `implement` the default is the confirmation-protocol version. Overriding this key beats the profile default | `{marker_completed}`, `{marker_needs_input}`, `{marker_failed}` |
| `branch_convention` | built-in | Branch creation instructions. Worktrees arrive detached, so the agent reads the repository's conventions and creates the branch. **Not injected in plan mode**, nor for a task already on a branch | none |
| `verification_rubric` | built-in, varies by profile | The clause describing when a completion claim is acceptable. **Write it as a condition, not an instruction** — see below | — |
| `verification_background_exemption` | built-in | The clause covering an intermediate stop while a background task runs | — |
| `verification_nonclaim_exemption` | built-in | The clause covering a stop that reports "needs input" or "failed" | `{marker_needs_input}`, `{marker_failed}` |
| `verification_marker_convention` | built-in | What to write in the reason when blocking. The reason goes back to the agent, so this is where the marker convention is taught | `{marker_completed}`, `{marker_needs_input}`, `{marker_failed}` |
| `verification_prompt` | see below | How the clauses are assembled | `{rubric}`, `{background_exemption}`, `{nonclaim_exemption}`, `{marker_convention}` |
| `opencode_plan_agent` | built-in | The prose body of opencode's plan agent file. **Global only** — writing it under a workflow is a parse error, since one file on disk is shared by every session | — |

> **The `verification_*` keys are conditions, not instructions.** Claude Code passes the hook body to the model under a fixed system prompt and takes back a verdict; a false verdict blocks the stop and the reason is handed to the agent. **The model does not control blocking, so writing "please allow this and do not block" has no effect.** That exact wording shipped once, and the judge quoted it verbatim while refusing eight times in a row. Write text that is **true in every case you want allowed**.

The five `verification_*` keys are used only by workflows with `verification = "llm"`. Only Claude has the stop hook they need; other tools degrade to `human`.

`opencode_plan_agent` is the **prose body only**. Its frontmatter is fixed in code and cannot be configured, because that deny map is the only mechanism carrying plan intent — and letting a prose-looking key inject an allow would be a privilege escalation. A value containing a line of `---` anywhere is rejected: frontmatter is conventionally only read at the top of a file, but this is a permission boundary and the design does not rely on that inference. Write horizontal rules as `***`.

**The markers themselves cannot be configured.** The hook scripts parse them literally, and they are the single completion signal shared by all three tools. What you can edit here is the prose that *teaches* the convention, not the convention.

### Precedence

Strongest first:

1. `[[workflows]].prompts.<key>` — this workflow only (except `opencode_plan_agent`, which is global)
2. `[[workflows]].rubric` — legacy, affects the rubric only
3. `[prompts].<key>` — global
4. The built-in default

**Layer 2 beating layer 3 is deliberate.** Both are per-workflow settings, and the other order would mean that adding a global `verification_rubric` silently overwrites every existing per-workflow `rubric`.

### Expansion rules

- Placeholder substitution is **single-pass**. A `{token}` inside a substituted value is not expanded again
- Assembly happens in two stages, each single-pass, so a literal `{marker_convention}` written inside a rubric is inserted rather than expanded
- Placeholder names must be identifiers, so other braces pass through as content and you can write JSON such as `{"ok": true}` in a prompt. The flip side is that a typo which is not a valid identifier — `{marker-needs-input}` — is not caught by placeholder checking. Markers are checked separately by confirming all three appear in the assembled output
- A `{` nested inside braces makes the whole span one unknown name, dropping the real placeholder inside it. This is reported as a warning
- The `[worktree]` templates use a different substitution, so **everything inside their braces is checked** and a typo like `{repo-name}` stays an error
- Unknown placeholders are passed through unchanged
- **Prompt changes take effect from the next dispatch.** An already-running agent does not see them

### Example

```toml
[prompts]
verification_rubric = "Confirm the change works by actually running the tests."

[[workflows]]
name = "slack-reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"

  [workflows.prompts]
  verification_rubric = "Check that the draft answers the question directly and shows its reasoning."
```

## `[llm]`

Assumes an OpenAI-compatible `/chat/completions`. Used to pick a repository for tasks that carry no hint, and supplied to task source plugins as their default classifier (a plugin's own LLM settings always win).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `base_url` | string | required | Base URL, e.g. `https://openrouter.ai/api/v1` |
| `model` | string | required | Model name |
| `max_tokens` | int? | 256 | Maximum tokens for a classification call |
| `timeout_secs` | int? | 30 | Request timeout |
| `api_key_ref` | string? | none | Secret reference for the API key |

## `[worktree]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `location` | string? | `<state dir>/worktrees/{repo_name}/{worktree_name}` | Placement template. Expands `{repo}`, `{repo_name}`, `{worktree_name}`, `{task_id}`, `{source}`, `${ENV}`, and `~`. **`{branch}` was removed** — the agent chooses the branch after the worktree exists, so it cannot appear in the directory name. Leaving it in stops startup |
| `cleanup` | policy? | `manual` | Cleanup policy for implement mode |
| `plan_cleanup` | policy? | `immediate` | Cleanup policy for plan mode |

**Resolving the default.** With `location` omitted, `<state dir>` is `$XDG_STATE_HOME/totsuka`, falling back to `$HOME/.local/state/totsuka`. The default is built as an already-resolved path, so it never goes through `${ENV}` expansion. If you **do** set `location`, an unset `${ENV}` is an error rather than an empty string — and since worktrees are created at dispatch, it shows up as every task failing rather than as a startup failure. `doctor`'s `worktree-location` check finds it first.

Policy values are `"immediate"`, `"manual"`, `{ retention_days = 5 }`, `"keep_7d"`, and `"keep_28d"`. The `keep_*` forms are sugar for 7 and 28 days; other durations use the explicit form. A worktree with uncommitted changes is never deleted.

```toml
[worktree]
cleanup      = "keep_7d"              # implement: delete after 7 days
plan_cleanup = "immediate"            # plan: delete right away (the default)
# cleanup    = { retention_days = 3 } # any other number of days
```

**Panes follow worktrees.** When a worktree is judged deletable, the task's pane is closed first. Panes of worktrees kept back — still within retention, set to `manual`, or holding uncommitted changes — stay open. **With the default `cleanup = "manual"`, neither the worktree nor the pane goes away, so panes accumulate one per task.** Unless you specifically want to inspect committed-but-unpushed work in the pane, `keep_7d` is the better choice.

## `[log]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `level` | string? | info | `error`, `warn`, `info`, `debug`, or `trace`. `--debug` raises it to debug |
| `log_prompts` | bool | true | Record prompts and payloads; only actually written at debug or above |
| `max_files` | int? | 7 | How many daily log files to keep |

## `[hooks]`

Settings for receiving agent CLI hook events. Every key is optional.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `auth_token_ref` | string? | none | Secret reference for the bearer token authenticating hook posts, e.g. `keychain:totsuka/hook-token`. **Required in practice** — without it the only protection is socket permissions |
| `socket_path` | string? | built-in | Path of the receiving socket |
| `spool_dir` | string? | built-in | Where events are spooled when a post fails |
| `block_retry_limit` | int? | 3 | Consecutive stop-hook blocks before escalating |

If a workflow uses a hook-capable agent, leaving `auth_token_ref` unset makes `config validate` and `run` warn per workflow, and makes `doctor` **fail**. Without any hook-capable agent, `doctor` only warns. A reference that is set but cannot be resolved always fails.

## `plugins/slack.toml`

The Slack source is event-driven — it pushes each event as it arrives — so `poll_interval_secs` is unused.

```toml
[plugins.slack]
enabled = true
kind = "task_source"
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `app_token` | string | required | App-level token (`xapp-`) for the socket connection. Use a secret reference |
| `user_token` | string | required | User OAuth token (`xoxp-`) for reading and writing as you. Use a secret reference |
| `bot_token` | string? | none | Bot token (`xoxb-`). With it set, a bot sends you a direct message when a draft or a picker arrives. **Without it the feature is simply off** (one warning at startup) |
| `target_user_id` | string | required | Your Slack user id. Mentions of this user become tasks, and it is checked against the token's own identity |
| `trigger_reactions` | string[] | `[]` | **Deprecated** in favour of `trigger.reaction` on a workflow. Emoji names that start a task when **you** react. Colons are stripped. Needs the `reactions:read` scope |
| `thread_context_limit` | int | 6 | How many recent thread messages to include in the task body |
| `reply_style` | string? | none | Tone instructions injected into the task body |
| `[prompts]` | table | — | Overrides for the prompts this plugin sends |
| `source_name` | string | `slack` | The source name stamped on each task |
| `[[repos]]` | array | none | Candidate repositories: `name` (must match one in `config.toml`), optional `summary` and `path`. **Omit it and the repositories from `config.toml` are used**, which is usually what you want |
| `[[channel_groups]]` | array | none | Narrow the candidates by channel name prefix; first match in definition order. `prefix` plus `repos` |
| `[llm]` | table | none | The classifier LLM: `base_url`, `model`, `api_key`, and `confidence_threshold` (default 0.6; below it you get a picker). **Omit it and `config.toml`'s `[llm]` is the default**, provided it has a key. With two or more candidates and neither source of settings, startup fails |
| `api_url` | string | `https://slack.com/api` | Web API base URL, for testing |
| `max_retries` | int | 3 | Retries for retryable API failures |

### `[prompts]` for the Slack source

Per-key overrides for the prompts this plugin sends; the key name is the setting name.

| Key | Used for | Placeholders |
|---|---|---|
| `reply_instructions` | Drafting a reply. The default for `answer`, and the fallback when the kind is unknown | — |
| `implement_instructions` | The default for `implement`: implement, open a pull request, report the URL | — |
| `triage_instructions` | The default for `triage`: file an issue, report the URL | — |
| `reply_style_suffix` | Appended to the reply instructions only when `reply_style` is set | `{style}` |
| `body_template` | The task body shown in the pane | `{sender}`, `{channel}`, `{text}` |
| `body_thread_header` | Heading of the thread context section | `{count}` |
| `body_thread_line` | One line of thread context | `{line}` |
| `body_thread_unavailable` | Replaces the whole section when the context could not be fetched | — |
| `classifier_system` | System prompt for repository classification | `{repo_names}` |
| `classifier_user` | The matching user message | `{mention_text}`, `{thread_context}`, `{catalog}` |
| `classifier_correction` | The retry turn when the response was not valid JSON | — |

Things to know:

- **`{text}` arrives already quoted.** The rewrite happens before expansion, so a template that drops the leading `> ` does not break continuation lines, and one that keeps it does not double-quote
- **`{text}`, `{thread_context}`, and `{catalog}` contain content chosen by whoever posted in Slack.** Expansion is single-pass, so a mention containing the literal text `{catalog}` is inserted as that string and does not splice in the candidate list
- Unknown placeholders pass through and are logged as a warning at startup. **This is deliberately not an error**: the symptom is a visible `{token}` in the draft, whereas the core's prompts fail hard because what goes missing there is the completion convention and the only symptom would be a timeout
- These are **LLM prompts only**. A bad override degrades classification or draft quality; unlike the core prompts, it cannot break completion detection

## `plugins/herdr.toml`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `socket_path` | string? | none | Explicit socket path; highest precedence |
| `session` | string? | none | Named session, resolved to a path under your herdr config. Used when `socket_path` is unset |
| `[layout]` | table | see below | Pane layout for dispatched work |
| `[kind_map]` | table | `{}` | Maps an executable name to herdr's own vocabulary |
| `[identity]` | table | `{ enabled = true }` | Whether dispatch reports the repository and task to herdr |
| `request_timeout_secs` | int | 30 | Timeout for a single socket call |

Socket resolution order: `socket_path`, then `session`, then `HERDR_SOCKET_PATH`, then `HERDR_SESSION`, then the default path.

**herdr 0.7.5 or newer is required.** Against anything older, initialization is refused and `config validate` and `doctor` name the version.

### `[identity]`

```toml
[identity]
enabled = true   # the default
```

Dispatch reports metadata to **both** the workspace and its root pane, because the sidebar resolves names differently in each panel — reporting to only one fixes only one of them.

| Token | Value |
|---|---|
| `totsuka_task` | The task id verbatim. It is a machine identifier used for comparison, so it is never reformatted or truncated; an id too long for herdr's limit is simply not sent, because a truncated identifier is worse than none |
| `repo` | The repository name (absent from older orchestrators) |
| `task` | The task title, for display: whitespace collapsed and truncated |
| `mode` | `plan` or `implement` |

**totsuka does not rewrite your sidebar layout.** Your herdr config belongs to you and to herdr. **In an environment without the sidebar snippet, reporting changes nothing visible** except the label.

Only when both reports succeed is the workspace label renamed to `{repo}: {title}`. The machine-readable ownership marker is written when the workspace is created, so it survives a failed rename. Without a repository name there is no rename.

**A failed report never fails the dispatch** — it only logs a warning. Identity is decoration, and losing a runnable task because herdr hiccuped costs more.

Setting `enabled = false` stops the reporting entirely.

### `[kind_map]`

herdr picks the executable from its own fixed vocabulary, so the plugin translates a **file name** into it. `claude`, `codex`, and `opencode` pass through, so you usually need nothing here. You need it for a wrapper script under a name herdr does not know:

```toml
[kind_map]
my-claude = "claude"
```

- The key is matched against the **file name**, not the path, so `/opt/bin/my-claude` is looked up as `my-claude`
- Values are not validated. herdr rejects an unknown one itself; duplicating its vocabulary here would silently drift when herdr adds to it
- This does not belong in the `[tools]` registry, which is shared across agents — herdr-specific vocabulary there would leak into setups that never use herdr

### `[layout]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `shell` | bool | `true` | Whether to open a companion shell pane. With `false` the agent goes full screen and the other two keys are ignored |
| `direction` | `"down"` or `"right"` | `"down"` | Split direction. Any other value is a startup error — `up` and `left` do not exist in herdr |
| `ratio` | float | `0.8` | The **agent's** share. **Not range-checked**; it is passed straight through |

- The default puts the agent on top at 80% with a shell below
- **The companion shell does not get the hook environment variables**, including the bearer token, so that a shell you type into does not hold one
- **A failed layout does not fail the dispatch.** It warns and continues, falling back to no shell or herdr's own default arrangement. An invalid `ratio` rejected by herdr takes the same path

## Example

A design-to-implementation handoff:

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "Ready for design" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { set_status = "Ready for design review" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "Ready to implement" }
mode = "implement"
agent = "herdr"
output = "source"
on_success = { set_status = "Ready for review" }
```

---

This page is generated from the internal document `ai-docs/development/config-reference.md`, which carries the design decisions and measurements behind it.
