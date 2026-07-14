# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Totsuka is an AI-driven dev-flow automation tool (detects task instructions and orchestrates them to AI agents via a Socket API, working with `herdr`). The repo is currently a fresh scaffold: **no source code exists yet** — language and directory structure are still undecided. The only established structure right now is the `docs/` knowledge bundle and its tooling.

## Code Intelligence (Rust / rust-analyzer)

Prefer LSP over Grep/Glob/Read for code navigation:
- `goToDefinition` / `goToImplementation` — jump to source
- `findReferences` — find all usages (required before renaming or changing a function signature)
- `workspaceSymbol` — find where something is defined by name
- `documentSymbol` — list all symbols in a file
- `hover` — get type info without reading the file
- `incomingCalls` / `outgoingCalls` — trace call hierarchy

Use Grep/Glob only for text/pattern searches (comments, strings, config values) where LSP doesn't help.

Trust the language server's results; do not re-read files to double-check them — that wastes tokens and defeats the purpose of using LSP.

After writing or editing code, check LSP diagnostics before moving on. Fix type errors or missing imports immediately. Diagnostics from rust-analyzer are backed by rustc and clippy, so they're a reliable substitute for running `cargo check` / `cargo clippy` manually during iteration.

If rust-analyzer becomes slow or unstable on this workspace (large monorepo), it's fine to disable it temporarily with `/plugin disable rust-analyzer-lsp@claude-plugins-official` and fall back to Grep.

## Documentation (`docs/` = OKF Knowledge Bundle)

All knowledge about this repository lives in `docs/`, an [OKF v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)-compliant Knowledge Bundle.

### Reading (progressive disclosure)

Do not scan all of `docs/` at once. Always follow this order:

1. `docs/index.md` — top-level table of contents: which directory holds what
2. The `index.md` of the relevant directory — its concept list with one-line summaries
3. Only then open the specific concept file(s) you actually need

Start from `docs/decisions/index.md` for past design decisions, `docs/operations/index.md` for runbooks, `docs/glossary/index.md` for terminology, and `docs/log.md` for a summary of recent changes.

For cross-cutting queries by frontmatter (e.g. "all `type: Decision` docs", "everything `status: deprecated`"), use the `okf-search` skill (`scripts/okf-search.sh`) instead of walking every `index.md` by hand.

### Writing

Before creating or updating anything under `docs/`, **always** read `docs/CLAUDE.md` first and follow its rules (frontmatter, `type` vocabulary, index/log update obligations, when to write). Use the `okf-docs` skill when it's available.

### Obligation when changing code

Work involving design decisions, new components, API/schema/infra changes, incident response, or releases must update the corresponding docs and `index.md`/`log.md` **in the same PR** — follow the trigger table in `docs/CLAUDE.md`.

### Verification

After changing docs, run `bash scripts/okf-lint.sh docs` and get it to zero errors before finishing. A PostToolUse hook (`.claude/settings.json`) also runs this automatically after edits under `docs/`, and CI (`.github/workflows/okf-lint.yml`) runs it on PRs touching `docs/**`.
