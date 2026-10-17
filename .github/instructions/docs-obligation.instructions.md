---
applyTo: "crates/**,plugins/**,services/**,apps/**,scripts/**,.github/workflows/**,**/Cargo.toml"
---

# Docs obligation check

Write every comment in Japanese. A code change must ship with its docs **in
the same PR** (`ai-docs/CLAUDE.md`, "ドキュメントを残すタイミング").

The mechanical checks (a new crate without its component doc, a new `pub mod`
not named there, a missing `ai-docs/log.d/` fragment, a config field missing
from `config-reference.md`) run before the PR is opened
(`scripts/docs-gap.sh`). Do not repeat them. Review what needs judgement:

## Stale descriptions

When the PR changes what a function, CLI flag, config key, error message, or
plugin does, look for existing text that still describes the old behaviour and
was not updated:

- `ai-docs/` concepts whose frontmatter `resource` points at the changed crate
  or file, and any `ai-docs/` page that names the changed item
- rustdoc on the changed item and on its callers
- comments in `crates/orchestrator-cli/templates/config.toml`
- CLI `--help` strings and user-facing error messages

Quote the stale sentence with its `path:line`. Pages under `docs/` are
generated from `ai-docs/` (ADR-0047): point at the `ai-docs/` source instead.

## Docs that should exist

| Change in this PR | Expected |
|---|---|
| The responsibility or public interface of a crate changes | `ai-docs/components/<crate-name>.md` says so |
| `crates/plugin-protocol/**` methods, params, results, events | `ai-docs/apis/` |
| SQLite schema / migration / persisted data model | `ai-docs/data/` |
| `.github/workflows/**`, Dockerfile, IaC | `ai-docs/infrastructure/` |
| A choice between alternatives explained only in the PR or a code comment | a new `ai-docs/decisions/adr-NNNN-*.md` |
| A new domain term or abbreviation | `ai-docs/glossary/` |

## Do not flag

- Pure refactors, test-only changes, and dependency bumps with no behaviour
  change.
- What `okf-lint` / `docs-freshness` already enforce (frontmatter shape,
  `log.md` / `index.md` sync, stale `docs/` pages).
- Past records: ADRs (`ai-docs/decisions/`), release notes, `ai-docs/log.d/`.
