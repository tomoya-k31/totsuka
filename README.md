> 🌐 **English** · [日本語](README.ja.md)

# totsuka

**AI-driven dev-flow automation.** totsuka detects task instructions from your
task sources (GitHub Issues, Notion, Slack mentions), matches them to
workflows, and orchestrates them to AI coding agents (herdr, orca) — each in
its own git worktree — then publishes the result as a pull request or writes
it back to the source.

- **Task sources**: GitHub Issues / Projects, Notion databases, Slack mentions
  (drafts replied under your own name after your approval)
- **Agents**: herdr, orca (agent IDEs driven over a plugin protocol)
- **Isolation**: one task = one repo = one worktree = one branch
- **Output policies**: open a pull request, write back to the source, or none
- **Local-first**: a single CLI binary, no daemon, secrets stay in the Keychain

> Status: v1. macOS only for now; the code is XDG-compliant and platform
> boundaries are abstracted for a future Linux port.

## Install

### Prebuilt tarball (GitHub Releases)

Download the macOS universal tarball from the
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
[the plugin development guide](ai-docs/development/plugin-dev-guide.md).

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

New machine, dev checkout, token rotation, and recovery are covered end to end
in the [setup playbook](ai-docs/operations/setup-playbook.md) (Japanese).

## Documentation

All project knowledge lives in [`ai-docs/`](./ai-docs/), an
[Open Knowledge Format (OKF) v0.2](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)
Knowledge Bundle.

- **Product spec**: [ai-docs/product/orchestrator-spec.ja.md](./ai-docs/product/orchestrator-spec.ja.md)
- **Configuration reference**: [ai-docs/development/config-reference.md](./ai-docs/development/config-reference.md)
- **Plugin development guide**: [ai-docs/development/plugin-dev-guide.md](./ai-docs/development/plugin-dev-guide.md)
- **Operations guide** (doctor / worktree cleanup / FAQ): [ai-docs/operations/operations-guide.md](./ai-docs/operations/operations-guide.md)
- **Table of contents**: [ai-docs/index.md](./ai-docs/index.md) · **Changelog**: [CHANGELOG.md](./CHANGELOG.md)

## Contributing

Conventional Commits are required (`type(scope): description`). Releases are cut
by merging the [release-please](https://github.com/googleapis/release-please)
Release PR. Docs changes are validated by `bash scripts/okf-lint.sh ai-docs`.

## License

[MIT](./LICENSE).
