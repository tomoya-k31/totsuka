# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Totsuka is an AI-driven dev-flow automation tool (detects task instructions and orchestrates them to AI agents via a Socket API, working with `herdr`). The repo is a Rust workspace (see the root `Cargo.toml` for the member crates); all project knowledge lives in the `ai-docs/` OKF bundle.

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

## Code Intelligence (Swift / sourcekit-lsp)

For the menu bar app under `apps/macos/` (ADR-0109), prefer LSP over Grep/Read
for navigation, the same way as rust-analyzer above (`goToDefinition`,
`findReferences`, `hover`, `documentSymbol`, …), and check LSP diagnostics
after editing Swift files.

- `apps/macos/Package.swift` is a SwiftPM package (`TotsukaKit` + the
  `TotsukaApp` executable built from `Sources/Totsuka`), which sourcekit-lsp
  indexes directly — run `swift build` there
  once so cross-file results resolve.
- The shipped `.app` is built from `apps/macos/project.yml` (XcodeGen; the
  `.xcodeproj` is generated and not committed). sourcekit-lsp does not read
  that project without a build server (e.g. `xcode-build-server`), so use the
  package, not the generated project, for navigation.
- On a machine with Command Line Tools only, run the tests with
  `apps/macos/test.sh` (plain `swift test` cannot find swift-testing there).
  The asset catalog needs Xcode's `actool`, so CI's `macos app` job
  (`xcodebuild`) is the gate for the app build itself.

## Documentation (`ai-docs/` = OKF Knowledge Bundle)

All knowledge about this repository lives in `ai-docs/`, an [OKF v0.2](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)-compliant Knowledge Bundle.

### Reading (progressive disclosure)

Do not scan all of `ai-docs/` at once. Always follow this order:

1. `ai-docs/index.md` — top-level table of contents: which directory holds what
2. The `index.md` of the relevant directory — its concept list with one-line summaries
3. Only then open the specific concept file(s) you actually need

Start from `ai-docs/decisions/index.md` for past design decisions, `ai-docs/operations/index.md` for runbooks, `ai-docs/glossary/index.md` for terminology, and `ai-docs/log.md` for a summary of recent changes.

For cross-cutting queries by frontmatter (e.g. "all `type: Decision` docs", "everything `status: deprecated`"), use the `okf-search` skill (`scripts/okf-search.sh`) instead of walking every `index.md` by hand.

### Writing

Before creating or updating anything under `ai-docs/`, **always** read `ai-docs/CLAUDE.md` first and follow its rules (frontmatter, `type` vocabulary, index/log update obligations, when to write). Use the `okf-docs` skill when it's available.

### Obligation when changing code

Work involving design decisions, new components, API/schema/infra changes, incident response, or releases must update the corresponding docs and `index.md`/`log.md` **in the same PR** — follow the trigger table in `ai-docs/CLAUDE.md`.

### Verification

After changing docs, run `bash scripts/okf-lint.sh ai-docs` and get it to zero errors before finishing. A PostToolUse hook (`.claude/settings.json`) also runs this automatically after edits under `ai-docs/`, and CI (`.github/workflows/okf-lint.yml`) runs it on PRs touching `ai-docs/**`.
