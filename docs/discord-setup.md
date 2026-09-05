> 🌐 **English** · [日本語](discord-setup.ja.md)

<!-- generated-from: ai-docs/operations/discord-quickstart.md sha256:623e032b56554be9e211ec6b91a9994e373d9ac8d2f8ffb2dac58a6af3f04132 -->

# Setting up the Discord source

About 10 minutes. When you are done, anything you post in one Discord channel becomes a totsuka task, and the result comes back as a reply in that post's thread.

> **Use a dedicated Discord server.** The permission totsuka needs lets it read the text of every channel the bot can see, and Discord offers no way to narrow that per channel. In a server you actually chat in, one wrong line of configuration reaches much further.

Posts go out under the bot's name: Discord forbids automating a normal user account, so there is no way for an app to post as you.

## 1. A server and a channel

Create a dedicated server and one channel to watch (`clip`, say).

## 2. Create the app and its bot

1. <https://discord.com/developers/applications> → New Application
2. **Bot** tab → Reset Token, and keep the token

   > **You cannot see it again after leaving that screen.** If you lose it you have to reset it, and resetting immediately invalidates the previous one.

3. On the same **Bot** tab, under **Privileged Gateway Intents**, turn on **MESSAGE CONTENT INTENT**

   > **This is the step people get stuck on.** Left off, Discord answers by **closing the connection** rather than refusing it, and totsuka stops with guidance instead of reconnecting. For an app under 10,000 users this is just a toggle — no review, no application.

4. **OAuth2 → URL Generator**: pick the `bot` scope and the permissions **View Channels / Read Message History / Send Messages / Send Messages in Threads / Create Public Threads**. Open the generated URL and invite the bot to your dedicated server.

## 3. Copy two IDs

Turn on **Settings → Advanced → Developer Mode** in Discord; "Copy ID" then appears in right-click menus.

- **Your own user ID** (right-click your name)
- **The watched channel's ID** (right-click the channel)

> Both are **all digits**. Do not paste names: the user ID is rejected at startup if you do, but **the channel ID is not — it simply matches nothing**, which looks exactly like a watch nobody has used.

## 4. Write `config.toml`

```toml
[[repositories]]
name = "my-docs"
path = "~/Workspace/my-docs"

[plugins.discord]
enabled = true
command = "discord"

[discord]
bot_token = "op://Dev/Discord/bot_token"
operator_user_id = "111111111111111111"

[[workflows]]
name = "discord-clip"
source = "discord"
agent = "herdr"
profile = "implement"
output = "source"
initial_prompt = "/clip-doc Read the article at the URL in this post and leave a summary under ai-docs/references/. If there is no URL, stop without doing anything."
trigger = { channel = "222222222222222222", channel_name = "clip", repo = "my-docs" }
# from = ["333333333333333333"]   # optional: by default only your own posts trigger it
```

`channel` is the ID; `channel_name` is there to be checked against it. Names can change, so the ID is what the watch keys on, and totsuka compares the name at startup and warns if the two have drifted apart.

## 5. Start it and check

```bash
totsuka config validate
totsuka doctor
totsuka run --watch
```

`discord gateway ready` in the log means it is connected. Post a URL in the watched channel: a task is raised, and the result comes back in a thread on that post.

## Four things that catch people out

| Symptom | Cause | Fix |
|---|---|---|
| It stops right after starting with `discord gateway closed with 4014` | MESSAGE CONTENT INTENT is off | Turn the toggle on under Developer Portal → Bot and restart. **It stops there rather than reconnecting**, so it never looks like a flaky network |
| It starts but `discord gateway ready` never appears | The token is being rejected (`4004`), or the network | Read the close code in the log. `4004` means check the token |
| Posting does nothing | ① you wrote the channel name instead of its ID ② the bot cannot see that channel ③ the poster is not in `from` (by default only you are) | ① copy the ID again ② check the channel's own permission overrides ③ add them to `from` |
| Tasks finish but nothing is posted back | The bot lacks Send Messages in Threads / Create Public Threads | Add those to its role. The failure appears in the log |

## After resetting the token

Pressing Reset Token **invalidates the previous token immediately**. Update wherever you store it and restart totsuka. With a stale token it stops at startup and tells you so.

---

For the design decisions behind this, see `ai-docs/operations/discord-quickstart.md` in the repository.
