> 🌐 **English** · [日本語](slack-setup.ja.md)

<!-- generated-from: ai-docs/operations/slack-quickstart.md sha256:fc41354fbb26bfdc31522429d6cdb5d3c0ef3e24a65f339f0c3c06acb71d14bc -->

# Setting up the Slack source

About 15 minutes. At the end, mentions of you in Slack become totsuka tasks, and when you approve an agent's draft it is posted as a thread reply **under your own name**.

Everything that appears in a conversation is posted with your user token. The app's bot user exists only to send you a notification DM, because ephemeral messages and self-DMs generate no Slack notification of their own.

> **Read your workspace's rules first if this is a work account.** A user token acts as you: anything it posts is indistinguishable from you typing it. Some organizations restrict or prohibit user-token apps.

## 0. Choose how events reach you — before creating the app

**A Slack app cannot have both Socket Mode and a Request URL.** The two are
mutually exclusive per app, and switching later means **recreating the app from
the other manifest** (which reissues every token). So this is the first step,
not a later tuning decision.

| | **Socket Mode** (default) | **Event Gateway** |
|---|---|---|
| What you need | Nothing | One GCP project, about $1/month |
| While totsuka is stopped | **Mentions are lost, with no way to recover them** | They queue, and are picked up when you start |
| After a long stop | **Slack disables the app's event subscription** (any app failing more than 95% of deliveries over 60 minutes). Re-enabling is a manual step in the Slack settings, and nothing tells totsuka it happened | **totsuka being stopped is no longer a cause.** Delivery can still fail for other reasons — the gateway itself being down, a wrong URL, a failed publish — so this is not "always succeeds" |
| Setting | `event_source = "socket"` (the default; you can omit it) | `event_source = "gateway"` plus `[slack.gateway]` |
| Manifest | `manifest.yml` | `manifest.gateway.yml` |
| Channel-watch latency | Immediate | `watch_poll_interval_secs` (60s default). **Only watching is slower**; mentions, reactions and approval buttons stay within a second or two |

It comes down to **whether that machine stops**.

- **An always-on desktop: Socket Mode.** Nothing to set up, no added latency.
- **A laptop: the Event Gateway.** It stops overnight, at weekends and while you
  travel — and the cost is not only the mentions that arrive meanwhile but the
  subscription itself being switched off. The exemption for low-volume apps
  (under 1,000 events an hour) does not protect this setup: what is subscribed
  is every message in every channel you are in, which passes that easily on a
  weekday.

If you choose the gateway, the ordering is awkward, so here it is up front: the
hostname that goes into the Request URL is a result of building the GCP side,
and the signing secret that goes into building it is a result of the Slack app.
Split it like this:

1. **Do only steps 1–3 below first** — create the app, copy the tokens and the
   signing secret. Leave the `<gateway-host>` placeholder in
   `manifest.gateway.yml` alone.
2. Work through [Event Gateway setup](event-gateway-setup.md).
3. **Go back to the app** and put the Request URL it produced into both places.
4. Do step 2 (store the tokens) and step 3 (`totsuka setup`) on this page.
5. **Add `event_source` and `[slack.gateway]` to the `[slack]` table that
   `setup` wrote.**

**Step 5 cannot be folded into step 4.** `setup` leaves an existing `[slack]`
table alone, so writing one yourself first means `user_token` and
`target_user_id` never get added. Let `setup` write it and you get Socket
Mode's `app_token` instead, and the `doctor` run at the end of `setup` fails
asking for an app-level token. Neither "write it all up front" ordering works —
hence: let `setup` write the table, then add to it. A red `doctor` during
`setup` is expected; re-run `totsuka doctor` after step 5.

## 1. Create the Slack app from the manifest

1. Go to <https://api.slack.com/apps> → **Create New App** → **From a manifest**, and pick the workspace.
2. Paste **the manifest for the mode you chose** into the YAML tab and create the app.

   | Mode | Manifest |
   |---|---|
   | Socket Mode | [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) |
   | Event Gateway | [`plugins/task-source-slack/manifest.gateway.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.gateway.yml) — replace `<gateway-host>` and `<opaque-token>` with your own |

3. **OAuth & Permissions → Install to Workspace**. Copy the **User OAuth Token** (`xoxp-…`) and the **Bot User OAuth Token** (`xoxb-…`) from that page.
4. **This step differs by mode.**
   - **Socket Mode**: **Basic Information → App-Level Tokens → Generate Token and Scopes**, with the `connections:write` scope. Copy the token (`xapp-…`).
   - **Event Gateway**: no app-level token is needed — nothing opens a WebSocket. Copy the **Signing Secret** instead (Basic Information → App Credentials). **It does not go in `config.toml`**; it belongs to the gateway's secret store and never reaches this machine.

   **The gateway also needs a Request URL in two places.** Creating the app from the manifest fills both in, so this is a check rather than a step:

   | Slack app setting | Carries |
   |---|---|
   | Event Subscriptions → Request URL | mentions, reactions |
   | Interactivity & Shortcuts → Request URL | approval and repository-picker buttons |

   **With only the first, mentions keep working and no button ever arrives.** Socket Mode delivered both down one connection, so this is a distinction that did not exist before. Slack verifies the URL when you save it, so the gateway has to be running first.

Also copy your own member id (`U…`): your Slack profile → **⋯** → **Copy member ID**.

## 2. Store the tokens

totsuka never stores a secret value — the config holds a *reference*, and the value is fetched at run time.

**Match the references `setup` writes.** Choosing the 1Password backend makes `setup` write a fixed shape — vault `Dev`, item `totsuka` — so storing the tokens anywhere else leaves the configuration pointing at something that does not exist, and the plugin cannot start:

```text
op://Dev/totsuka/slack-user   ← xoxp-…
op://Dev/totsuka/slack-app    ← xapp-…  (Socket Mode only; the gateway needs none)
op://Dev/totsuka/slack-bot    ← xoxb-…  (only if you want the notification DM)
```

```sh
op item edit totsuka slack-user='xoxp-…'   # create the item first if it does not exist
op item edit totsuka slack-app='xapp-…'
op item edit totsuka slack-bot='xoxb-…'    # optional
```

**The vault name `Dev` is fixed too.** If your vault is called something else, edit the references in the `[slack]` table after `setup` generates it. (Writing the file by hand, below, lets you use any reference you like.)

On macOS the Keychain works too. Those references look like `keychain:totsuka/slack-user`, and they match what `setup` writes:

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'   # optional
```

## 3. Create the configuration

```bash
totsuka setup
```

Pick the **"Slack — reply as yourself"** recipe. It asks for your repositories, the member id from step 1, and the LLM used to decide which repository a mention is about. It writes the `[slack]` table, installs and enables the plugin, and runs `doctor` — all from this one command. **It never asks for a token value.**

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

# Optional: reacting with :eyes: yourself turns a message into a task.
# The plugin decides which workflow each event selects: a reaction picks
# the workflow with the matching emoji, a plain mention goes to the one
# workflow WITHOUT a `reaction` trigger — order in this file does not
# matter. Writing the same emoji on two workflows, or two workflows
# without a reaction, is rejected at startup (and by
# `totsuka config validate`).
# Someone else reacting does not start anything, and there is no setting
# that relaxes this. Names take or omit the colons; 👀 is `eyes` and
# 👁 is `eye`, which are different emoji.
[[projects]]
name = "slack"  # a source with one domain still declares it
source = "slack"

[[workflows]]
name = "slack-reaction"
projects = ["slack"]
trigger = { reaction = "eyes" }
mode = "plan"
agent = "herdr"
output = "source"

[[workflows]]
name = "slack-reply"
projects = ["slack"]
trigger = {}
mode = "plan"            # drafting a reply needs no push or pull request
agent = "herdr"
output = "source"        # the result goes through the approval flow
```

In `~/.config/totsuka/config.toml`:

```toml
[slack]
app_token = "op://Dev/totsuka/slack-app"
user_token = "op://Dev/totsuka/slack-user"
bot_token = "op://Dev/totsuka/slack-bot"  # optional: the notification DM.
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
totsuka run --watch
```

**What `doctor` checks differs by mode.**

| | Socket Mode | Event Gateway |
|---|---|---|
| The app-level token (`xapp-`) | Probed | **Not probed** — nothing opens a connection, so failing startup over an unused token would be wrong |
| The queues | — | One read of each queue at startup. A wrong Google identity, a missing permission or a mistyped name all fail the same way otherwise: **`doctor` green, and not one event ever arrives** |

With the gateway, run `gcloud auth application-default login` once on this
machine. totsuka reads **your own** queues with **your own** Google account;
no service-account key is handed out.

To try it end to end, have someone mention you. After the agent finishes, a draft arrives as an ephemeral message in the thread and as a self-DM (plus a bot DM if you configured `bot_token`). **Approve** posts it as a thread reply under your name; **reject** discards it.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `doctor` reports `invalid_auth` or `token_revoked` | The token was revoked. Reissue it and update wherever you stored it |
| `doctor` reports an identity mismatch | The token belongs to someone else, or `target_user_id` is wrong. This is refused on purpose, to prevent posting as another person |
| Mentions do not become tasks | Check that the mention is `@you` (only channels you are in are visible), that `run --watch` is running, and that the message is a plain post — edits and bot posts are ignored |
| Reacting does not create a task | Check that a workflow has `trigger = { reaction = "…" }` (**order in the file does not matter** — mentions and reactions arrive on separate event paths, so a reaction workflow written after the catch-all is not hidden by it), that the emoji name matches (👀 is `eyes`, 👁 is `eye`; a custom emoji arrives under the name actually clicked, so list aliases too), that **you** were the one who reacted, that the app was reinstalled with a manifest containing `reactions:read` — without that scope the event never arrives **and nothing reports an error** — and that the message was not **already handled as a mention**: both paths share one set of processed messages, so reacting to a message that already became a task does nothing |
| Re-adding a reaction does not re-run it | Intended. A message that was handled successfully is not handled again, so removing and re-adding a reaction cannot start a second agent. A message whose fetch **failed** can be retried this way |
| The draft arrives but the buttons no longer work | They expire after 24 hours, or were evicted once more than 1024 drafts accumulated. Reply by hand from the self-DM copy, or mention again. Drafts survive a restart |
| A group mention (`@team-name`) does not create a task | Check that the app was reinstalled with a manifest containing `usergroups:read`. **Without that scope the startup lookup of your groups fails, your group set stays empty, and no group mention becomes a task** — personal mentions keep working, so it looks like "only part of it is broken". totsuka logs one warning at startup; look there. Your groups are resolved **once, at startup**, so restart after being added to a group. `@here`, `@channel` and `@everyone` are **out of scope by design**: they name no one |
| **Gateway**: not a single mention arrives | Check that `gcloud auth application-default login` has been run (the startup check reports it), that Slack accepted the Request URL when you saved it (it verifies the URL on save, so saving fails if the gateway is not running), and that `[slack.gateway]` matches what the deployment produced |
| **Gateway**: mentions work but no approval button arrives | The **Interactivity & Shortcuts** Request URL is not set. It is a separate setting from Event Subscriptions, and easy to miss because Socket Mode delivered both down one connection |
| **Gateway**: nothing is filed after coming back | The events are older than `drain_max_age_hours` (24 by default). The queue holds 7 days, so raising the setting picks them up — but **raise it before the first start after the absence**. Events judged outside the window are acknowledged and discarded on the spot, so raising it afterwards does not bring back what was already dropped |
| **Gateway**: a watched channel reacts slowly | Expected. With the gateway, watching is a poll of the channel history (`watch_poll_interval_secs`, 60s default), while mentions, reactions and buttons keep coming off the queue — which is not a guaranteed number of seconds either, since the queue read is allowed but not required to wait and the reader backs off on empty answers. Only things that name you travel through the gateway, and an ordinary post in a watched channel does not |
| You tried to switch modes by editing the config | It does not work that way. Socket Mode and a Request URL are **mutually exclusive per Slack app**, so switching means recreating the app from the other manifest (reissuing every token). Changing `event_source` alone changes nothing on Slack's side |
| You changed the app's scopes | A scope change requires reinstalling the app, which **reissues both `xoxp-` and `xoxb-`**. Update both stored values, then run `doctor`. Updating only one leaves the app half-broken |
| Channel-prefix rules never apply, so every mention falls back to the classifier LLM (or to the picker, if no LLM is configured) | The app cannot read channel names. Reinstall with a manifest containing `channels:read` and `groups:read`, then update the stored tokens as above |
| No notification DM arrives | Check that `bot_token` is set and valid (`doctor` probes it), look for a warning about resolving the bot DM in the startup log, and check that you have not muted the app's DMs in Slack |

---

The full setup path for a new machine — including everything that is not Slack — is in the [setup playbook](setup-playbook.md).

Detailed design notes and the reasoning behind these steps live in `ai-docs/operations/slack-quickstart.md` in the repository.
