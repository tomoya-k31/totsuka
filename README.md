> 🌐 **English** · [日本語](README.ja.md)

# totsuka

**AI-driven dev-flow automation.** totsuka detects task instructions from your
task sources (GitHub Issues, Notion, Slack mentions), matches them to
workflows, and orchestrates them to AI coding agents (herdr, orca) — each in
its own git worktree — then publishes the result as a pull request or writes
it back to the source.

- **Task sources**: GitHub Issues / Projects, Notion databases, Slack mentions
  (drafts replied under your own name after your approval)
- **Agents**: [herdr](https://herdr.dev/) and [orca](https://www.onorca.dev/)
  — third-party agent IDEs, each driven over totsuka's plugin protocol
- **Isolation**: one task = one repo = one worktree = one branch
- **Output policies**: open a pull request, write back to the source, or none
- **Local-first**: a single CLI binary, no daemon, secrets stay in your own
  secret store (1Password, Keychain, env, or a command)

> Status: pre-1.0, targeting the v1 scope — see the
> [releases page](https://github.com/tomoya-k31/totsuka/releases) for the
> current version. macOS only for now; the code is XDG-compliant and
> platform boundaries are abstracted for a future Linux port.

## Prerequisites

totsuka orchestrates agents; it does not bundle one. Install at least one
agent IDE and point a workflow at it:

- **[herdr](https://herdr.dev/)** — **0.7.5 or newer is required**. The plugin
  reads herdr's own version from `ping` during `initialize` and refuses
  anything older with `CONFIG_INVALID`. There is no upper bound: a newer herdr
  is never refused.
- **[orca](https://www.onorca.dev/)** — driven through the `orca` CLI.

## Install

### Homebrew

```sh
brew install tomoya-k31/tap/totsuka
```

That is the whole install: no `sudo`, no tree to place by hand, and no
quarantine attribute to clear. Homebrew fetches the release tarball with plain
`curl`, which does not set `com.apple.quarantine` — measured on macOS 15.7.3,
on the binary and on the bundled plugins.

Homebrew requires trust for third-party taps, but **naming the formula grants it
in the same command**. The install prints one line —
`==> Trusted formula tomoya-k31/tap/totsuka` — and carries on. There is no
prompt to answer.

Upgrade with `brew upgrade totsuka`; the release workflow points the formula at
each new release.

### Prebuilt tarball (GitHub Releases)

For a machine without Homebrew. Download the macOS universal tarball from the
[latest release](https://github.com/tomoya-k31/totsuka/releases/latest). It
contains `totsuka` **and the bundled plugins**, so keep the tree together and
symlink the binary onto your `PATH`:

```sh
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
```

The whole directory moves, not just the binary: `totsuka` looks for the bundled
plugins next to itself, which is how `totsuka setup` installs them with no path
from you. Keeping the tree in place also means adding or reinstalling a plugin
later needs no second download.

Everything is ad-hoc–signed. If Gatekeeper blocks it, clear the quarantine
attribute on the whole tree once:

```sh
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

### From source

```sh
cargo install --git https://github.com/tomoya-k31/totsuka orchestrator-cli
```

This installs the CLI only. Plugins are built from a checkout — see
[the plugin development guide](docs/plugin-dev-guide.md).

## Quickstart (5 minutes, 1 task)

```sh
# 1. Answer a few questions. Pick a starting recipe, name your repositories,
#    say where your secrets live — the wizard writes the config, installs and
#    enables the plugins the recipe needs, and finishes by running `doctor`.
totsuka setup

# 2. Register the secrets it listed. It never handles the values itself, so it
#    prints one ready-to-paste command per secret, e.g.
security add-generic-password -U -s totsuka -a github-token -w '<the token>'

# 3. Run one cycle (add --watch to keep polling).
totsuka run --dry-run   # preview: which task -> which repo -> which agent
totsuka run             # execute: fetch -> dispatch -> monitor -> publish
```

`setup` exits with code 3 until every secret it listed exists — that is
`doctor` reporting real work left to do, not a failure of the setup itself.

**`doctor` stays red until after your first `totsuka run`**, even with every
secret registered: the `state-db` check fails while the state database does not
exist, and only `run` creates it. So the order above is the order that goes
green — register the secrets, run once, then `totsuka doctor` exits 0. It may
still print `warn:` lines (an unset hook token, no bundled plugins); those are
advisory and do not fail it.

Inspect progress with `totsuka status`, drill into a task with
`totsuka task show <id>`, and follow logs with `totsuka logs -f`.

`totsuka init` is still there for CI and scripted bootstraps: it never prompts,
and writes only directories plus a fully commented config skeleton. `setup`
fills that skeleton in, so running `init` first is harmless but unnecessary.

`setup` also has a non-interactive form: answer once, keep the file, and bring
the next machine up from it. `setup` never writes a secret value into the file —
it records which backend to use and prints the commands to register the values —
so a file it generated is safe to commit to your dotfiles.

```sh
totsuka setup --save-answers ~/dotfiles/totsuka-answers.toml
totsuka setup --answers ~/dotfiles/totsuka-answers.toml --yes   # on the next machine
```

New machine, dev checkout, token rotation, and recovery are covered end to end
in the [setup playbook](docs/setup-playbook.md).

## Documentation

- **What totsuka is**: [docs/orchestrator-spec.md](./docs/orchestrator-spec.md)
- **Setup playbook**: [docs/setup-playbook.md](./docs/setup-playbook.md)
- **Slack setup**: [docs/slack-setup.md](./docs/slack-setup.md)
- **Configuration reference**: [docs/config-reference.md](./docs/config-reference.md)
- **Operations guide** (doctor / worktree cleanup / FAQ): [docs/operations-guide.md](./docs/operations-guide.md)
- **Plugin development guide**: [docs/plugin-dev-guide.md](./docs/plugin-dev-guide.md)
- **Table of contents**: [docs/index.md](./docs/index.md) · **Changelog**: [CHANGELOG.md](./CHANGELOG.md)

Those pages are generated from [`ai-docs/`](./ai-docs/), an
[Open Knowledge Format (OKF) v0.2](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)
Knowledge Bundle holding the repository's full knowledge — design decisions,
measurements, and history. Read that if you are working on totsuka itself.

## Contributing

Conventional Commits are required (`type(scope): description`). Releases are cut
by merging the [release-please](https://github.com/googleapis/release-please)
Release PR. Docs changes are validated by `bash scripts/okf-lint.sh ai-docs`.

## License

[MIT](./LICENSE).
