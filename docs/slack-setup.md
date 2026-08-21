> 🌐 **English** · [日本語](slack-setup.ja.md)

<!-- generated-from: ai-docs/operations/slack-quickstart.md sha256:d53d06728a766396bdd56340dcfcc8bc00d3392b76ddaa6d7a20574120780ebd -->

# Setting up the Slack source

About 15 minutes. At the end, mentions of you in Slack become totsuka tasks, and when you approve an agent's draft it is posted as a thread reply **under your own name**.

Everything that appears in a conversation is posted with your user token. The app's bot user exists only to send you a notification DM, because ephemeral messages and self-DMs generate no Slack notification of their own.

> **Read your workspace's rules first if this is a work account.** A user token acts as you: anything it posts is indistinguishable from you typing it. Some organizations restrict or prohibit user-token apps.

## 1. Create the Slack app from the manifest

1. Go to <https://api.slack.com/apps> → **Create New App** → **From a manifest**, and pick the workspace.
2. Paste [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) into the YAML tab and create the app.
3. **OAuth & Permissions → Install to Workspace**. Copy the **User OAuth Token** (`xoxp-…`) and the **Bot User OAuth Token** (`xoxb-…`) from that page.
4. **Basic Information → App-Level Tokens → Generate Token and Scopes**, with the `connections:write` scope. Copy the token (`xapp-…`).

Also copy your own member id (`U…`): your Slack profile → **⋯** → **Copy member ID**.

## 2. Store the tokens

totsuka never stores a secret value — the config holds a *reference*, and the value is fetched at run time.

With 1Password, create an item with three fields and note the references:

```text
op://Dev/Slack/user_token   ← xoxp-…
op://Dev/Slack/app_token    ← xapp-…
op://Dev/Slack/bot_token    ← xoxb-…  (only if you want the notification DM)
```

On macOS the Keychain works too; the references then look like `keychain:totsuka/slack-user`:

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'   # optional
```

## 3. Create the configuration

```bash
totsuka setup
```

Pick the **"Slack — reply as yourself"** recipe. It asks for your repositories, the member id from step 1, and the LLM used to decide which repository a mention is about. It writes `plugins/slack.toml`, installs and enables the plugin, and runs `doctor` — all from this one command. **It never asks for a token value.**

If you have not stored the tokens yet, `setup` prints a checklist of the exact commands to run.

**Even with every token stored, the `state-db` check still fails and `doctor` exits 3.** That only means the state database does not exist yet, and the only thing that creates it is `totsuka run`. It goes green after the first run.

### Writing the configuration by hand

`setup` **never overwrites an existing file**, so add Slack by hand if you already have a configuration, or if you want a shape the recipe does not express.

```bash
totsuka plugin install --bundled slack --enable
```

> From a source checkout, use `totsuka plugin install --from-source slack --enable` instead. Pointing at a directory such as `./plugins/task-source-slack` only works if you put a built binary there yourself.

In `~/.config/totsuka/config.toml`:

```toml
[plugins.slack]
enabled = true
kind = "task_source"
poll_interval_secs = 5   # how often the socket-mode buffer is drained

# Optional: reacting with :eyes: yourself turns a message into a task.
# Put it BEFORE the catch-all — `trigger = {}` matches everything, so a
# reaction workflow placed after it can never be reached.
# Someone else reacting does not start anything, and there is no setting
# that relaxes this. Names take or omit the colons; 👀 is `eyes` and
# 👁 is `eye`, which are different emoji.
[[workflows]]
name = "slack-reaction"
source = "slack"
trigger = { reaction = "eyes" }
mode = "plan"
agent = "herdr"
output = "source"

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
mode = "plan"            # drafting a reply needs no push or pull request
agent = "herdr"
output = "source"        # the result goes through the approval flow
```

In `~/.config/totsuka/plugins/slack.toml`:

```toml
app_token = "op://Dev/Slack/app_token"
user_token = "op://Dev/Slack/user_token"
bot_token = "op://Dev/Slack/bot_token"    # optional: the notification DM.
                                          # Omit it and there is simply no DM.
target_user_id = "U012AB3CD"              # your member id
reply_style = "Keep it short and polite"  # optional

# Candidate repositories come from `[[repositories]]` in config.toml, so you
# usually do not need `[[repos]]` here. Set it only to narrow the candidates
# or to override a summary:
# [[repos]]
# name = "web-app"
# summary = "The customer-facing web app"

# With two or more candidates a classifier LLM is required. If config.toml has
# an `[llm]` section with a key, it is supplied automatically. Set this only to
# use a different model or threshold for this plugin:
# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "…"
# api_key = "op://Dev/Openrouter/api_key"
```

Every key is described in the [configuration reference](config-reference.md).

## 4. Verify, then run

```sh
totsuka config validate   # offline checks
totsuka doctor            # checks the tokens against Slack, including that the
                          # user token's identity matches target_user_id
totsuka run --watch       # stays resident on the socket connection
```

To try it end to end, have someone mention you. After the agent finishes, a draft arrives as an ephemeral message in the thread and as a self-DM (plus a bot DM if you configured `bot_token`). **Approve** posts it as a thread reply under your name; **reject** discards it.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `doctor` reports `invalid_auth` or `token_revoked` | The token was revoked. Reissue it and update wherever you stored it |
| `doctor` reports an identity mismatch | The token belongs to someone else, or `target_user_id` is wrong. This is refused on purpose, to prevent posting as another person |
| Mentions do not become tasks | Check that the mention is `@you` (only channels you are in are visible), that `run --watch` is running, and that the message is a plain post — edits and bot posts are ignored |
| Reacting does not create a task | Check that a workflow has `trigger = { reaction = "…" }`, that it sits **before** the catch-all `trigger = {}`, that the emoji name matches (👀 is `eyes`, 👁 is `eye`; a custom emoji arrives under the name actually clicked, so list aliases too), that **you** were the one who reacted, and that the app was reinstalled with a manifest containing `reactions:read` — without that scope the event never arrives **and nothing reports an error** |
| Re-adding a reaction does not re-run it | Intended. A message that was handled successfully is not handled again, so removing and re-adding a reaction cannot start a second agent. A message whose fetch **failed** can be retried this way |
| The draft arrives but the buttons no longer work | They expire after 24 hours, or were evicted once more than 1024 drafts accumulated. Reply by hand from the self-DM copy, or mention again. Drafts survive a restart |
| You changed the app's scopes | A scope change requires reinstalling the app, which **reissues both `xoxp-` and `xoxb-`**. Update both stored values, then run `doctor`. Updating only one leaves the app half-broken |
| Channel-prefix rules never apply and you always get the picker | The app cannot read channel names. Reinstall with a manifest containing `channels:read` and `groups:read`, then update the stored tokens as above |
| No notification DM arrives | Check that `bot_token` is set and valid (`doctor` probes it), look for a warning about resolving the bot DM in the startup log, and check that you have not muted the app's DMs in Slack |

---

The full setup path for a new machine — including everything that is not Slack — is in the [setup playbook](setup-playbook.md).

Detailed design notes and the reasoning behind these steps live in `ai-docs/operations/slack-quickstart.md` in the repository.
