# Slack (`slack`)

Work in this order, with `totsuka config validate --offline` after each write: outside totsuka, `[slack]`, the project, repository selection, workflows. Source-independent workflow rules (profiles, `timeout_secs`, `initial_prompt`) are in [workflows.md](workflows.md).

## Outside totsuka

The full guide is [Slack setup](https://github.com/tomoya-k31/totsuka/blob/main/docs/slack-setup.md) §0–1 (online only; this skill does not ship it). In short:

- Choose how events arrive first, since the two need different apps:
  - `socket` (Socket Mode): nothing to host, but mentions made while totsuka is stopped are lost.
  - `gateway`: a hosted relay on Google Cloud, set up from [Event gateway setup](https://github.com/tomoya-k31/totsuka/blob/main/docs/event-gateway-setup.md).
- Create the Slack app from the matching manifest and install it.
- Note the user's member ID (`U…`).
- Changing the app's scopes later reissues both tokens; the user then forgets and re-enters them in the app.

## `[slack]`

- `user_token: "secret:slack.user_token"` (the `xoxp-` token).
- `target_user_id`: the member ID.
- `event_source`: `"socket"` or `"gateway"`.
- With socket: `app_token: "secret:slack.app_token"` (the `xapp-` token).
- With gateway: the `[slack.gateway]` values printed by `tofu output totsuka_config`.
- `bot_token: "secret:slack.bot_token"`: recommended (without it there are no nudges) and required for channel watching.
- `mention_groups`: the user groups (`S…`) whose mentions should start tasks at all. Unset means every group the user belongs to; `[]` means personal mentions only. Ask when the user is in broad groups (all-staff, on-call announcements).
- `reply_style`: a tone for replies ("concise, polite").

## Project

`{"name", "source": "slack"}` only, one entry. No repository links to it.

## Choosing the repository

Each Slack task needs a repository, picked in this order:

1. `"repo": "<name>"` in a mention trigger (`{"mention": true, "repo": "web-app"}`) fixes it for that workflow; nothing else runs. `repo` is not allowed next to `reaction` (Start fails with "has `repo` in its trigger but no `channel`"): a reaction task takes the repository its thread already has, or goes through the steps below. A channel watch always names its `repo`.
2. `[[slack.channel_groups]]` (`{"prefix": "dev-", "repos": [...]}`) narrows the candidates by channel name, and `fallback_repo` catches the channels no group matches.
3. With two or more candidates left, an LLM picks.

Ask which repositories questions are about. With a single candidate, no repository is classified and nothing needs pinning.

## The LLM

Every mention is first asked whether it needs a reply at all: an FYI, an announcement, a bare thank-you, a "react when done" request or a bot's ask is dropped with a note in the app DM instead of becoming a task, and the user's reaction still turns it into one. So **a workflow answering mentions needs `[slack.llm]` or the top-level `[llm]`** — with a single repository too — as does a file listing more than one repository. Without it, the plugin refuses to start. Ask which API the user has, then write `set /llm '{"base_url": "<API base URL>", "model": "<model>", "api_key_ref": "secret:llm.api_key_ref"}'` (`model` is required; other keys are in the schema). If the user's team marks finished work with a specific emoji reaction, offer to name it in `[slack.prompts].reply_check_criteria`.

## Workflows

Slack has no columns: there is no `status` trigger and no re-run. **Do not write `on_start` / `on_success` / `on_failure` here.** Slack accepts them and silently does nothing, so they only mislead the next reader.

### Questions, in order

1. What should start work: a mention of the user, the user's own reaction with an emoji, or any post in a watched channel?
2. What should come out: a reply (`answer`), an issue filed on a board (`triage`), or code and a PR (`implement`)?
3. Should replies wait for the user's approval (`publish: "draft"`, the default) or post at once (`"direct"`)?

### Recipes

**Answer mentions** (the usual first workflow):

```json
{"name": "slack-answer", "projects": ["slack"], "agent": "herdr", "profile": "answer",
 "trigger": {"mention": true}, "timeout_secs": 1800}
```

**Implement on a reaction**, posting the PR URL back to the thread:

```json
{"name": "slack-implement", "projects": ["slack"], "agent": "herdr", "profile": "implement",
 "trigger": {"reaction": "hammer"}, "output": "source"}
```

Reacting on a thread's first message gives the agent the whole thread; on a reply, only that message. The repository is inherited from an earlier answer in the thread.

**File an issue on a reaction:**

```json
{"name": "slack-triage", "projects": ["slack"], "agent": "herdr", "profile": "triage",
 "trigger": {"reaction": "books"}, "timeout_secs": 1800}
```

The issue goes to the board (GitHub or Notion) that the chosen repository links to, into that project's `triage_status` column. Ask whether filed issues should wait for a person (leave `triage_status` unset or on a column no workflow triggers on) or start work at once (set it to a workflow's trigger column).

**Watch a channel** and answer every post there, as the bot:

```json
{"name": "help-desk", "projects": ["slack"], "agent": "herdr", "profile": "answer",
 "trigger": {"channel": "C0123ABCD", "channel_name": "help", "repo": "web-app", "from": ["U0456EFGH"]}}
```

Needs `bot_token` and the bot invited to the channel. Without `from`, only the user's own posts start a task.

### Trigger rules

- One workflow per kind: at most one plain `"mention": true`, and one workflow per emoji.
- `mention` cannot sit with `reaction` or `channel`; `from_bot` needs `reaction`; `channel` needs `channel_name` and `repo`. An empty trigger is rejected.
- `"to_group": ["S…"]` next to `"mention": true` sends mentions of that user group to this workflow instead of the plain one. The group must be in `mention_groups` when that is set.
- `"from_bot": ["B…"]` next to `reaction` lets the emoji work on those bots' posts. It takes the bot's `B…` ID, not its `U…` user ID or its name.
- Emoji names have no colons: `"hammer"`, `"eyes"`.
- The skill cannot look up `C…` / `S…` / `B…` IDs; ask the user for them and check the prefix.

### Profiles on Slack

`design` is not available: it runs with no instructions and posts nothing. `triage` and `implement` reply only with `"output": "source"` (`triage` has it by default).
