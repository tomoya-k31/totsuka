> 🌐 **English** · [日本語](setup-playbook.ja.md)

<!-- generated-from: ai-docs/operations/setup-playbook.md sha256:ff191fb4a433cda727054ab9b5868110d2dff4f6312c5432674cca748eb0cb5a -->

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

Download the macOS universal tarball from the [latest release](https://github.com/tomoya-k31/totsuka/releases/latest). Move the **whole tree** — `totsuka` looks for its bundled plugins next to itself, so moving just the binary leaves setup unable to find them.

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

Skip the `xattr` step and Gatekeeper silently kills **plugin startup only**. The main binary works, which makes this hard to spot; `doctor` can only report "crashed or exited".

### 2. Run `totsuka setup`

```bash
totsuka setup
```

It asks four kinds of question and gets the rest from the recipe you choose.

1. **Which recipe to start from** (minimal GitHub, design-to-implement handoff, Slack replies under your own name, human sign-off required)
2. **Repository paths and names** (more than one is fine)
3. **Where to keep secrets** (Keychain, 1Password, or environment variables) — it never asks for the values themselves
4. Whatever the recipe still needs (GitHub Project owner and number, your Slack member ID, the LLM model name)

It prints a plan; nothing has side effects until you confirm. **Pressing Ctrl-C during the questions leaves nothing behind.**

It then generates per-plugin config, installs and enables the plugins, and runs `doctor`. You do not need to run `totsuka init` first.

### 3. Register your secrets

Setup finishes with a checklist. Each line gives the reference name, what it enables, and the command to register it, so you can copy them straight out.

```bash
security add-generic-password -U -s totsuka -a github-token -w '<paste the value>'
```

**Everything on that checklist is required.** Your configuration refers to these, so a single missing one stops that plugin from starting. Anything genuinely optional never appears on the list in the first place.

The Slack bot token looks optional but is not: **replies posted under your own name raise no Slack notification at all**, so the recipes are built around the bot delivering the nudge.

### 4. Verify and run

```bash
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
| Click-to-focus notifications | Install `terminal-notifier` and set the bundle id |

## Installing from a development checkout

Build and install from source. No tarball needed.

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka plugin install --from-source --all --enable
totsuka setup
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
totsuka setup
```

Existing config files are skipped, so the second run effectively does just the plugin installation and `doctor`.

### You want to start the configuration over

Setup never overwrites existing files. To start over, move them aside yourself.

```bash
mv ~/.config/totsuka/config.toml{,.bak}
mv ~/.config/totsuka/plugins ~/.config/totsuka/plugins.bak
totsuka setup
```

One exception: the all-comments template that `totsuka init` writes is treated as unconfigured, and setup fills it in. That one needs no moving aside.

### You want the same configuration on another machine

Save the answers file and take it with you. **Secrets cannot structurally end up in it** — the answer format has no field that could hold a token — so it is safe to keep in your dotfiles.

```bash
totsuka setup --save-answers ~/dotfiles/totsuka-answers.toml
totsuka setup --answers ~/dotfiles/totsuka-answers.toml --yes
```

Registering the secrets themselves is still done by a human on each machine.

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
