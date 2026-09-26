> 🌐 **English** · [日本語](config-reference.ja.md)

<!-- generated-from: ai-docs/development/config-reference.md sha256:16fcae12ec1cbad48b4b1dce6f7356259513a62317ff8eb45febde0d0db7d257 -->

# Configuration reference

Every key in `config.toml` — totsuka's own and each plugin's — with its type, default, and meaning.

## Where the file lives

**There is one configuration file:** `$XDG_CONFIG_HOME/totsuka/config.toml` (by default `~/.config/totsuka/config.toml`).

- `--config <path>` overrides its location
- A plugin's own settings are a top-level `[<name>]` table in the same file. totsuka keeps it uninterpreted and passes it to the plugin once secrets are resolved

The separate `plugins/{name}.toml` files are gone. If you still have them they are not read, and they do not produce an error either — delete them when you move your settings across.

`totsuka setup` writes a template. **Every key this page documents is in that template, commented out**, with a one-line summary — so the fastest way to find a setting is usually to open the file rather than this page. `totsuka config validate` checks it; `totsuka config show [--redacted]` prints it.

## Secret references

Never write a plain secret into your configuration. Any string value can instead be one of:

| Form | Resolves from | When to use |
|---|---|---|
| `op://<vault>/<item>/<field>` | 1Password | **The usual choice.** Works outside macOS |
| `bw:<item>/<field>` | Bitwarden | The same role as `op://`, for Bitwarden users. Needs `BW_SESSION` exported |
| `cmd:<command>` | The standard output of a command | Credentials another tool owns and rotates, e.g. `cmd:gh auth token` |
| A string containing `${ENV_VAR}` | Environment variables | A value you already export. `totsuka setup --secret-backend env` writes `TOTSUKA_SECRET_<ACCOUNT>` names, which are exempt from the unknown-override warning that every other unrecognised `TOTSUKA_*` gets |
| `keychain:<service>/<account>` | The macOS Keychain | macOS only |

`~` and `${ENV}` are also expanded in paths.

**`op://`** shells out to the 1Password CLI and assumes you have already run `op signin`. It works on **any string value** in either config file, and because the CLI is cross-platform it works outside macOS (`keychain:` is the macOS-only form; `${ENV_VAR}` and `cmd:` run anywhere but hold nothing themselves). A missing CLI, a missing item, and a missing sign-in each produce a specific, actionable error. `totsuka doctor` only probes 1Password when your configuration actually contains an `op://` reference.

**`bw:`** shells out to the Bitwarden CLI — `bw:totsuka-slack/password` becomes `bw get password totsuka-slack`. `<field>` is a `bw get` object name (`password`, `username`, `totp`, `uri`, …), so a reference reads the way you would invoke the CLI yourself, and the vocabulary is not checked here: a new `bw get` object works as soon as your `bw` supports it. **The split is at the last `/`**, so an item name may contain one — `bw:github.com/myorg/password` means item `github.com/myorg`, field `password`. Note that this is the opposite of `keychain:`, where everything after the *first* `/` is the account.

Bitwarden has no background session, so **`BW_SESSION` must be exported**: run `bw unlock`, export the session key it prints, and start `totsuka run` from that shell. Without it the reference fails immediately instead of starting `bw` at all — `bw` asks for your master password on standard input, and a long-running `totsuka run` that reaches that prompt stops with nothing on screen. If the item name matches more than one entry, the error tells you to use the item's id instead. `totsuka doctor` only probes Bitwarden when your configuration actually contains a `bw:` reference, and it treats the vault as usable only when `bw status` reports `unlocked`.

**Custom fields are out of scope for this form**, because the Bitwarden CLI has no single command that returns one. Use `cmd:` for those:

```bash
cmd:bw get item totsuka-slack | jq -r '.fields[]|select(.name=="api_token").value'
```

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
| `[llm]` | table | none | Repository classification (a chat LLM or a decisions model). Without it, repository selection that needs a classifier falls back to `pending` |
| `[worktree]` | table | — | Worktree placement and cleanup |
| `[log]` | table | — | Logging |
| `[hooks]` | table | — | Receiving agent CLI hook events |
| `default_tool` | string? | `"claude"` | Default AI tool when neither the workflow nor the repository pins one |
| `[tools.{name}]` | table | — | AI tool registry; overrides and extends the built-ins |

## Schema versioning

The current schema is **v1**, and it has never been bumped.

A `config.toml` whose `version` does not match is rejected at startup validation, and **totsuka never rewrites your configuration**. `config validate`, `run`, and `doctor` share the same validation, so all three notice the same mismatch, but they treat it differently: `config validate` and `run` stop with an error (`config validate` exits 1; `run` exits 4, its code for a startup failure only you can fix), while `doctor` reports it as a failing `config` check and carries on with the other checks.

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
| `project` | string? | none | The tracker new items for this repository are filed into: the `name` of a `[[projects]]` entry. **One at most.** Leaving it out is normal — it means no tracker is configured |

## `[[projects]]`

One addressable domain of a task source: a GitHub Project board, a Notion database, a Slack workspace, a Discord guild.

Two things point at an entry. A workflow's `projects` names the domains it **draws tasks from**; a repository's `project` names the tracker its new items are **filed into**. For GitHub and Notion these coincide, but they are different relations.

**A source with a single domain still needs an entry.** Slack and Discord serve one each, so their entries are just the two keys below — but they cannot be left out, because a workflow points at a domain rather than at a plugin.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Stable id that `[[repositories]].project` and `[[workflows]].projects` point at |
| `source` | string | required | The task source plugin that owns this domain |
| everything else | — | — | Belongs to that plugin. totsuka passes it through without reading it |

```toml
[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"        # read by the github plugin
owner_type = "user"
project_number = 6
triage_status = "Inbox"     # status a filed item lands in (omit for none)

[[projects]]
name = "design-db"
source = "notion"
database_id = "..."         # read by the notion plugin

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"
```

Writing `source` out is what lets `totsuka config validate` follow the chain `[[repositories]].project` → `[[projects]].name` → `[plugins.<source>]` **without launching a plugin**, so a broken reference is caught offline. `config validate` rejects a duplicate `name`, a `source` that is not an enabled task source, and a `project` pointing at an entry that does not exist.

Because `project` is a single value, a repository files into exactly one tracker. Two sources cannot end up claiming the same repository.

## `[plugins.{name}]`

`{name}` is the instance name a workflow refers to with `source` or `agent`. **This is the roster, not the settings** — a plugin's own settings go in the top-level `[<name>]` table.

The roster is also what makes a `[<name>]` table legitimate: **a top-level table whose name is not in it is a configuration error**. That catches a mistyped core key (`[worktre]`) and a mistyped plugin name (`[slak]`) alike.

**A plugin cannot be named after one of totsuka's own top-level keys** (`version`, `max_concurrency`, `repositories`, `projects`, `plugins`, `default_tool`, `tools`, `workflows`, `llm`, `worktree`, `log`, `hooks`, `prompts`). Its `[<name>]` table would be read as that key instead, and the plugin would start with an empty configuration and no complaint, so the roster entry is refused up front.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enabled` | bool | false | Whether it is active. Also toggled by `totsuka plugin enable/disable` |
| `kind` | enum | required | `task_source`, `agent_ide`, or `notifier` |
| `max_concurrency` | int? | unlimited | Per-agent-plugin limit on tasks running at once |
| `timeout_secs` | int? | 120 | Timeout for a single call to the plugin |
| `restart` | bool | true | Whether a crashed plugin is launched again. Retries back off (1s, 2s, 4s, …) up to **5 attempts within a rolling 5 minutes**, then send an `escalated` notification. **Setting it to `false` keeps the detection** — the death is logged, counted in the run summary's `plugin_crashes`, and still sends an `escalated` notification; an agent plugin's in-flight tasks are still failed. Only the relaunch stops, which is what you want while investigating a plugin by hand |

There is no `log_level` key: plugin logs are filtered by `[log] level` like everything else, and a config that still sets `log_level` fails at startup.

## `[[workflows]]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | Workflow name |
| `projects` | array of strings | required | The `[[projects]]` entries this workflow **draws tasks from**, by `name`. **The task source is not written here** — it is the owner of those domains. An empty list is an error (there would be nothing to route it to). Naming more than one states that those domains share a lane vocabulary; naming domains owned by different sources is an error. Note the plural: `[[repositories]].project` is a single value, this is a list |
| `trigger` | table | `{}` | Trigger condition. **Deciding which tasks match is the source plugin's job.** Keys are ANDed, a list is an OR, and `exclude` drops a task matching any one of its conditions — the rules and the keys each source reads are in "The `trigger` vocabulary" below |
| `profile` | enum? | none | One of `answer`, `triage`, `design`, `implement`. Decides `mode`, `output`, and `verification` together |
| `mode` | enum | required without `profile` | `plan` or `implement` |
| `agent` | string | required | Agent instance name |
| `output` | enum | required without `profile` | `source` or `none` |
| `on_start` | `{ status = "..." }`? | none | Update the status in the source right before the task is handed to an agent, so the board mirrors "in progress" while the run happens. In a multi-member setup this also makes `in_progress_statuses` keep other members' instances from picking the task up. Omitted, nothing is written. **If you set it, set `on_failure` too** — otherwise a failed task leaves the column stuck on the in-progress status. **An unknown key in `on_start` / `on_success` / `on_failure` is a startup error** (`status` is the only valid key). The check exists because the breakage is silent: write `set_stauts` and the task still runs, still succeeds, and only the board stops moving |
| `on_success` | `{ status = "..." }`? | none | Update the status in the source on success |
| `on_failure` | `{ status = "..." }`? | none | Update the status in the source on failure. Retryable failures do not write back |
| `verification` | enum | `llm` | How a completion claim is checked: `llm` (checked in session), `human` (waits for `totsuka task verify`), or `none`. Cannot be combined with `profile` |
| `timeout_secs` | int? | 0 | Seconds of silence after the last signal before escalating. **`0` (the default) opts this workflow out of the timeout sweep entirely**. Time spent waiting on a permission or idle prompt does not count as silence |
| `rubric` | string? | none | The criteria used for `llm` verification. **The only prompt override there is** (see below); it beats the profile's default |
| `tool` | string? | none | Pins the AI tool. Workflow beats repository beats `default_tool` |
| `initial_prompt` | string? | none | Extra instructions prepended for this workflow's agent. See below |
| `cleanup` | same values as `[worktree]` | none | Worktree cleanup override for this workflow's tasks. Beats the mode default in `[worktree]`. `manual` keeps the worktree **and its pane** open after the task finishes. If you later remove or rename the workflow in config, finished tasks fall back to the mode default |

Workflows are matched in definition order, first match wins — **and the source plugin is what runs that match**. It receives your workflows at startup, decides which one a task belongs to, and names it when it hands the task over. totsuka checks only that the name exists and belongs to that source.

### The `trigger` vocabulary

What a trigger means is up to the source plugin: it receives your workflows at startup and runs first-match. One key is totsuka's own: **`status`** names the source's status column, and totsuka reads it to build the column graph its cycle check walks — it only compares that string against an `on_*` write-back, and never uses it to match a task. Whether a source accepts the key is up to that source; Slack has no status column and rejects it as unknown.

#### Common rules

GitHub and Notion triggers follow these rules (Slack and Discord pick a *kind* of trigger instead — see "Slack / Discord keys" below):

- **Keys are ANDed; a list is an OR.** `trigger = { status = "Todo", label = ["bug", "chore"] }` means "in the `Todo` column, and labelled `bug` or `chore`". Keys stay singular even when they take a list (`label`, `assignee`)
- **`exclude` drops a task that matches any one of its conditions.** It uses the same keys and values as the trigger — see below
- **An unknown key in a trigger is a hard startup failure.** A trigger is read key by key, so a key nobody reads is dropped and the condition simply goes away — which means a typo does not narrow the trigger, it *widens* it (write `assinee` and you get "no condition", firing on exactly the tasks you meant to exclude). The error lists the keys the source does read, so it doubles as migration guidance. **Keys inside `exclude` are checked the same way**
- `trigger = {}` has no keys, so the unknown-key check always passes it — but whether it *means* anything is up to the source, and Slack rejects it as naming no trigger at all.

| Source | Keys that select tasks | Keys allowed inside `exclude` |
|---|---|---|
| github | `status` (string) / `label` (string or list) / `assignee` / `exclude` | `status` / `label` / `assignee` (`status` and `label` take lists) |
| notion | `status` (string) / `assignee` / `filter` / `exclude` | `status` (takes a list) / `assignee`. **Not `filter`** (below) |
| slack | `mention` / `to_group` / `reaction` / `from_bot` / `channel` / `channel_name` / `repo` / `from` | none |
| discord | `channel` / `channel_name` / `repo` / `from` | none |

#### `status`

For GitHub's `status` triggers, **entering the column is the request**: even after completion, a human moving the card back into the trigger column re-runs the same workflow (who re-runs it is decided by the assignee and the claim). If the card lands in **another** workflow's trigger column, the conversation is handed over to that workflow — the next stage of a column pipeline continues with the same worktree and the same agent session. Only a finished conversation is handed over. A delivery that arrives while a stage is still running is passed over: with a **polling** source (github / notion) the next tick brings it back and the handoff happens then, but Slack acks first and never re-sends, so that trigger is lost — re-issue it once the run has finished.

#### `label` (GitHub)

Passes when the issue or PR carries any of the named labels. A string or a list (OR). **Matching ignores case** — GitHub keeps label names unique regardless of case, so `label = "Bug"` matches the `bug` label. A `label`-only trigger has no lane identity, so it runs **at most once** per task.

#### `assignee`

**`assignee` is the ingest gate for who may hold the task.** Write `"@me"`, `"@none"`, `"@any"`, a login, or a list of those (matched as an OR); **omitting it means `["@me", "@none"]`**, which is what totsuka did before the key existed. There is no second gate behind it, so a condition you write can never be overruled by one you did not. The `@` matters: `me`, `none` and `any` are all names a real account can have, so `assignee = "any"` means the user called `any`. **`@any` ingests other people's tasks too.** `@any` is also the one condition that does **not** read assignees, so on Notion you can write it even when `property_map.assignee` is unmapped — it is how you state that you do not filter by assignee. Every other value fails at startup without the mapping. What the names are matched against is source-specific — GitHub uses the issue's own assignees and `github_login`, Notion the property named by `property_map.assignee` and `notion_user_id`. On GitHub, an `assignee` with no `status` beside it gives its deliveries no lane identity, so that task runs **at most once** and re-assigning will not repeat it; a warning says so at startup. Notion mints no lane identity for any trigger, so adding a `status` there would not make a task repeatable — and no such warning is given, because it would not be advice that helps.

#### `exclude` — conditions that keep a task out

```toml
[[workflows]]
name     = "spec"
projects = ["my-board"]
agent    = "herdr"
profile  = "design"
trigger  = { status = "🤖 Spec", assignee = "@none", exclude = { label = "waiting" } }
```

Unassigned issues in the `🤖 Spec` column are picked up, except those labelled `waiting`. Remove the label and the next poll picks the issue up. **Adding `waiting` after a task was picked up does not stop it** — `exclude` is a condition on picking tasks up.

- Same keys and values as the trigger, and **any one match excludes** the task. A list is an OR here too, so `exclude = { label = ["waiting", "blocked"], assignee = "bot" }` excludes tasks labelled `waiting` or `blocked`, or assigned to `bot`
- `exclude.assignee` takes `@me`, `@none`, a login, or a list of those. Unlike the trigger's own `assignee` it has **no default** — leave it out and nobody is excluded. It needs the same settings to be evaluated (on Notion, `property_map.assignee`, and `notion_user_id` for `@me`); without them startup fails
- `exclude.status` takes a list. The trigger's own `status` stays a single string
- **Only key names are checked.** `exclude = {}` (excludes nothing) and `exclude = { assignee = "@any" }` (excludes everything) do what they say. A misspelt key such as `exclude = { lable = "waiting" }`, or an `exclude` nested inside `exclude`, fails at startup
- A trigger with only `exclude` is allowed; the default `assignee = ["@me", "@none"]` still applies
- **`in_progress_statuses` is separate.** It skips a board's "in progress" columns for every workflow, and writing `exclude` does not turn it off

#### Notion's `filter` — a raw Notion filter

`filter` is a Notion [database query filter](https://developers.notion.com/reference/post-database-query-filter), passed to the query as written. Every per-type operator works (`does_not_contain`, `does_not_equal`, `is_empty`, nested `and` / `or` …), so **on Notion you can write "does not contain" right inside `filter`**:

```toml
trigger = { assignee = "@none", filter = { and = [
  { property = "Status", status = { equals = "🤖 Spec" } },
  { property = "Tags",   multi_select = { does_not_contain = "waiting" } },
] } }
```

With `filter` present, `status` is not sent to Notion; it is only checked against the results. A value written as `@{name}` is resolved through `[notion.dynamic.{name}]` (see `[notion]`).

`filter` and `exclude` do different jobs:

| | `filter` (Notion only) | `exclude` (GitHub / Notion) |
|---|---|---|
| Evaluated by | Notion (in the query) | totsuka (after fetching) |
| Vocabulary | Notion's filter syntax | totsuka's `status` / `label` / `assignee` |
| How you negate | per operator (`does_not_contain`, …) | one `exclude` table |
| Use it for | anything Notion can query | conditions that read the same on every source |

**`filter` is not allowed inside `exclude`**: Notion has no general NOT to wrap a raw filter in. Write the negation inside `filter` instead.

#### Slack / Discord keys

**`mention` is the mention trigger** (Slack only): the workflow that writes `mention = true` is where mentions addressed to you go. It carries no ids — who counts as "you" is `[slack] target_user_id` plus the user groups you belong to, so there is nothing here to drift from them. Writing it beside `reaction` or `channel` is rejected, because two kinds would start it and the config would not say which. `mention = false` reads as the boolean it is and may sit beside a `reaction`, but a trigger holding only `mention = false` names no kind and is rejected.

**`to_group` routes mentions by who they are addressed to**, and only means anything beside `mention = true`. A mention of a user group you list goes to that workflow, and **a workflow with `to_group` always wins over a bare `mention = true` (the catch-all) — the order in `[[workflows]]` does not matter**. Order decides only the tie, when one message names two groups claimed by two workflows (`@oncall @design`): the one written first wins. Mentions of your other groups still fall to the catch-all, so adding one `to_group` does not stop the rest. **You may only list groups you belong to** — anything else is rejected at startup, so a conversation you are not part of cannot start an agent here. Adding `repo` pins the repository and skips resolution entirely (no `task/lookup`, no classifier), overriding whatever the conversation had settled on. **`repo` works without `to_group` too** — `trigger = { mention = true, repo = "web-app" }` sends every mention to that repository and never calls the classifier, which is what a single-repository setup wants. Writing `to_group` makes the `usergroups:read` scope required. **`totsuka config validate` cannot check membership** — that needs a live `usergroups.list`, and the command is deliberately offline, exactly as it cannot check a revoked token.

**`channel` is the channel watch trigger**: every top-level post in that channel becomes a task. It takes `channel_name` (required, checked against the live name so a rename is reported), `repo` (required, the repository those tasks go to) and `from` (extra people allowed to trigger it — **by default only your own posts do**). Writing it beside `reaction` is rejected, and so is writing the other three without `channel`. A watch is a trigger kind of its own, so it needs no `mention = true` — writing both is rejected

### Keys a plugin defines

A plugin can add its own keys to a workflow, written **flat**, next to totsuka's:

```toml
[[projects]]
name = "slack"  # a source with one domain still declares it
source = "slack"

[[workflows]]
name = "slack-books"
projects = ["slack"]
agent = "herdr"
profile = "triage"
publish = "direct"      # defined by the slack plugin
```

totsuka cannot tell whose key that is — a workflow names a `source` **and** an `agent`. So it does not decide: the leftover keys go to both plugins at startup, and each answers which ones it consumes.

| Claimants | Result |
|---|---|
| 0 | **Error.** Either a typo (`profil = "triage"` fails here) or a key meant for a plugin this workflow does not name |
| 1 | That plugin's key |
| 2 | **Error.** One key would mean two things; totsuka will not pick |

Both `totsuka run` and `totsuka config validate` enforce this. **`--offline` cannot** — it never launches a plugin, so it cannot ask.

Keys that exist today:

| Key | Owner | Meaning |
|---|---|---|
| `publish` | slack | `draft` (present it for approval first — the default) or `direct` (post immediately). A value neither of those **fails at startup**, so a typo cannot silently leave the approval gate in place — or take it away |

The default `timeout_secs = 0` suits attended workflows where a human is watching the pane. A genuinely hung agent is not detected either, so set a limit on unattended workflows.

If `verification = "llm"` may resolve to a non-Claude tool, you get a warning suggesting `tool = "claude"` — in-session verification needs Claude's stop hook.

### Which trigger keys work

Every key is interpreted by the source plugin; totsuka passes the whole table through untouched. What each source understands:

| Source | Keys |
|---|---|
| github | `status`, `label`, `assignee` |
| notion | `status`, a raw `filter`, `assignee` |
| slack | `reaction` (a workflow without one takes mentions) |

### `reaction` — pick a workflow with an emoji

```toml
[[workflows]]
name = "slack-implement"
projects = ["slack"]
trigger = { reaction = "hammer" }     # you react with :hammer: → implementation task
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"                  # mentions (order does not matter)
projects = ["slack"]
trigger = { mention = true }
profile = "answer"
agent = "herdr"
```

- The emoji name is a **string** in the form Slack reports, without colons. Writing `":eyes:"` works — the colons are stripped. Note that 👀 is `eyes` and 👁 is `eye`, which are different
- **Using the same emoji in two workflows is a configuration error**, rather than letting one silently win
- **Two workflows with `mention = true` is also an error** — a mention would go to whichever came first
- **Order does not matter.** Mentions and reactions arrive on different paths inside the plugin, so a reaction workflow written after the mention one is not shadowed by it
- Only your own reactions start a task. There is no setting that relaxes this

#### `from_bot` — let that emoji work on a bot's posts too

By default a reaction can only turn a **human** post into a task. To start from a notification a bot posts, list that bot's id on the trigger.

```toml
[[workflows]]
name = "pr-approval-review"
projects = ["slack"]
trigger = { reaction = "mag", from_bot = ["B0123ABC"] }
profile = "implement"
agent = "herdr"
```

- **It is still only your own reaction that starts anything.** An allowed bot posting does nothing on its own; what widens is only *what you can point the emoji at*
- What you write is the `bot_id` (`B…`) carried on the post — not the app's name and not a channel. Allowing a whole channel instead would admit every bot in it
- **Edits and deletions are still excluded**, even for an allowed bot. Only the post the bot actually made gets through
- A bot post carries no sender user id, so the sender shown in the pane is the `bot_id` itself
- **It is written per workflow; there is no global setting.** One global list would open every emoji you already use to that bot at once
- `from_bot` without a `reaction`, `from_bot` beside `channel` (a channel watch), an empty `[]`, and a value that is not shaped like a bot id (a `U…` user id, an app's display name) are all startup errors. Each of them fails silently as "I allowed a bot and nothing happens", so totsuka refuses to start instead
- **Mentions and channel watching still never turn a bot post into a task.** This setting applies to reactions only

### `initial_prompt`

```toml
[[workflows]]
name = "github-design"
projects = ["tomo-prj"]
trigger = { status = "Design" }
profile = "design"
agent = "herdr"
on_success = { status = "Design Review" }
initial_prompt = "Use the /grill-me skill and produce a detailed design."
```

| Property | Behaviour |
|---|---|
| **Visible** | It appears in the pane. These instructions can change how the whole task is approached, so they stay auditable afterwards |
| **First** | It goes before the task body |
| **New conversations only** | It is not added when resuming a conversation. An opening instruction re-entered on the third turn would restart the skill and wreck the context. Tools that cannot resume start a new conversation every time, so they get it every time |
| **Literal** | No placeholder expansion, so `{` is safe to write |
| **Unset means unchanged** | An empty or whitespace-only value is treated as unset, and workflows without one are byte-for-byte identical to before |

**If you write instructions that make the agent ask a human something, an unattended pane will hang** — nothing fires while a tool waits for an answer, so it stays stuck until `timeout_secs` escalates it, or indefinitely if you left `timeout_secs` at its default of `0`. totsuka does not append a caveat automatically, because that could contradict what you wrote.

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
projects = ["tomo-prj"]
trigger = { status = "Ready for design" }
profile = "design"
agent = "herdr"
on_success = { status = "Designed" }
```

| Combination | Result |
|---|---|
| `profile` plus `mode` or `verification` | **Error.** The profile decides these, so writing them would leave dead settings that look alive |
| `profile` plus `output` | **Allowed**, and `output` wins. This is a wiring choice rather than a permission, and a Slack-triggered implement workflow needs it to return the pull request URL to the thread |
| No `profile` and no `mode` / `output` | **Error.** Either name a profile or write both |
| `profile` plus `rubric`, `tool`, `timeout_secs`, `on_start`, `on_success`, `on_failure` | Allowed |
| A `status` the board does not have | **Error.** `trigger.status`, the `on_start` / `on_success` / `on_failure` write-backs, and each `[[projects]].triage_status` are checked against the board's real options. This runs in the **online part of `config validate` and in `doctor`** — not at startup, so removing one column from a board does not stop unrelated workflows. It needs the network, so `--offline` skips it. Only the domains a workflow actually names are queried, one request each. `in_progress_statuses` is not checked: it is shared across all of a source's domains, so a value absent from one board can still be right. **Without this check a misspelling stays silent** — narrowing a workflow to one board does not help, because a column name that does not exist simply never matches, with no error, no warning and no log line |
| `status` write-backs that form a **cycle of columns** | **Error.** Columns are nodes and write-backs are edges; a cycle re-runs forever with **no human in it**, dispatching an agent every lap. Writing back into your own trigger column is the length-1 case. The error names the actual route; the fix is to route one hop through a column no workflow triggers on, so a person moves the card out of it. Checked per `[[projects]]` entry, lexically only — two different boards that happen to share a column name are not a cycle, because the graph is kept separate per domain. A card does move between boards, but only because a person moved it, which needs a human every lap and so is not what this check is for. A workflow naming several domains contributes its write-backs to each of them, and one interlocking group is reported once, naming the board the check reached it on |

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

1. When the agent thinks it is done it does **not** claim completion. It summarises what it did and asks you to confirm
2. totsuka parks the task as waiting for input — exempt from the timeout sweep, keeping its concurrency slot, a notification sent
3. Once you approve explicitly in the pane, the agent claims completion and the task finishes

Verification criteria change to match: the judge, which can see the conversation, checks whether a human approved before the claim — an answer you selected in a question dialog counts. **An agent that skips the confirmation and claims completion is blocked by the same layer that catches a missing marker.** Stopping to ask is not a completion claim, so it is never blocked.

Leave `timeout_secs` at its default of `0` if you want to avoid spurious escalation during a long unattended stretch.

A known limitation: a second "needs input" stop while already waiting — you send corrections, the agent asks again in plain text — does not send another notification. In an attended pane you are part of the conversation anyway, so the impact is small. Questions asked through the picker below **do** re-notify.

#### Questions arrive as a picker, not free text

How the agent asks — for the completion confirmation above and for any other decision it needs mid-task — depends on the tool running in the pane:

- **claude**: the agent asks through `AskUserQuestion`, a single-select picker in the pane with options such as "Approve completion" and "Request changes". While the picker is open the task is parked as waiting for input (keeping its slot), and the notification you receive carries the question text. Answer in the pane and the conversation continues.
- **opencode**: the agent uses its native `question` dialog, with the same parking behavior.
- **codex**: has no question dialog outside plan mode, so the agent stops with "needs input" as before — but presents the choices as a short numbered list, so you can answer by typing just a number.

If the question tool is unavailable or fails, every tool falls back to the numbered list plus a "needs input" stop.

### The verification-criteria ladder

From strongest to weakest: `[[workflows]].rubric` > **the profile's default** (`triage` verifies the result URL, `design` and `implement` verify the human's approval) > the generic default.

**There used to be a global layer above the profile's default.** A global `verification_rubric` meant a `triage` workflow did not get the result-URL check, and a global `marker_self_report` meant `design` and `implement` did not get the confirmation protocol. The symptom in both cases was a task claiming it "wrote the design" passing without having posted anything — verification quietly getting looser. Rather than reorder the ladder, the global layer was removed. Only the workflow's own `rubric` outranks the profile's default now.

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
projects = ["slack"]
trigger = { reaction = "hammer" }
profile = "implement"
output = "source"                 # so the PR URL goes back to the thread
agent = "herdr"

[[workflows]]
name = "slack-reply"              # mentions (order does not matter)
projects = ["slack"]
trigger = { mention = true }
profile = "answer"
agent = "herdr"
```

- The task id is distinct from the thread's answer task, so they do not collide
- **What the agent sees depends on where you react.** On the first message of a thread (or a standalone message) it gets the whole conversation; on one reply inside a thread it gets only that message
- The repository is inherited from the conversation. If the answer task already resolved it, no LLM call and no picker
- The report goes through the approval gate, since a mistaken implementation report is expensive

**Limitations.** Thread history is clamped at 200 messages, so longer threads lose the oldest. And reacting while the answer task is still running gives you two tasks in parallel — they use separate worktrees so nothing breaks, but implementation starts before the approach is settled.

### Source plugin instructions

`[github.prompts]` and `[notion.prompts]` hold the instructions a plugin attaches when a profile tells it what kind of task this is.

| Key | Used when | Placeholders |
|---|---|---|
| `triage_instructions` | `profile = "triage"` | github: `{issue_number}`, `{repo}` / notion: `{page_url}`, `{title}` |
| `design_instructions` | `profile = "design"` | as above |
| `implement_instructions` | `profile = "implement"` | as above |
| `design_pr_instructions` | `profile = "design"` and the task is a pull request (github only) | `{pr_number}`, `{repo}` |
| `implement_pr_instructions` | `profile = "implement"` and the task is a pull request (github only) | `{pr_number}`, `{repo}` |

All are optional. **Without profiles these keys are never used** and task instructions stay empty as before.

The Slack source reads the same signal and picks from its own three keys. **The choice is made on the kind, not on the task id prefix** — both `triage` and `implement` have prefixes, so branching on the prefix hands implementation instructions to a triage task. When the kind is unknown it falls back to reply instructions rather than guessing.

**Setting `profile = "design"` on a Slack source does nothing visible.** The Slack plugin has no design instructions, and `design` outputs nothing, so the agent works and the result goes nowhere. Configuration validation passes, so the plugin logs a warning at dispatch. Use `triage` if you want Slack to file something.

**The built-in defaults are English, and they never name a language.** The language of the deliverable is decided by a rule the agent follows — write the reply in the same language as the thread, the issue, or the page it came from. When you override these keys, prefer to leave the language unnamed too: naming one overrides both your agent's own settings and the language of the source message. Name a language only when you want to force it.

The labels in the task **body** (`body_template` and the attachment / thread-context keys of the Slack source) are a separate decision and are left in Japanese: a human reads those in the pane, while the instructions above are read only by the agent.

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
| `env_file` | string? | none | A file of `KEY=value` lines whose values are added to the environment of every agent this tool launches (see below). After `~` / `${VAR}` expansion it must be an absolute path |

Using `kind = "codex"` needs a one-time trust setup in the tool itself. `kind = "opencode"` needs no trust step but degrades in more places.

The adapters differ in how they resume and how they receive hook configuration. Claude takes a settings file and resumes with a flag; codex registers hooks globally and resumes with a subcommand; opencode also registers globally and resumes with a flag. opencode has no invisible injection, so task instructions and the marker convention reach it as visible context in the pane.

### Choosing a model and a reasoning effort

**There is no dedicated `model` or `effort` key in `[tools.{name}]`.** The five keys above are the only ones accepted; anything else fails when the configuration is parsed:

```text
unknown field `model`, expected one of `kind`, `command`, `mode_args`, `plan_args`, `env_file`
```

Model and reasoning effort go into `command`, **as flags of the tool CLI itself**.

```toml
[tools.claude-fast]
kind = "claude"
command = "claude --model haiku --effort low"

[tools.claude-deep]
kind = "claude"
command = "claude --model opus --effort high"
```

The spelling belongs to the tool CLI, not to totsuka, so it differs per kind. The table below was checked against claude 2.1.233, codex 0.145.0, and opencode 1.18.4. **What gets launched is the interactive CLI** (`command` defaults to the kind's own name), so a flag that only exists on a non-interactive subcommand (`codex exec`, `opencode run`) is not available.

| kind | Model | Reasoning effort |
|---|---|---|
| claude | `--model <alias\|full-name>` | `--effort <low\|medium\|high\|xhigh\|max>` |
| codex | `-m, --model <MODEL>` | `-c model_reasoning_effort=<value>` (no dedicated flag) |
| opencode | `-m, --model <provider/model>` | **not settable on the interactive CLI** (see below) |

codex's `model_reasoning_effort` is a configuration override passed with `-c`, so **the CLI does not validate the value**. An invalid one still launches and fails on the first request instead (passing `bogusvalue` returns `Supported values are: 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', and 'max'.`).

opencode's reasoning effort is `--variant`, but **that flag belongs to `opencode run` (non-interactive) and does not exist on the interactive TUI that gets launched**. The alternative is to set `variant` on an agent in opencode's own `opencode.json` — though the official schema defines it as applying **only when the agent's configured model is used**, so writing `-m` in `command` may defeat it (unverified). Pick one place for the model and the variant rather than splitting them.

#### Switching per workflow

Tool resolution goes workflow pin > repository default > `default_tool` > built-in `claude`, so put several profiles in the registry and select one with `[[workflows]].tool`:

```toml
[[workflows]]
name = "triage"
tool = "claude-fast"

[[workflows]]
name = "implement"
tool = "claude-deep"
```

#### Do not put them in `mode_args` / `plan_args`

Those two **replace the kind's default wholesale**. Writing `plan_args = ["--effort", "low"]` drops claude's default `["--permission-mode", "plan"]`, removing the structural boundary of plan mode. Launch options that do not depend on the mode belong in `command`.

#### `command` is not a shell

`command` is only split on whitespace; shell quoting is not interpreted. **A single argument containing a space therefore cannot be written in `command`.** If you need one, use `mode_args` / `plan_args`, which are arrays — but then you have to restate the kind's defaults yourself, as above.

### Passing environment variables (`env_file`)

The agent process is started by the herdr / orca server, not by totsuka, so running `op run -- totsuka run` does not pass totsuka's own environment on to agents. Put the variables an agent needs (API keys, `GIT_CONFIG_*` for commit signing, …) in an `env_file`. It can point at the same file you already hand to `op run --env-file=… -- claude`:

```toml
[tools.claude-opus]
kind = "claude"
command = "claude --model opus"
env_file = "~/.claude/.env.tpl"
```

- **Syntax**: `KEY=value` lines, comment lines starting with `#`, and blank lines. One pair of surrounding `"…"` / `'…'` is stripped from a value (no escapes, no expansion). There are no trailing comments — anything after `#` is part of the value
- **Refused** (reported with the file and line number, never skipped): an `export ` prefix, multi-line values (an unclosed quote), `op run` templates (`{{ … }}`), duplicate keys, key names that do not match `[A-Za-z_][A-Za-z0-9_]*`, and **names starting with `TOTSUKA_`** (reserved for totsuka). Error messages never include a value
- **Values** are resolved like every other secret reference: `op://` / `keychain:` / `cmd:` / `bw:` come from their store, and anything else is a literal in which `${VAR}` is expanded (an unset variable is an error). The character sequence `${` itself therefore cannot appear in a value (a lone `$` or `$VAR` is kept as written)
- **Differences from `op run`**: `op run` only treats `op://` as a reference. totsuka also resolves values starting with `keychain:` / `cmd:` / `bw:` and expands `${VAR}` in literals. If both read the same file, do not write literal values that start with those prefixes
- **When**: resolved once, when `totsuka run` starts and before any plugin is launched, for **every** `[tools]` entry that has an `env_file` (including tools no workflow uses; a shared file is read once). The 1Password approval is the same single one at startup as for other `op://` references — nothing is resolved when an agent launches. If any value fails to resolve, `totsuka run` does not start. The values are kept for the life of the run and never re-read: **restart `totsuka run` after changing the file or the stored values**. `--dry-run` resolves nothing
- **Delivery**: the values are added to the launch environment of every agent that tool starts. herdr receives them as an API parameter and orca through a named pipe, so they never appear on the terminal screen
- **`totsuka doctor`** stays non-interactive and **resolves nothing**. Its `tool-env-file` check covers only that the file exists, its syntax, the shape of references, and `TOTSUKA_` names
- File permissions are not checked (the file normally holds references, not secrets)
- **Not covered**: resolved values stay in the memory of the `totsuka run` process. Anything that calls 1Password outside the agent launch — commit signing through the 1Password SSH agent, `op` in an rc file the orca terminal reads — is not affected by this setting

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

Tool resolution at dispatch is workflow pin, then repository default, then `default_tool`, then the built-in `claude`. totsuka builds the complete command line here, so the `agent_command` and `plan_args` keys under `[herdr]` — once a backward-compatibility fallback — **have been removed**. Leaving one in place makes the plugin refuse to start, naming the key and its replacement. Configure tools through `[tools.{name}]`.

## Prompt text

The prompt text injected into the AI tools is embedded in the binary and **cannot be overridden from configuration**. The one thing you can set is the criteria used to judge a completion claim, spelled `rubric` on the workflow.

A `[prompts]` table (8 keys) and a per-workflow `prompts` table (7 keys) used to exist. Both were removed, for two reasons:

- **One global key could silently disable a check a profile had chosen.** Setting `verification_rubric` globally meant `triage` stopped verifying the artifact URL; setting `marker_self_report` globally meant `design` and `implement` stopped using the human-confirmation protocol. Both failures lean the same way — verification gets *looser* — which is the direction you do not notice
- **The design moved past it.** Every prompt added after those tables was built-in and chosen by the workflow's `profile`, never configurable

**A configuration that still has one does not start.** Each key fails with an error saying what became of it:

```text
[prompts] sets `verification_rubric`, which was removed in favour of built-in
prompt text → write the criteria as `rubric` on the workflow itself — the one
prompt key that survived
```

| Removed key | Instead |
|---|---|
| `verification_rubric` | Write the criteria as `rubric` on the workflow |
| `marker_self_report` | Nothing replaces it. The completion protocol is chosen by the workflow's `profile` — `design` and `implement` get the human-confirmation variant — which is what an override here used to defeat |
| `branch_convention` | Nothing replaces it. The agent reads the branch convention out of the target repository |
| `verification_prompt`, `verification_marker_convention`, `verification_background_exemption`, `verification_nonclaim_exemption` | Nothing replaces them. How the judging prompt is assembled is built in; `rubric` is the part of it that was ever meant to be yours |
| `opencode_plan_agent` | Nothing replaces it. The prose of opencode's plan agent is built in — its permission deny map never was configurable |

**Changing prompt text now needs a rebuild.** That is a deliberate reversal: prompt text turned out to be part of how completion and verification behave, not a knob to tune.

### Writing a `rubric`

`[[workflows]].rubric` fills **one branch** of the judging prompt. Assembled, the whole thing reads:

```text
This stop may be allowed. That is, at least one of the following holds:

{nonclaim_exemption}      ← the final message reports "needs input" or "failed"
{background_exemption}    ← an intermediate stop while a background task runs
{rubric}                  ← your text goes here

{marker_convention}       ← what to write in the reason when blocking
```

> **A rubric is a condition, not an instruction.** Claude Code passes the hook body to the model under a fixed system prompt and takes back a verdict; a false verdict blocks the stop and the reason is handed to the agent. **The model does not control blocking, so writing "please allow this and do not block" has no effect.** That exact wording shipped once, and the judge quoted it verbatim while refusing eight times in a row. Write text that is **true in every case you want allowed**.

`rubric` is used only by workflows with `verification = "llm"`. Only Claude has the stop hook they need; other tools degrade to `human`. Setting it on any other verification mode is a warning.

**The markers themselves cannot be configured.** The hook scripts parse them literally, and they are the single completion signal shared by all three tools.

### Precedence

Strongest first:

1. `[[workflows]].rubric`
2. **The profile's default** — `triage` verifies the artifact URL, `design` and `implement` verify the human's approval
3. The built-in default

The removed tables sat above and *between* these, which is how one global key could reach past layer 2.

### Expansion rules

- **A rubric cannot contain placeholders.** A `{name}` is a validation error: branches are rendered on their own before the assembly fills `{rubric}`, so a name inside a branch has nothing to resolve against and ships as literal text
- Placeholder names must be identifiers, so other braces pass through as content and you can write JSON such as `{"ok": true}` in a rubric
- A `{` nested inside braces makes the whole span one unknown name. This is reported as a warning
- The `[worktree]` templates use a different substitution, so **everything inside their braces is checked** and a typo like `{repo-name}` stays an error
- Assembly happens in two stages, each single-pass, so a literal `{marker_convention}` written inside a rubric is inserted rather than expanded
- **A rubric change takes effect from the next dispatch.** An already-running agent does not see it

### Example

```toml
[[workflows]]
name = "slack-reply"
projects = ["slack"]
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"
rubric = "Check that the draft answers the question directly and shows its reasoning."
```

## `[llm]`

Used to pick a repository for tasks that carry no hint, and supplied to task source plugins as their default classifier (a plugin's own LLM settings always win).

`api` chooses the kind of API:

- **`chat`** (the default): an OpenAI-compatible `/chat/completions`. Point `base_url` at OpenRouter, LiteLLM, and so on. The model returns `{repo, confidence, reason}` as structured output
- **`decisions`**: a decision-only model such as TypeSafe Jev. It generates no text; it picks one of the candidates (or "none fits") and returns a probability for every candidate, and cannot answer outside them. **The default endpoint is OpenRouter's Decisions API (`https://openrouter.ai/api/alpha/decisions`), which OpenRouter labels alpha**, so set `endpoint` if it changes or to call TypeSafe directly (`https://api.typesafe.ai/v1/systemone`). Your OpenRouter API key works as is

| Key | Type | Default | Meaning |
|---|---|---|---|
| `api` | string? | `"chat"` | `"chat"` or `"decisions"` |
| `model` | string | required | Model name. For decisions, `~typesafe/jev-latest` (always the latest) or `typesafe/jev-1.13` (pinned) |
| `base_url` | string | required for chat | Base URL, e.g. `https://openrouter.ai/api/v1`; `/chat/completions` is appended. **An error with decisions** |
| `max_tokens` | int? | none | Maximum tokens for a classification call. When omitted, it is not sent (the provider default applies). **Chat only** (an error with decisions) |
| `endpoint` | string? | OpenRouter's Decisions API | The **full URL** of the Decisions API (not a base URL — gateways put it at different paths). **Decisions only** (an error with chat) |
| `timeout_secs` | int? | 30 | Request timeout |
| `api_key_ref` | string? | none | Secret reference for the API key |
| `confidence_threshold` | float? | 0.6 | A classification below this confidence is not used; the task goes to `pending` for a human to confirm. `0.0` to `1.0`; out-of-range values fail validation at startup. Not included in the default passed to task_source plugins (they keep their own threshold) |

A key that does not apply to the chosen `api` (`base_url` / `max_tokens` with decisions, `endpoint` with chat) fails config loading rather than being ignored.

**With decisions, the value compared against the threshold** is the chosen candidate's **probability** (say, 0.84 for `totsuka`), which reads the same way as a chat model's self-reported confidence. The separate `confidence` the API returns (how concentrated the distribution is — 0.6 is possible at a probability of 0.84) is only used when no probability comes back. Probabilities are **rounded to two decimals**, so the threshold is only meaningful in steps of 0.01. A task for which "none fits" is chosen goes to `pending` whatever the probability.

```toml
# chat (the default)
[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4-5"
api_key_ref = "keychain:totsuka/openrouter"

# decisions (TypeSafe Jev through OpenRouter; the same key as chat)
[llm]
api = "decisions"
model = "~typesafe/jev-latest"
api_key_ref = "keychain:totsuka/openrouter"
# endpoint = "https://openrouter.ai/api/alpha/decisions"   # the default; overridable because it is alpha
confidence_threshold = 0.7
```

## `[worktree]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `location` | string? | `<state dir>/worktrees/{repo_name}/{worktree_name}` | Placement template. Expands `{repo}`, `{repo_name}`, `{worktree_name}`, `{task_id}`, `{source}`, `{task_number}`, `{hash}`, `{handle}`, `${ENV}`, and `~`. `{worktree_name}` is `<task number>[-<handle>]-<8 hex>` — the number `totsuka status` and `totsuka task retry <n>` use, an optional short name from the source (GitHub `repo-number`, Slack and Discord the channel name, Notion none), and the first 8 hex characters of a digest over the source and its task id. The parts are also available separately, so you can join them differently or drop some. `{handle}` is **empty** when the source offers none, so do not use it alone as a directory name. It is also the one placeholder that is **normalized** before substitution (anything outside letters, digits, `-` and `_` is folded), because a plugin writes it and a `../` in a path would escape the worktree root; `{task_id}` and `{source}` stay raw so that existing custom templates keep rendering what they always did. `{task_id}` is the **source's own** id (for Slack, `{channel}:{ts}`). **`{branch}` was removed** — the agent chooses the branch after the worktree exists, so it cannot appear in the directory name. Leaving it in stops startup |
| `cleanup` | policy? | `manual` | Cleanup policy for implement mode |
| `plan_cleanup` | policy? | `immediate` | Cleanup policy for plan mode |
| `git_timeout_secs` | int? | `300` | How many seconds a single git command totsuka runs may take. Past it, git is stopped and the command fails; during a dispatch the task is requeued automatically. `0` means no limit |

`cleanup` and `plan_cleanup` are both **defaults selected by mode**; a workflow that sets its own `cleanup` wins over them.

**Resolving the default.** With `location` omitted, `<state dir>` is `$XDG_STATE_HOME/totsuka`, falling back to `$HOME/.local/state/totsuka`. The default is built as an already-resolved path, so it never goes through `${ENV}` expansion. If you **do** set `location`, an unset `${ENV}` is an error rather than an empty string — and since worktrees are created at dispatch, it shows up as every task failing rather than as a startup failure. `doctor`'s `worktree-location` check finds it first.

Policy values are `"immediate"`, `"manual"`, `{ retention_days = 5 }`, `"keep_7d"`, and `"keep_28d"`. The `keep_*` forms are sugar for 7 and 28 days; other durations use the explicit form. A worktree with uncommitted changes is never deleted.

```toml
[worktree]
cleanup      = "keep_7d"              # implement: delete after 7 days
plan_cleanup = "immediate"            # plan: delete right away (the default)
# cleanup    = { retention_days = 3 } # any other number of days
```

**Choosing `git_timeout_secs`.** It is there to stop a git that is stuck — ssh waiting on a dead connection after the machine woke from sleep, for example — not to police a fetch that is slow but alive. If you use SSH remotes, keep it longer than `ServerAliveInterval` × `ServerAliveCountMax` in `~/.ssh/config`, so ssh drops a dead connection first with a clearer error (see the [operations guide](operations-guide.md#ssh-keepalive-recommended)). The limit only covers the git totsuka runs itself, not git that an agent runs inside its pane.

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
| `socket_path` | string? | built-in | Path of the receiving socket |
| `spool_dir` | string? | built-in | Where events are spooled when a post fails |
| `block_retry_limit` | int? | 3 | Consecutive stop-hook blocks before escalating |

### The hook bearer token is generated by `run`

You do not configure the bearer token that authenticates hook posts. `totsuka run` generates a random one on its first start, stores it in `$XDG_STATE_HOME/totsuka/hook-token` (mode 0600), and reuses it on every later start. `totsuka focus` and `totsuka doctor` read the same file.

- **Rotate**: delete the file and restart `totsuka run`
- **Check**: `totsuka doctor`'s `hook-token` (the file exists and is 0600 — missing before the first `run` is normal) and `hook-socket` (a self-post answers 200)

### Migrating: `[hooks].auth_token_ref` was removed

Up to 0.9, `[hooks].auth_token_ref` (and its override variable `TOTSUKA_HOOKS_AUTH_TOKEN_REF`) pointed at a token you created in Keychain or elsewhere. It was removed with no grace period, so a leftover line stops with this error (`run` exits 4 as a configuration error; `config validate` and `doctor` print the same words):

```text
[hooks].auth_token_ref was removed → delete this line; `totsuka run` generates the hook token itself ($XDG_STATE_HOME/totsuka/hook-token)
```

1. Delete the `auth_token_ref = …` line from `config.toml` (drop the `[hooks]` header too if nothing else is under it). Unset `TOTSUKA_HOOKS_AUTH_TOKEN_REF` if you export it
2. Restart `totsuka run`. Agents that were already running still hold the old token, so their hooks get 401; restart those tasks with `totsuka task retry`
3. Delete the secret you no longer need (it is not removed automatically). For the Keychain item `totsuka setup` created: `security delete-generic-password -s totsuka -a hook-token`. If you created it in 1Password or Bitwarden, delete that item

The variable injected into agents is still named `TOTSUKA_HOOK_TOKEN`.

## `[github]`

This is one of the two polling task sources (the other is `[notion]`) — `poll_interval_secs` is the plugin's own fetch interval, set in its `[github]` table. (The Slack source next door is event-driven and ignores it.)

```toml
[plugins.github]
enabled = true
kind = "task_source"

[github]
poll_interval_secs = 60   # 60 is also the default; 0 warns and falls back to it
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `token` | string | required | API token, sent as a bearer token and nothing else. See the permissions below. `cmd:gh auth token` works |
| `status_field` | string | `Status` | Name of the single-select field holding the status column. **Shared by every board** |
| `github_login` | string | required | Your own login, used to detect self-assigned tasks and as the claim target (the login totsuka self-assigns when it takes a task). **One login = one instance**: running several totsuka instances under the same login is unsupported — the claim arbitration cannot tell them apart |
| `in_progress_statuses` | string[] | `[]` | Status names treated as in progress and therefore skipped. **Shared by every board** |
| `source_name` | string | `github` | The source name stamped on each task. Adding boards does not change it — a task's `source` identifies the plugin and says nothing about which board it came from. Workflows name boards individually, with `projects` |
| `api_url` | string | `https://api.github.com/graphql` | GraphQL endpoint, for GitHub Enterprise or testing |
| `claim_verify_delay_ms` | int? | `750` | Milliseconds to wait between writing the claim (self-assign) and reading it back. The read-back is what detects both a race with a teammate and a silently ignored assignment, so it must not run before the API shows the write. `0` is honoured (a too-early read only costs one retry) |
| `max_retries` | int | 3 | Retries for retryable API failures. **One call may sleep 90s in total**; if the next wait would exceed that, the call returns the real cause instead of retrying, so a long `retry-after` cannot look like a hang. Raising `max_retries` therefore does not raise the total wait |
| `[github.prompts]` | table | — | Overrides for the prompts this plugin sends |

**The boards are not in this table.** They are `[[projects]]` entries with `source = "github"`, and the repositories that use them say so with `[[repositories]].project`:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | totsuka's key: what `[[repositories]].project` points at |
| `source` | string | required | totsuka's key: `"github"` |
| `owner` | string | required | Login of the project owner, a user or an organization |
| `owner_type` | `user` \| `organization` | `user` | Whether `owner` is a user or an organization. **Set per entry**, so user-owned and org-owned boards can be mixed |
| `project_number` | int | required | The ProjectsV2 number under `owner`. The positive-number check does **not** run at startup — see below |
| `triage_status` | string? | none | Status option to put a triage-filed item into. **Unset = the item is added with no Status.** A status-less item matches no `status` trigger condition, so **if every workflow on this source filters by status** nothing picks it up until you triage it on the board (a trigger without a status condition matches it regardless — the gate is only as real as your triggers). **Setting this to a value one of your workflow triggers polls (e.g. `Todo`) removes that gate** — filing then flows straight into an unattended run. Fine when intended; the default keeps it from happening by accident |

```toml
[github]
token = "cmd:gh auth token"
github_login = "your-login"
status_field = "Status"
in_progress_statuses = ["In Progress"]

[[projects]]
name = "my-board"
source = "github"
owner = "your-login"
project_number = 7

[[projects]]
name = "web-board"
source = "github"
owner = "my-org"
owner_type = "organization"
project_number = 3
triage_status = "📥 Inbox"   # optional: put triage-filed items into this column

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/your-login/totsuka"
project = "my-board"

[[repositories]]
name = "web-app"
path = "~/Workspace/github/my-org/web-app"
project = "web-board"
```

**Mind the TOML ordering.** A key written *after* a `[[projects]]` block lands **inside that block** — an array-of-tables runs until the next heading. Put the `[github]` keys under the `[github]` heading and the boards after them, or startup fails with an unknown key inside a project entry.

### What the binding does

It is both:

1. **The intake filter** — an issue on that board, but in a repository not bound to it, is skipped.
2. **The repository → board mapping** — this is what lets a triage task started from Slack know which board to file into.

Two jobs, one place to write it. **A repository files into exactly one board**, because `project` is a single value; there is no way to write the ambiguity that used to need checking. The same holds across sources — a repository cannot be claimed by both the GitHub and the Notion plugin.

### A wrong `project_number` does not fail at startup

A `project_number` of zero or a negative number **starts fine**. The check that requires a positive number lives only in config validation, not in startup: startup succeeds as soon as the config deserializes.

The symptom is instead that every poll fails to find the project and **no task is ever ingested**, with a clean startup log. That is the hardest failure to diagnose here. Only `totsuka doctor` and `totsuka config validate` catch it, so run one of them after editing this file.

An unknown key is the opposite: it is a hard startup failure, because that check happens during deserialization.

### Permissions the token needs

Every call is a POST to `https://api.github.com/graphql`, and there are only four: fetching project items, resolving project/field/item ids, `updateProjectV2ItemFieldValue`, and `viewer`. No REST, no Contents API, and **nothing is written to an issue** — the agent writes the deliverable itself.

**Pick the token type first. Getting this wrong is not something the permission tables below can fix:**

| Who owns the project | Token type that works |
|---|---|
| An **organization** | A fine-grained PAT (Projects under Organization permissions), or a classic PAT |
| A **user** | **A scope-based token** — a classic PAT with `project`, or the OAuth token `gh auth token` returns (which carries the same scope). Fine-grained PATs have no Projects permission under Account permissions, so they cannot reach ProjectsV2 here. What matters is the scope, not the label on the token |

For a fine-grained PAT (org-owned boards only):

| Kind | Permission |
|---|---|
| Repository | **Metadata: Read** (required) |
| Repository | **Issues: Read** (write is not needed) |
| Organization | **Projects: Read and write** |

**Contents is not needed.** For a classic PAT: `project`, plus `repo` (if private repositories are involved) or `public_repo`. A private organization's board may also need `read:org`.

**Both tables above are derived from what the code calls, not measured.** No token matching either one has been tried. What *has* been run against a real user-owned project is a single scope-based OAuth token carrying `gist, project, read:org, repo, workflow` — a superset of the classic list — and it passed all four operations. So the classic route is known to work with at least that much; the fine-grained table has not been exercised at all. Treat both as an upper bound, and cut them down for your own setup if you want the tightest token.

**Opening the pull request is not this token's job.** In an `implement` workflow the agent runs `gh pr create` itself, using your own `gh` authentication from the pane's environment. `gh auth login` is a separate prerequisite.

### `[github.prompts]`

Built-in defaults are embedded in the binary; this table overrides them one key at a time, and the key names are the config keys.

| Key | Used when |
|---|---|
| `triage_instructions` | The workflow's profile is `triage` |
| `design_instructions` | The workflow's profile is `design` |
| `implement_instructions` | The workflow's profile is `implement` |
| `design_pr_instructions` | The profile is `design` and the task is a pull request. Sent **instead of** `design_instructions` |
| `implement_pr_instructions` | The same for `implement`. The issue text ends in "open a pull request", which on a pull request's own branch opens a second one |

The pull request texts take `{pr_number}` and `{repo}`. **Three things must survive an override:**

- State that it is a pull request, as a fact.
- Name the commands that read it (`gh pr view {pr_number} --comments` / `gh pr diff {pr_number}`). The task body is the pull request's description and nothing else — neither the diff nor the review comments.
- Make the agent stop, changing nothing, if the pull request is not OPEN.

**Say nothing about the branch.** Whether the worktree is on the pull request's branch or detached at its head is decided by totsuka from the profile, and totsuka tells the agent itself.

### Pull requests on the board become tasks too

A workflow with `profile = "design"` or `"implement"` takes pull request cards **with no configuration change**. The trigger (`status` / `label` / `assignee`) and the `[[repositories]].project` ingest filter apply exactly as they do to issues. A pull request has three further conditions.

| Condition | Why |
|---|---|
| The workflow's profile is `design` or `implement` | The only two with a text for a pull request. A workflow that cannot take one passes silently, so another workflow on the same board can |
| It is OPEN (a draft is fine) | Commits pushed to a merged or closed pull request's branch go nowhere |
| It does not come from a fork | Its head branch is not on `origin`, and a branch of the same name may be there meaning something else (a fork's head is often `main`) |

A pull request that fails either of the last two is logged as a warning, once.

A pull request's task is **separate** from the task of the issue it came from. Under `implement` the worktree is created on the pull request's branch; under `design` it is detached at that branch's head commit.

The deliverable is a comment on the pull request either way.

| profile | What the agent does |
|---|---|
| `design` | Comments a **design for the additional changes** the pull request needs before it can merge. Not a code review |
| `implement` | Commits and pushes the additional changes, then comments **what it changed and why**. It reports that comment's URL |

When the task cannot start, it fails with the reason. It never falls back to the default branch: starting anywhere else opens a second pull request for the same change.

| Error message | What to do |
|---|---|
| `the hinted branch … is not on origin` | The pull request may have been merged or closed. Check the card, then retry or cancel |
| `the local branch … has diverged from origin/…` | You have a local branch of that name and it has diverged from `origin` (it may have been force-pushed). Delete it or reconcile it, then retry |
| `the hinted branch … is checked out in another worktree at …` | Another worktree is using the branch. If it belongs to another task, move that task's card back to its trigger column and continue it there. Otherwise remove that worktree and retry |
| `… is recorded as this task's worktree but is not one of the repository's worktrees any more` | The recorded worktree path is no longer a git worktree. Remove that directory and retry; the worktree is re-created |
| `could not move the worktree at … to the hinted branch` | The surviving worktree has uncommitted changes, or commits no branch reaches. Save them (commit, stash, or put a branch on them), then retry |

**For a pull request totsuka itself opened from an issue, move the issue's card back to the trigger column instead.** The same task resumes on the same branch in the same agent session. Putting that pull request on the board makes a separate task, which fails with the third error above for as long as the issue's worktree is kept (`keep_7d` and the like) and still holds the branch. A pull request card earns its keep for pull requests that did not come from a totsuka task: a dependency bot's update, or one a person opened.

**After pushing to a dependency bot's branch.** Renovate stops updating a branch once someone else has committed to it. Using the rebase label or checkbox afterwards makes Renovate rebuild the branch from its own commit, which discards the agent's. The comment `implement` leaves is the record of what was lost. See the [Renovate documentation](https://docs.renovatebot.com/updating-rebasing/).

**Cleanup at the end of the task deletes the pull request's local branch too.** The condition is the same as for any other task: only when every commit is reachable from `origin`. No commit is lost, but a local branch of the same name that you once made with `gh pr checkout` and left behind goes with it. (If it is checked out, the task does not start at all: the third error above.)

## `[notion]`

The other polling task source. `poll_interval_secs` is the fetch cadence.

```toml
[plugins.notion]
enabled = true
kind = "task_source"

[notion]
token = "cmd:ntn auth token --plain"
notion_user_id = "8f2c…"                 # you (omit and self-detection is off)
property_map = { title = "Name", status = "Status", assignee = "Owner" }
in_progress_statuses = ["In progress"]
```

Unknown keys here are a hard startup failure, so a typo shows up immediately.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `token` | string | required | Notion token, sent as a bearer token and nothing else. **Either an integration secret or the official CLI’s login token** — `cmd:ntn auth token --plain`. Measured: it is accepted by `GET /v1/users/me` with `Notion-Version: 2022-06-28`, the same call the plugin uses to validate. `ntn` keeps its credentials in the macOS Keychain (service `notion-cli`, account = workspace id), or in `~/.config/notion/auth.json` when `NOTION_KEYRING=0`; **prefer the command over reading either directly**, since the storage layout is `ntn`’s own business and may change. Three caveats when the token comes from the CLI. **(1) The output follows `ntn`’s default workspace** — log in to another workspace and the token changes underneath you without a single edit to `config.toml`, leaving `database_id` unreachable. Pin it with `cmd:NOTION_WORKSPACE_ID=<workspace id> ntn auth token --plain`. **(2) `cmd:` is resolved once, at startup**, so if the CLI login session expires, polling keeps returning 401 until `totsuka run` is restarted. If you need a value that does not expire, put an integration secret in `op://` instead. **(3) 401 and 404 are different questions** — 401 `unauthorized` means the token itself was rejected, while a page that is not shared (or an id that is wrong) comes back as 404 `object_not_found`. Notion deliberately does not distinguish "does not exist" from "not shared", so on a 404 what to check is the id, and whether that page is visible to the workspace you logged into with the CLI or to the integration. Both messages spell out the next action for each kind of token. |
| `notion_user_id` | string? | none | Your own Notion user id — what `trigger.assignee`'s `@me` is compared against. **Omit it and `@me` matches nobody**: the default trigger becomes "only unassigned tasks", and a workflow that spells out `@me` fails to start. Note the asymmetry with GitHub, where `github_login` is required. **And with `property_map.assignee` unset, even "only unassigned" does not hold** — there is nowhere to read assignees from, so every page looks unassigned and **the assignee filter disappears: the whole database is ingested**. Nothing warns, because the default condition was never written down. If your database divides work by assignee, map that property. |
| `property_map` | table | below | Which Notion property holds which field. |
| `body_source` | enum | `none` | Where a task's body comes from: `none`, `property` (the `rich_text` named by `property_map.body`), or `page` (the page's blocks, converted to Markdown). |
| `in_progress_statuses` | string[] | `[]` | Status options that mean "already running" and are skipped on ingest. Shared across every database. |
| `priority_map` | table | `{}` | Priority option name to a number; higher runs first. A `number` priority property is used directly and ignores this. |
| `source_name` | string | `notion` | The source name stamped on each task. |
| `api_url` | string | `https://api.notion.com/v1` | REST base URL. |
| `api_version` | string | `2022-06-28` | The `Notion-Version` header. |
| `max_retries` | int | 3 | Retries for retryable API failures. |
| `poll_interval_secs` | int? | 60 | Fetch cadence. **`0` does not stop polling**: it would busy-spin, so it logs one warning and falls back to the 60-second default. To stop polling, remove the workflow or set `[plugins.notion] enabled = false`. GitHub behaves the same way. Note that `[[workflows]].timeout_secs` reads `0` the opposite way, as an opt-out. |
| `rate_limit_rps` | int | 3 | Client-side requests per second. Notion's published limit is about 3 rps. |
| `[prompts]` | table | — | Overrides for the instruction text this plugin sends (below). |
| `[dynamic.{name}]` | table | `{}` | A named lookup referenceable as `@{name}` inside `trigger.filter` (below). |

### `[dynamic.{name}]` — dynamic values in `trigger.filter`

**A Notion query filter can only read properties of the database being queried.** A property on the other side of a relation cannot be a condition, so "the related sprint's status is `Current`" is unwritable — the only expressible form is that sprint's page id, and the id changes every sprint.

`[notion.dynamic.{name}]` holds the **rule for looking that id up on every poll**, so the config states "the current sprint" instead of this fortnight's answer.

```toml
[notion.dynamic.current_sprint]
database_id = "..."                                             # the sprint list database
filter = { property = "Sprint status", status = { equals = "Current" } }

[[workflows]]
name = "notion-implement"
projects = ["design-db"]
agent = "herdr"
profile = "implement"
trigger = { status = "Not started", assignee = "@me", filter = { and = [
  { property = "Type",   multi_select = { contains = "AI" } },
  { property = "Sprint", relation = { contains = "@current_sprint" } },
] } }
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `database_id` | string | required | The database to look in. It **need not be one of your `[[projects]]` databases** — a sprint list usually is not — but the token must be able to read it. |
| `filter` | table | required | A Notion filter selecting **exactly one** page. Like `trigger.filter`, it is passed to the query **untouched**, so totsuka never needs to know the property's type (`status` / `select` / `date` ...). |

**Zero matches and two-or-more matches are both errors, and the poll fails.** There is deliberately no fallback to "no condition": dropping the filter would ingest **the entire database** — the loudest possible failure wearing the face of success. Two-or-more does not take the first match for the same reason; failing beats silently picking an arbitrary one. Detecting this costs one query with `page_size: 2` (asking for a single page would make ambiguity undetectable).

**Names are limited to `[a-z0-9_]+`.** That keeps literal filter values that merely start with `@` (`@example.com`) out of the reference namespace, which is what makes the next rule safe:

**An `@{name}` that nothing declares fails at startup.** Passed through as a literal it would go to Notion verbatim, match nothing, and ingest zero tasks — exactly the silent failure this key exists to remove. The error lists the names that are configured.

Only the `trigger.filter` subtree is checked. Widening that to the whole trigger table would read `assignee = "@me"` as an undeclared reference, so the two namespaces are separated by scope: **`@{name}` may only appear inside `trigger.filter`**, never in `trigger.status`.

**Nothing is cached between polls.** A lookup is memoized within a single fetch only. The whole point of a lookup is that its answer changes, and a cache would need a staleness policy — but the config has no way to know when a sprint rolls over. The cost is one extra query per referenced name per workflow per poll, which is small against the 60-second default and `rate_limit_rps = 3`.

It lives under `[notion]`, so **a lookup is shared by every database**. Per-database lookups cannot be expressed.

**Adding a derived column on the Notion side also works, and is cheaper when you can.** A rollup or formula on the task database that pulls the sprint's status can be filtered directly, with no totsuka configuration at all. This key exists for the case where the board's schema is not yours to change.

### `property_map`

**Only `title` is required**; an optional field you leave unset is simply not extracted, which is how one plugin handles any database layout.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `title` | string | `Name` | The property holding the title (Notion's own default is `Name`). |
| `status` | string? | none | The status property. Both `trigger.status` and `on_*.status` read and write this one. |
| `status_kind` | enum | `status` | `status` (Notion's dedicated status type) or `select`. |
| `assignee` | string? | none | A `people` property holding assignees. **Required if you write `trigger.assignee`** — without it every page reads as unassigned, so the condition could not do anything, and startup fails rather than letting it look like it works. **Leaving it unset is not harmless even without that key**: the default condition also sees every page as unassigned, so the assignee filter disappears entirely (see `notion_user_id` above). |
| `priority` | string? | none | A `number` / `select` / `status` property holding priority. |
| `repo_hint` | string? | none | A `rich_text` / `select` / `status` / `multi_select` / `url` / `title` property naming a repository. Read in the order `rich_text` → `select`/`status` → `multi_select` → `url` → `title`; the first one that holds a value wins. A `multi_select` answers only when **exactly one** option is selected — zero and two-or-more both count as "no hint", and the task falls back to repository selection (`[llm]`, or pending when there is none). Two-or-more is deliberately not narrowed to the first option: the page really does name several repositories, and the agent must not run against an arbitrary one of them. A pending task is re-evaluated on every poll, so narrowing the choice on the board is picked up on the next tick. |
| `body` | string? | none | The `rich_text` property read when `body_source = "property"`. |

`property_map` is shared by every database, so `totsuka config validate` checks all of them: if one database is missing a mapped property, only the tasks from that database break — checking just the first one would be the quietest possible failure.

Databases go in `[[projects]]`, not here. An entry with `source = "notion"` belongs to this plugin:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | required | What `[[repositories]].project` points at. |
| `source` | string | required | `"notion"`. |
| `database_id` | string | required | The database to poll. |
| `triage_status` | string? | none | The status a triage-filed page is created with. **Leave it out and the page is created with no status**, which keeps a human triage gate. Setting it to a value some trigger polls removes that gate: filing then flows straight into an unattended run. It needs `property_map.status` to be mapped — `config validate` errors if it is not, but `run` does not check, so an unvalidated config starts and the status instruction is silently dropped. |

```toml
[[projects]]
name = "design-db"
source = "notion"
database_id = "…"

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "design-db"
```

### Notion tasks run at most once

**A Notion task runs once, whatever its trigger is.** Its deliveries carry no lane identity, so moving the status back does not re-deliver it — the repeat is discarded as a duplicate. GitHub records when the status cell changed and can therefore repeat a task when a card moves back into the column — **but only for a workflow whose trigger has a `status`**; a `label`-only or `assignee`-only trigger is at most once on GitHub too. The Notion API exposes no per-property timestamp, so the same approach is not available there at all.

**Adding a `status` to the trigger does not make a Notion task repeatable.**

### `[prompts]`

Per-key overrides of the instruction text this plugin sends; anything you leave out keeps the built-in wording.

| Key | Used when the workflow's profile is | Placeholders |
|---|---|---|
| `triage_instructions` | `triage` | `{page_url}` `{title}` |
| `design_instructions` | `design` | `{page_url}` `{title}` |
| `implement_instructions` | `implement` | `{page_url}` `{title}` |

## `[slack]`

The Slack source is event-driven — it pushes each event as it arrives — so `poll_interval_secs` is unused.

**Choose how events reach you before you create the Slack app.** `event_source` selects between a resident
socket connection and an HTTP Request URL answered by an event gateway, and Slack treats the two as mutually
exclusive per app — switching later means recreating the app from the other manifest. There is one manifest
per mode: `plugins/task-source-slack/manifest.yml` for the socket, `manifest.gateway.yml` for the gateway.

```toml
[plugins.slack]
enabled = true
kind = "task_source"
```

| Key | Type | Default | Meaning |
|---|---|---|---|
| `app_token` | string? | required with `socket` | App-level token (`xapp-`) for the socket connection. Use a secret reference. **Not needed with `event_source = "gateway"`** — no WebSocket is opened, so the token has no use. Leave it out, or leave it in (the `xapp-` shape is still checked). Startup fails if it is missing under `socket` |
| `user_token` | string | required | User OAuth token (`xoxp-`) for reading and writing as you. Use a secret reference |
| `bot_token` | string? | none | Bot token (`xoxb-`). With it set, a bot sends you a direct message when a draft or a picker arrives. **Without it the feature is simply off** (one warning at startup) — except with a channel watch trigger, where it is **required** and startup fails without it, because a watch result is posted under the bot's name. Invite the bot to the watched channel too, or posting fails with `not_in_channel` |
| `target_user_id` | string | required | Your Slack user id. Mentions of this user become tasks, and it is checked against the token's own identity |
| `watch_backfill_limit` | int? | 100 | For a channel watch: how many recent messages per channel totsuka re-reads at startup to recover posts made while it was down. Unused when no channel is watched. **`0` is rejected** |
| `watch_backfill_max_age_hours` | int? | 24 | How far back that recovery reaches. Without an age bound, **pointing a watch at a channel that already has history would turn its recent posts into tasks on the first start**; this caps that at a day. A restart of minutes or hours still recovers everything missed. **`0` is rejected** |
| `event_source` | `"socket"` \| `"gateway"` | `socket` | Where events arrive from. `socket` keeps a WebSocket open from the `totsuka run` process itself: nothing to deploy, but **mentions that arrive while totsuka is stopped are lost**, and if delivery keeps failing Slack disables the subscription until you re-enable it by hand in the app settings. `gateway` points Slack at an HTTP Request URL answered by an event gateway, which publishes coordinates to a queue that totsuka drains while it is running; nothing is lost while totsuka is down. **Slack treats the two as mutually exclusive per app**, so switching needs a manifest change and re-setup — which is also why **there is no automatic fallback from `gateway` to `socket`**: falling back is not something this process can do. When the gateway is unreachable it backs off and warns |
| `[slack.gateway]` | table | none | Where the queues live, with `event_source = "gateway"`. `project` / `subscription` (messages and reactions) / `block_actions_subscription` (button presses) / `pubsub_url` (default `https://pubsub.googleapis.com`) / `pull_max_messages` (default 50). **The two subscriptions exist because their retentions differ**; naming the same one twice is rejected, since presses would then sit under a retention measured in days and the two drain loops would compete for them. Authentication shells out to `gcloud auth application-default print-access-token`, so **no service-account key is distributed** — each operator pulls with their own Google identity and is granted their own subscription and nothing else |
| `drain_max_age_hours` | int? | 24 | Under `gateway`, how old a queued event may be and still become a task. The queue holds 7 days, so a long absence loses nothing — but filing a week of mentions the moment you come back is not what anyone wants. **The window lives here rather than in the queue's retention, so widening it after a trip is a local edit and needs no redeploy.** Anything older is acknowledged and dropped. **`0` is rejected** — "drop everything" should not be spelled as a silent zero. Unread under `socket` |
| `drain_limit` | int? | 100 | Under `gateway`, how many queued events become tasks per drain pass. **`0` is rejected.** Unread under `socket` |
| `watch_poll_interval_secs` | int? | 60 | Under `gateway`, seconds between polls of watched channels. **Channel watching cannot ride the gateway**: the gateway only forwards things that name you, and an ordinary post in a watched channel does not. Giving the gateway a copy of the watch list would split the setting across two places, and when the two drift the symptom is that watching silently stops working — so totsuka polls the channel history instead. **Only the watch path gets slower**; mentions, reactions and approval buttons still come off the queue. That is not a one-to-two-second guarantee either, though: the queue read is specified as *allowed* to wait for a message, not required to, and the reader backs off when it keeps answering empty. **`0` is rejected.** Unread under `socket`, where Slack pushes these posts |
| `thread_context_limit` | int | 6 | How many recent thread messages to include in the task body |
| `reply_style` | string? | none | Tone instructions injected into the task body |
| `[slack.prompts]` | table | — | Overrides for the prompts this plugin sends |
| `source_name` | string | `slack` | The source name stamped on each task |
| `[[slack.repos]]` | array | none | Candidate repositories: `name` (must match one in `config.toml`), optional `summary` and `path`. **Omit it and the repositories from `config.toml` are used**, which is usually what you want |
| `[[slack.channel_groups]]` | array | none | Narrow the candidates by channel name prefix; first match in definition order. `prefix` plus `repos`. Matching is **prefix-only** — `*` is a literal character, and there is no glob or regex. `prefix` takes **either a string or a list of strings** (`prefix = "dev-"`, `prefix = ["dev-", "team-"]`); a list lets several prefixes share one `repos`, so you do not copy the same candidate list once per prefix — and a copy that goes stale in one place is a silent mis-routing rather than a config error. Any entry in the list matching is a hit, and **the outer first-match-by-declaration-order is unchanged**. **A blank string is rejected either way** — as `prefix = ""` or as an entry in `prefix = ["dev-", ""]` — because a blank matches every channel, which would quietly turn its group into the catch-all. An empty list is rejected too (it names nothing). Every blank in a list is reported at once, so you fix them in one pass rather than one restart each. So **a catch-all for every channel does not belong here**; that is what `fallback_repo` is for |
| `fallback_repo` | string? | none | Where a mention goes when no `[[slack.channel_groups]]` rule matches its channel (a name from `[[slack.repos]]`). Being the only candidate it **resolves without calling the classifier**, so this is how you give org-wide questions — the ones that belong to no single code repository — one deliberate destination. **Omit it and every repository stays a candidate** and goes to the classifier, which gets less accurate and more expensive the more candidates there are. A matching rule that narrows to nothing (an empty `repos`, or only names that do not exist) is treated as no match and lands here too. **It does not let you omit `[slack.llm]`**: that requirement is a plain count of the declared candidates and reads neither this key nor `[[slack.channel_groups]]`, so it applies even to a setup where nothing could reach the classifier (every rule naming one repository, plus this key set). **A name that matches no repository fails startup** with a config error, the same way a `[[slack.channel_groups]]` entry referencing an unknown repository does. So does an empty string, rather than silently ignoring a fallback you meant to set |
| `[slack.llm]` | table | none | The classifier: `api` (`"chat"` by default, or `"decisions"`), `base_url` (chat), `endpoint` (decisions; OpenRouter's Decisions API when omitted), `model`, `api_key`, and `confidence_threshold` (default 0.6; below it — or when a decisions model answers "none fits" — you get a picker). **Omit it and `config.toml`'s `[llm]` is the default**, provided it has a key. With two or more candidates and neither source of settings, startup fails |
| `api_url` | string | `https://slack.com/api` | Web API base URL, for testing |
| `max_retries` | int | 3 | Retries for retryable API failures. **One call may sleep 90s in total**; if the next wait would exceed that, the call returns the real cause instead of retrying, so a long `retry-after` cannot look like a hang. Raising `max_retries` therefore does not raise the total wait |

### `[slack.prompts]`

Per-key overrides for the prompts this plugin sends; the key name is the setting name.

| Key | Used for | Placeholders |
|---|---|---|
| `reply_instructions` | Drafting a reply. The default for `answer`, and the fallback when the kind is unknown. This key is also the fallback for any kind this plugin has no instructions for, and those workflows have different tool boundaries — `answer` has no file edits and no shell, `design` has both, and a workflow with no profile has no restrictions at all. **So it must not claim what the agent can run**, only what the task is for. Ask here for a change, a commit or a pull request and an `answer` agent will try, be refused, and report a failure with no reply at all | — |
| `implement_instructions` | The default for `implement`: implement, open a pull request, report the URL | — |
| `triage_instructions` | The default for `triage`: file an issue, report the URL | — |
| `reply_style_suffix` | Appended to the reply instructions only when `reply_style` is set | `{style}` |
| `body_template` | The task body shown in the pane. For a mention-driven task, `{text}` is the original message with **your own (`target_user_id`) mention tag removed**: when the agent sees the raw `<@U…>` tag, it copies it into a reply that is posted as you. A channel-watch task is answered as the bot, so a mention of you stays there as content | `{sender}`, `{channel}`, `{text}` |
| `body_attachment_header` | Heading of the attachment section, emitted only when the message carried files. **The default text says two things, and both are behaviour**: (1) The attachment's content was not handed over — the plugin has no `files:read` scope and downloads nothing; without this the agent guesses what the attachment said. Refer only to what the prompt itself shows: the agent reads instructions and a body, so neither the orchestrator nor the notion of a "task" is visible from where it stands. (2) Where a link is shown, the agent may fetch the file from it — an agent with its own Slack tooling reads the file id out of the permalink and retrieves the content. The permission is conditional because Slack does not always supply a permalink, and promising one that is not there points the agent at a handle it cannot find. Keep **both** if you override the key. The default deliberately names no tool: naming one sends an agent that lacks it looking for something that is not there. **The heading and the parenthetical are in different languages on purpose**: the heading is a label a human reads in the pane, the parenthetical addresses the agent and so follows the same English-for-instructions rule as the rest of the file | `{count}` |
| `body_attachment_line` | One attachment line. `{file}` arrives **already composed** from the name, MIME type, size and permalink — each of those but the name can be missing, so four separate placeholders would leave an empty `（・）` behind | `{file}` |
| `body_thread_permalink` | **The parent thread's permalink**, emitted only when the mention is a reply inside a thread. A top-level mention is the thread root, so its own link is already the task's URL and the section is omitted rather than repeating it. It is also omitted when a reaction-driven task is attached to a single reply rather than the thread root: there the thread context is deliberately withheld — the agent was pointed at one message, not the conversation — and handing over the entrance would give back what withholding the transcript decided not to give. The thread context and this key are one statement about the conversation and share their scope rule. **The parenthetical is the point of the key**: the task's URL names the mention itself, so an agent mentioned partway down a thread has no way back to where the conversation started. The thread context in the body is a *window* — the most recent `thread_context_limit` messages — not a handle on what falls outside it. The default text says both of those out loud; drop it and the agent answers from a truncated conversation as if it were the whole one. It says "any thread context below" rather than naming it outright, because the section underneath is not always a transcript — when the context could not be fetched it is replaced by `body_thread_unavailable`, and there the link is worth more, not less. Like `body_attachment_header`, **the heading is in your language and the parenthetical is in English**, because the heading is a label a human reads in the pane and the parenthetical addresses the agent | `{url}` |
| `body_thread_header` | Heading of the thread context section | `{count}` |
| `body_thread_line` | One line of thread context | `{line}` |
| `body_thread_unavailable` | Replaces the whole section when the context could not be fetched | — |
| `classifier_system` | System prompt for repository classification | `{repo_names}` |
| `classifier_user` | The matching user message | `{mention_text}`, `{thread_context}`, `{catalog}` |
| `classifier_correction` | The retry turn when the response was not valid JSON | — |

Things to know:

- **`{text}` arrives already quoted.** The rewrite happens before expansion, so a template that drops the leading `> ` does not break continuation lines, and one that keeps it does not double-quote
- **`{text}`, `{thread_context}`, and `{catalog}` contain content chosen by whoever posted in Slack.** Expansion is single-pass, so a mention containing the literal text `{catalog}` is inserted as that string and does not splice in the candidate list
- Unknown placeholders pass through and are logged as a warning at startup. **This is deliberately not an error**: the symptom is a visible `{token}` in the draft. The core's `rubric` fails hard instead, because it is the judging condition and a broken one only makes verification looser
- These are **LLM prompts only**. A bad override degrades classification or draft quality; it cannot break completion detection. That difference in blast radius is why this table stayed while the core's prompt overrides were removed

## `[herdr]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `socket_path` | string? | none | Explicit socket path; highest precedence |
| `session` | string? | none | Named session, resolved to a path under your herdr config. Used when `socket_path` is unset |
| `[layout]` | table | see below | Pane layout for dispatched work |
| `[kind_map]` | table | `{}` | Maps an executable name to herdr's own vocabulary |
| `[identity]` | table | `{ enabled = true }` | Whether dispatch reports the repository and task to herdr |
| `request_timeout_secs` | int | 30 | Timeout for a single socket call |

Socket resolution order: `socket_path`, then `session`, then `HERDR_SOCKET_PATH`, then `HERDR_SESSION`, then the default path.

**herdr 0.7.5 or newer is required.** Against anything older, initialization is refused and `config validate` and `doctor` name the version. The check reads herdr's own version, and **there is no upper bound** — a newer herdr is never refused.

### `[discord]`

```toml
[discord]
bot_token = "op://Dev/Discord/bot_token"   # required
operator_user_id = "111111111111111111"    # required: your own Discord user ID
```

Every `[discord]` key (unknown keys are an error; setup steps are in [Discord setup](discord-setup.md)):

| Key | Type | Default | Meaning |
|---|---|---|---|
| `bot_token` | string | required | Bot token (Developer Portal → Bot). **There is no user-token option** — Discord forbids automating a normal account, so an app can only post under its bot |
| `operator_user_id` | string | required | Your own Discord user ID. **This is what decides whose posts start a task**; by default only this ID's do. It is **all digits**, copied with "Copy User ID" in developer mode. A username here is rejected at startup |
| `api_url` | string | `https://discord.com/api/v10` | REST base URL |
| `source_name` | string | `discord` | This source's name (it appears as the task's `source`) |
| `max_retries` | int | 3 | Retry attempts for retryable failures |
| `watch_backfill_limit` | int? | 100 | How many messages per channel are re-read at startup. **Discord's own maximum is 100**, so a larger value is rejected at startup. `0` is rejected too |
| `watch_backfill_max_age_hours` | int? | 24 | How far back that re-read reaches. `0` is rejected |

The trigger is `trigger = { channel = "…", channel_name = "…", repo = "…" }` (plus an optional `from`). **This source has no other kind of trigger**, so a configuration where no workflow watches a channel fails at startup.

## `[herdr.identity]`

```toml
[herdr.identity]
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

### `[herdr.kind_map]`

herdr picks the executable from its own fixed vocabulary, so the plugin translates a **file name** into it. `claude`, `codex`, and `opencode` pass through, so you usually need nothing here. You need it for a wrapper script under a name herdr does not know:

```toml
[herdr.kind_map]
my-claude = "claude"
```

- The key is matched against the **file name**, not the path, so `/opt/bin/my-claude` is looked up as `my-claude`
- Values are not validated. herdr rejects an unknown one itself; duplicating its vocabulary here would silently drift when herdr adds to it
- This does not belong in the `[tools]` registry, which is shared across agents — herdr-specific vocabulary there would leak into setups that never use herdr

### `[herdr.layout]`

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
projects = ["tomo-prj"]
trigger = { status = "Ready for design" }
profile = "design"          # resolves mode, output and verification
agent = "herdr"
on_success = { status = "Ready for design review" }

[[workflows]]
name = "implement"
projects = ["tomo-prj"]
trigger = { status = "Ready to implement" }
profile = "implement"
agent = "herdr"
on_success = { status = "Ready for review" }
```

---

This page is generated from the internal document `ai-docs/development/config-reference.md`, which carries the design decisions and measurements behind it.
