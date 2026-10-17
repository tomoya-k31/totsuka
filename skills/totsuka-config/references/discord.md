# Discord (`discord`)

Work in this order, with `totsuka config validate --offline` after each write: outside totsuka, `[discord]`, the project, workflows. Source-independent workflow rules are in [workflows.md](workflows.md).

## Outside totsuka

The full guide is [Discord setup](https://github.com/tomoya-k31/totsuka/blob/main/docs/discord-setup.md) §1–3 (online only; this skill does not ship it). In short:

- A server and a channel.
- An application with a bot. Turn on **MESSAGE CONTENT INTENT**, or the bot stops with close code 4014.
- The bot invited to the server.
- With Developer Mode on, copy the user's ID and the channel's ID.

## `[discord]`

`{"bot_token": "secret:discord.bot_token", "operator_user_id": "<numeric user ID>"}`.

## Project

`{"name", "source": "discord"}` only, one entry. No repository links to it.

## Workflows

Discord's only trigger is watching a channel. At least one workflow is required, or totsuka refuses to start. Discord has no columns, so leave out `on_start` / `on_success` / `on_failure` (accepted and ignored).

Questions: which channel, which repository its posts are about, who besides the user may start a task, and what should come out (see the profiles in [workflows.md](workflows.md)).

```json
{"name": "discord-help", "projects": ["discord"], "agent": "herdr", "profile": "answer",
 "trigger": {"channel": "123456789012345678", "channel_name": "help", "repo": "web-app"}}
```

- `channel` is the numeric ID; a channel *name* there matches nothing, silently. `channel_name` is the name, `repo` a `[[repositories]]` name.
- `"from": ["<user ID>", ...]` lets others start tasks; by default only `operator_user_id` can.
- The channel gets a report only with output `source`: the default for `answer` and `triage`, and `"output": "source"` must be added for `design` and `implement`.
