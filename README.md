> 🌐 **English** · [日本語](README.ja.md)

# totsuka

**AI-driven dev-flow automation.** totsuka detects task instructions from your
task sources (GitHub Issues, Notion), matches them to workflows, and
orchestrates them to AI coding agents (herdr, orca) — each in its own git
worktree — then publishes the result as a pull request or writes it back to the
source.

- **Task sources**: GitHub Issues / Projects, Notion databases
- **Agents**: herdr, orca (agent IDEs driven over a plugin protocol)
- **Isolation**: one task = one repo = one worktree = one branch
- **Output policies**: open a pull request, write back to the source, or none
- **Local-first**: a single CLI binary, no daemon, secrets stay in the Keychain

> Status: v1. macOS only for now; the code is XDG-compliant and platform
> boundaries are abstracted for a future Linux port.

## Install

### Prebuilt binary (GitHub Releases)

Download the macOS universal binary from the
[latest release](https://github.com/tomoya-k31/totsuka/releases/latest), then
put `totsuka` on your `PATH`:

```sh
tar -xzf totsuka-*-macos-universal.tar.gz
install -m 0755 totsuka /usr/local/bin/totsuka
```

The binary is ad-hoc–signed. If Gatekeeper blocks it, clear the quarantine
attribute once:

```sh
xattr -d com.apple.quarantine /usr/local/bin/totsuka
```

### From source

```sh
cargo install --git https://github.com/tomoya-k31/totsuka orchestrator-cli
```

## Quickstart (5 minutes, 1 task)

```sh
# 1. Scaffold the config and check the environment.
totsuka init

# 2. Install the plugins you need (task source, agent, notifier), then enable them.
totsuka plugin install ./path/to/task-source-github
totsuka plugin enable github

# 3. Store your secrets in the Keychain and reference them from config
#    (e.g. api_key_ref = "keychain:totsuka/github"); edit
#    ~/.config/totsuka/config.toml — repositories, workflows, and the [llm] block.

# 4. Verify everything is wired up.
totsuka doctor

# 5. Run one cycle (add --watch to keep polling).
totsuka run --dry-run   # preview: which task -> which repo -> which agent
totsuka run             # execute: fetch -> dispatch -> monitor -> publish
```

Inspect progress with `totsuka status`, drill into a task with
`totsuka task show <id>`, and follow logs with `totsuka logs -f`.

## Documentation

All project knowledge lives in [`docs/`](./docs/), an
[Open Knowledge Format (OKF) v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)
Knowledge Bundle.

- **Product spec**: [docs/product/orchestrator-spec.ja.md](./docs/product/orchestrator-spec.ja.md)
- **Configuration reference**: [docs/development/config-reference.md](./docs/development/config-reference.md)
- **Plugin development guide**: [docs/development/plugin-dev-guide.md](./docs/development/plugin-dev-guide.md)
- **Operations guide** (doctor / worktree cleanup / FAQ): [docs/operations/operations-guide.md](./docs/operations/operations-guide.md)
- **Table of contents**: [docs/index.md](./docs/index.md) · **Changelog**: [CHANGELOG.md](./CHANGELOG.md)

## Contributing

Conventional Commits are required (`type(scope): description`). Releases are cut
by merging the [release-please](https://github.com/googleapis/release-please)
Release PR. Docs changes are validated by `bash scripts/okf-lint.sh docs`.

## License

[MIT](./LICENSE).
