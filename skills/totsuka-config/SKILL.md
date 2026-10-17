---
name: totsuka-config
description: "Build or edit a totsuka config.toml with the user, one confirmed step at a time. Use when someone sets up totsuka for the first time, adds a task source (GitHub Projects, Notion, Slack, Discord), a project, a repository or a workflow to an existing totsuka config, changes a workflow's trigger or profile, or moves secrets to secret: references for the menu bar app."
---

# totsuka config

Walk the user through `config.toml` as a conversation: ask one step's questions, write that step with `totsuka config set`, check it, then move to the next. Talk in the user's language.

Your knowledge of the config comes from **the schema**, not from memory: `totsuka config schema` prints the JSON Schema of the installed `totsuka` and its installed plugins, with each key's meaning in `x-title` / `x-help` and secrets marked `x-secret`. Key names, required keys and allowed values all come from there, so the skill keeps working as totsuka changes. Query the part you need instead of reading all of it:

```bash
totsuka config schema | jq '.schema.properties.github'                          # a plugin's own table
totsuka config schema | jq '.schema.properties.projects.items["x-by-source"].github'  # a project entry for one source
totsuka config schema | jq '.schema.properties.workflows.items'                 # workflows (x-by-source = trigger keys)
```

A plugin's keys appear only after the plugin is installed.

## The ground rules

Every branch below writes the file the same way.

- **Read the file only through this filter**, which hides every secret-looking value that is not a `secret:` reference and keeps only its kind (`op`, `cmd`, `literal`, …):

  ```bash
  totsuka config get | jq 'walk(if type == "object" then with_entries(if (.key | test("(^|_)token$|^api_key(_ref)?$")) and (.value | type == "string") and (.value | startswith("secret:") | not) then .value = (((.value | capture("^(?<s>[a-z]+):") | .s) // "literal") + " (hidden)") else . end) else . end)'
  ```

  `config get` prints the file as it is, and an older config may hold a token in plain text. If the schema marks an `x-secret` key whose name the filter does not match, add it to the pattern.
- **Write with `totsuka config set <JSON Pointer> '<json>'`**. `set` keeps the file's comments, writes atomically, follows a symlinked config (GNU Stow) to its real file, and refuses any write that would leave the file unparseable.
- **One object per `set`.** `set` refuses a table that is missing a required key, so write a whole entry at once: `set /plugins/github '{"kind":"task_source","enabled":true}'`, not `enabled` alone.
- **Append to an array with `/-`**: `set /projects/- '{...}'`. With no array yet, `/projects/0` creates a table literally named `"0"` and is refused.
- **Key names come from the schema only.** `set` and `validate --offline` accept a misspelt key inside a plugin's table (`[github]`, `[slack]`, …): only the plugin checks its own keys, and that happens at the app's Start.
- **Every key marked `x-secret` gets `secret:<its dotted key path>`**: `[github].token` → `"secret:github.token"`, `[slack].user_token` → `"secret:slack.user_token"`, `[slack.llm].api_key` → `"secret:slack.llm.api_key"`. Never ask for or write a secret's value: the totsuka menu bar app asks for each `secret:` value at Start and keeps it in the Keychain. Use `secret:` for every secret in the file. A file that mixes `secret:` with `op://`, `cmd:`, `bw:` or `keychain:` cannot start, because the app's `run --secrets-stdin` refuses every other store.
- **Leave notifier plugins out.** The app posts notifications itself, so a new config has no `[plugins.macos]`. In an existing config, leave whatever is there.
- **Check after each step.** Run `totsuka config validate --offline`: exit 0 ends with `configuration is valid`; exit 1 prints `error:` lines naming what to fix. Plain `totsuka config validate` additionally probes the plugins that use no secret (herdr / orca must be running), and prints a `note:` for each one it skips because it uses `secret:`. That note is expected.

## 1. Preflight

1. `totsuka --version` must print 0.10.5 or later (`config set` arrived in 0.10.4, the app's "Use gh auth token" in 0.10.5). If it is older, or totsuka is missing, point to `brew install tomoya-k31/tap/totsuka` / `brew upgrade totsuka` and stop.
2. The menu bar app must exist, since only it can hand `secret:` values to totsuka: `ls -d /Applications/Totsuka.app ~/Applications/Totsuka.app "$(brew --prefix)/opt/totsuka/Totsuka.app" 2>/dev/null`. If none exists, stop and say why.
3. Read the file with the filter above: it prints `config_path`, `exists` and the contents. Tell the user which file you will edit (on some machines it is `hosts/<host>.toml`, not `config.toml`).
4. In an existing file, look for secrets outside `secret:`: the filter shows them as `op (hidden)`, `cmd (hidden)`, `literal (hidden)` and so on. If there are any, offer to replace each one with its `secret:<dotted key path>` before doing anything else, and explain that the app will ask for those values at the next Start.
5. Tell the user once that with `secret:` references, `totsuka doctor` and `totsuka run` in a terminal cannot resolve the secrets; the app is how totsuka is started.

## 2. Route

Decide from the user's request:

- No file, or a file with nothing but `version` → **3. New config**. When a filled file exists and the user wants to start over, ask them to move it aside themselves (`mv <config_path> <config_path>.bak`); `set` only adds to a file, so building a new config on top of the old one would keep the old entries running.
- "add Slack", "another board", "a new repo", "a workflow that …" → **4. Add**.
- "change the trigger", "make it design instead", "move the repo" → **5. Change**.

Only when the request names none of these, summarise what the file has (plugins, projects, repositories, workflows) and ask which.

## 3. New config

1. `totsuka config set /version 1` (this creates the file).
2. Ask which task sources the user wants (github, notion, slack, discord; one or more) and which agent IDE (herdr or orca). Install each with `totsuka plugin install --bundled <name> --yes`, then register it: `set /plugins/<name> '{"kind":"task_source","enabled":true}'` (`"agent_ide"` for herdr / orca). Done when `totsuka plugin list` shows each one installed.
3. The agent IDE's own table needs no keys; add one only if the user asks (layout and so on, from the schema). Remind them that herdr 0.7.5+ or the Orca app must be running when totsuka starts.
4. For each task source in turn, follow its file: [github.md](references/github.md), [notion.md](references/notion.md), [slack.md](references/slack.md) or [discord.md](references/discord.md). Each runs the human prerequisites, then `[<source>]`, `[[projects]]`, the repositories, and the workflows, with `validate --offline` after each. For the workflows, also read [workflows.md](references/workflows.md).
5. With Slack and two or more repositories, an LLM picks the repository for each message, so add `[llm]` (see slack.md).
6. Only if the user wants an AI tool other than Claude: `[tools.<name>]` and `default_tool` from the schema.
7. Finish with **6. Hand-off**.

## 4. Add

Install and register a missing plugin first, exactly as in step 3.2. Then follow the item's path, each ending in `validate --offline`:

- **A task source** → its file (as in step 3.4), then offer to add a workflow for it.
- **A project** (another board, another database) → the project entry from the source's file. A GitHub or Notion project with no repository linked to it fails at Start, so link a repository in the same sitting, then offer a workflow.
- **A repository** → `{"name", "path"}` (check the path is a git checkout: `git -C <path> rev-parse --show-toplevel`), plus `"project"` to link it. A repository links to one project. Linking one that already points at a GitHub or Notion board would leave that board without a repository, so offer a repository that is not linked yet. Then offer a workflow. If `[slack]` exists without `[[slack.repos]]`, every repository is a Slack candidate: when this one makes two and neither `[llm]` nor `[slack.llm]` exists, Slack refuses to start, so add `[llm]` in the same sitting (see slack.md).
- **A workflow** → follow the order in [workflows.md](references/workflows.md), with the questions, recipes and trigger keys from the source's file.

Finish with **6. Hand-off**. The running app does not reload the file: the user needs Stop then Start.

## 5. Change

1. Show the current value (workflows hold no secrets, so a direct read is fine here): `totsuka config get | jq '.config.workflows[] | select(.name == "<name>")'`.
2. Names that other entries refer to stay as they are: `projects[].name`, plugin names, `workflows[].name`. If the user wants to rename one, explain which entries refer to it and leave the edit to them.
3. A secret's value never lives in the file: to change one, the user picks **… → Forget saved secrets…** in the app and enters it again at the next Start.
4. Write the new value with `set` at its exact pointer (`/workflows/<index>/trigger`, `/repositories/<index>/path`, …). For a change to the profile, trigger, write-backs or `initial_prompt`, re-read that part of [workflows.md](references/workflows.md) and the source's file first. A profile cannot sit next to `mode` / `verification`, so remove those with `totsuka config unset` if present.
5. `validate --offline`, then **6. Hand-off**.

## 6. Hand-off

1. `totsuka config validate --offline` exits 0, and plain `totsuka config validate` shows no `error:` (a `note:` for each `secret:` plugin is expected).
2. Hand off to the app: the user clicks **Start** in the totsuka menu bar app (**Stop** first if it is running). The app asks once for each `secret:` value it does not have yet. For `github.token` it offers **Use gh auth token**, which needs the `project` scope; see [github.md](references/github.md). The app then runs the full check, the one that talks to GitHub, Notion, Slack and Discord.
3. Done when the app shows totsuka running. If Start fails, ask the user to paste the message from the app's menu, find the cause, fix it with `set`, and go back to step 1.
