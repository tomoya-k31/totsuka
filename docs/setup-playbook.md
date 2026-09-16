> 🌐 **English** · [日本語](setup-playbook.ja.md)

<!-- generated-from: ai-docs/operations/setup-playbook.md sha256:e29b2b7fd960b2d6731137bb679106e775ab28feb0c37aba0aa081ee2214b4be -->

# Setup playbook

Getting from nothing to a working totsuka, end to end: a new machine, a development checkout, rotating tokens, and recovering when setup fails partway.

Targets macOS. For individual topics:

| What you want | Where |
|---|---|
| What each config key means | [Configuration reference](config-reference.md) |
| Reading doctor, cleaning up worktrees | [Operations guide](operations-guide.md) |
| Writing your own plugin | [Plugin development guide](plugin-dev-guide.md) |

## Installing on a new machine

### 1. Put it in place

```bash
brew install tomoya-k31/tap/totsuka
```

That is the whole install: no `sudo`, no tree to place by hand, and no `xattr`. The bundled plugins land in `libexec/totsuka/plugins`, which is one of the places `totsuka` looks, so setup can install them with no path from you.

Homebrew requires trust for third-party taps, but naming the formula grants it in the same command. The install prints one line — `==> Trusted formula tomoya-k31/tap/totsuka` — and carries on. There is no prompt to answer.

Upgrade with `brew upgrade totsuka`.

#### Without Homebrew

Download the macOS universal tarball from the [latest release](https://github.com/tomoya-k31/totsuka/releases/latest). Move the **whole tree** — `totsuka` looks for its bundled plugins next to itself, so moving just the binary leaves setup unable to find them.

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

**Only this route needs the `xattr` step.** A browser download carries `com.apple.quarantine`, and Gatekeeper then silently kills **plugin startup only**. The main binary works, which makes this hard to spot; `doctor` can only report "crashed or exited". Homebrew fetches with plain `curl`, which never sets the attribute.

### 2. Run `totsuka setup`

```bash
totsuka setup
```

**It asks one question** — which plugins you will use (arrow keys to move, space to toggle, enter to confirm). Nothing else. It installs what you picked and writes a `config.toml` with **every setting totsuka understands in it, commented out**, then tells you where that file is.

Non-interactively:

```bash
totsuka setup --plugins github,herdr,macos     # or --plugins all / --plugins none
totsuka setup --plugins all --secret-backend bw
```

**Without a terminal and without `--plugins`, it stops.** A default selection would install plugins you did not pick, and an empty one would be indistinguishable from a run that worked. A misspelling (`--plugins gihub`) is an error for the same reason.

`--secret-backend` takes `op` (the default), `bw`, `keychain`, `cmd` or `env`. **The form you pick is written into `config.toml` as well**, not just into the commands printed at the end — otherwise you would be told to run `security add-generic-password` over a file full of `op://` references, and register a secret nothing reads. Each reference line carries the other four forms in the comment above it, so switching stores later needs no documentation.

The generated file has **exactly one live line, `version = 1`**; everything else is commented out. So `totsuka config validate` passes immediately, and **nothing runs yet**. At minimum, uncomment:

1. a `[[repositories]]` block — the local clone tasks are dispatched into
2. `[plugins.<name>] enabled = true` for the plugins you installed
3. that plugin's own `[<name>]` table, including its token reference
4. `[[projects]]` and `[[workflows]]` — **the recipe section at the end of the file** has four combinations that work, ready to uncomment

**The plugins are installed but not enabled.** `totsuka config validate` launches every enabled plugin, so enabling `github` while `[github].token` is still commented out would make the command that confirms your setup fail on the setup itself.

**If a `config.toml` already exists, only the missing sections are appended.** Not one existing line changes. When you later want Notion, `totsuka setup --plugins notion` adds the commented `[notion]` skeleton to the end of the file.

### 3. Register your secrets

Setup finishes with **the secrets the plugins you picked are referenced by**. Each line gives the reference name, what it enables, and the command to register it, so you can copy them straight out.

```bash
security add-generic-password -U -s totsuka -a github-token -w '<paste the value>'
```

**Register the ones whose lines you actually uncomment.** The list names every reference the file *mentions*, but a commented line is never resolved. The converse is what matters: an uncommented reference that is not registered stops that plugin from starting.

`--secret-backend cmd` and `env` have no registration command — the value lives in another tool or in your environment — so they say so instead.

If you chose Bitwarden, **run `bw login`, then `bw unlock`, and export the `BW_SESSION` it prints before you start** — the registration command writes to your vault and needs an unlocked session. (The prerequisites table below says the same thing, but this step comes first, so it is repeated here.) The command is a `bw get template item | jq … | bw encode | bw create item` pipeline (it needs `jq`), because `bw` has no single-line equivalent of `op item edit`. **That command always creates a new item** — if one with the same name already exists, edit that one instead, because a duplicate makes `bw get` fail with "more than one result" and the reference stops resolving. You get one item per account (`bw:totsuka-<name>/password`). That is a convention rather than a limit — a Bitwarden item does hold a `username`, `password`, `uri` and `totp` — but `bw:` references do not reach custom fields, so an item cannot hold *arbitrarily many* secrets, and everything setup names is a token, which maps to the same `password` object.

If you reply on Slack under your own name, register the bot token too: **replies posted under your own name raise no Slack notification at all**, so without the bot's nudge you never learn a draft is waiting.

### 4. Verify and run

Do this **after** editing `config.toml`. Setup does not run `doctor` for you: before you edit, the configuration is effectively empty, so `doctor` would only report that nothing is configured yet.

```bash
totsuka config validate # the configuration parses and hangs together
totsuka doctor          # tells you if any secret is still unregistered
totsuka run --dry-run   # which task goes to which agent in which repository
totsuka run --watch
```

The `state-db` check fails until you have run `totsuka run` at least once. That is expected and clears itself.

### 5. One-time steps in the tools themselves (when they apply)

Things setup cannot do on your behalf.

| Tool | What you have to do |
|---|---|
| Codex | Approve hooks trust in the TUI. **Without it, hooks are silently skipped and every task times out** |
| OpenCode | First launch and config placement |
| 1Password | `op signin`, if you use `op://` references |
| Bitwarden | `bw login`, then `bw unlock`, then export the `BW_SESSION` it prints — if you use `bw:` references. **Start `totsuka run` from that same shell**: `bw` keeps no background session, and without one it asks for your master password on standard input, which leaves a long-running process stopped with nothing on screen |
| Click-to-focus notifications | Install `terminal-notifier` and set the bundle id — see [click-to-focus setup](click-to-focus-setup.md) |

## Installing from a development checkout

Build and install from source. No tarball needed.

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka setup --plugins all
```

`--from-source` walks upwards from the current directory looking for one that is both a Cargo workspace root and has a `plugins/` directory, so it will not misfire inside some other repository. Running `totsuka setup` inside the checkout picks `--from-source` automatically when there is no bundled tree.

Reinstalling a single plugin after changing it uses the same path.

```bash
totsuka plugin install --from-source slack --enable
```

Add `--print-plan` to see what would be built and installed from where, without invoking cargo.

## Rotating tokens

### Slack — changing a scope reissues both tokens

**This is the easiest trap to fall into.** Changing your Slack app's scopes requires a reinstall, and that reissues **both** the user token (`xoxp-`) and the bot token (`xoxb-`). Update only one and only the other one's functionality breaks.

```bash
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'
```

The app-level token (`xapp-`) does not change on reinstall. Update it only when you explicitly regenerate it.

The scopes themselves have a trap too: without `reactions:read`, `channels:read`, and `groups:read`, **events simply never arrive and nothing reports an error**.

### In general

You do not need to re-run `setup`. The reference names have not changed, only the values, so overwrite them with `-U` (update existing) and run `totsuka doctor`.

## Recovering from a failed setup

### It failed partway

**Run it again.** Each step is idempotent, and it prints how far it got.

```bash
totsuka setup --plugins <the same selection>
```

Only the sections your `config.toml` is missing get appended, so the second run effectively does just the plugin installation.

### You want to start the configuration over

Setup never rewrites a line in an existing file. To start from scratch, move it aside yourself.

```bash
mv ~/.config/totsuka/config.toml{,.bak}
totsuka setup --plugins all
```

**To add a section you do not need to move anything aside.** `totsuka setup --plugins notion` appends the commented `[notion]` skeleton to the end of the file and changes nothing you wrote.

### You want the same configuration on another machine

**Take the `config.toml` itself.** Setup writes no secret *values* — only references such as `op://…` and `keychain:…` — so the file is safe to keep in your dotfiles.

```bash
cp ~/.config/totsuka/config.toml ~/dotfiles/totsuka-config.toml
```

On the other machine:

```bash
totsuka setup --plugins <the same selection>   # directories and plugin installation
cp ~/dotfiles/totsuka-config.toml ~/.config/totsuka/config.toml
```

Registering the secrets themselves is still done by a human on each machine. Fix up `[[repositories]].path` if your clones live somewhere else there.

### doctor is still red

The [operations guide](operations-guide.md) covers how to read it. The ones that show up right after installation:

| Check | Usual cause |
|---|---|
| `state-db` | You have not run `totsuka run` yet (expected) |
| `plugin:<name>` — secret not found | A secret from the checklist was not registered |
| `plugin:<name>` — crashed or exited | You skipped `xattr -dr com.apple.quarantine` |
| `bundled-plugins` (warning) | A `cargo install` build ships no bundled plugins. Use `--from-source` |
| `hook-token` (warning) | `[hooks].auth_token_ref` is unset. Set it before using a hook-capable agent |

---

This page is generated from the internal document `ai-docs/operations/setup-playbook.md`, which carries the design decisions and measurements behind it.
