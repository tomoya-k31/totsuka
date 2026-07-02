# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`totsuka` is a local-PC agent orchestration stack: 5 binaries + 4 shared libs coordinating Claude Code agents (via the existing tool **herdr**, assumed already running) using GitHub Projects V2 as the source of truth for task state.

- **Binaries** (`crates/{name}`, each has a thin `src/main.rs` + logic in `src/lib.rs`): `totsukactl` (supervisor/CLI — boots/shuts down the whole stack, manages Postgres via docker compose), `agent-adapter` (HTTP→herdr bridge + git worktree management), `orchestrator` (task state machine, drives agent-adapter), `github-watcher` (polls GitHub ProjectsV2/Issues), `qa-service` (Slack Socket Mode bot).
- **Shared libs**: `totsuka-core` (domain types), `totsuka-bus` (pgmq wrapper), `totsuka-config` (TOML schema + `${section.key}`/`~`/`op://` expansion), `totsuka-telemetry` (tracing/healthz/readyz).
- `secrets.toml` values may be `op://vault/item/field` (1Password Secret Reference) instead of plaintext, resolved via the `op` CLI during `Config::load()`. Since every binary loads config independently at its own startup, each one needs `op` authenticated in its own process environment (e.g. `OP_SERVICE_ACCOUNT_TOKEN`) — there's no central process that resolves secrets once and distributes them.
- Full architecture, startup/shutdown sequence, and config schema: `@docs/superpowers/specs/2026-06-28-rust-app-decomposition-design.md`.

## Build, test, lint

```bash
just test    # cargo test --workspace --all-features
just lint    # cargo clippy --workspace --all-targets --all-features -- -D warnings ; cargo fmt --check
```

**CI's clippy invocation is stricter than `just lint`** — it adds `--locked` (catches lockfile drift too). Use this exact command before considering clippy clean:

```bash
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

DB-dependent tests need `DATABASE_URL` (Postgres + `pgmq` extension); `just pgmq-up` starts the dev container (`deploy/docker-compose.yml`, `totsuka-pgmq`). Tests that need it check `std::env::var("DATABASE_URL")` and silently `return` early if unset — `cargo test` passes locally without Postgres running, but skipped tests aren't the same as passing tests.

## Code style

- `#![forbid(unsafe_code)]` at the top of every crate's `lib.rs` — no unsafe code, anywhere.
- **Never call `Utc::now()` / `SystemTime::now()` in production code.** Take a `totsuka_core::Clock` (`trait Clock { fn now(&self) -> DateTime<Utc>; }`, in `crates/totsuka-core/src/clock.rs`) as a dependency instead — `SystemClock` in production, `MockClock` (fixed/advanceable) in tests. This is enforced by convention, not a lint; grep for stray `Utc::now()`/`SystemTime::now()` outside `clock.rs` and test files before considering a change done.
- `[profile.release] panic = "abort"` — don't rely on unwinding for control flow.

## Testing conventions

- `#[tokio::test]` (single-threaded) is the default. Use `#[tokio::test(flavor = "multi_thread")]` only for true end-to-end tests that spawn real OS-level concurrency (process spawn/signals, real servers) — see `crates/github-watcher/tests/e2e_*.rs` and `crates/totsukactl/tests/e2e_lifecycle.rs`.
- When writing a test that waits for an async background task to finish some work, don't use a fixed `sleep()` before checking results — it races under CI load. Poll a readiness signal that's provably ordered after the work (a DB cursor/row write, a channel message) with a bounded timeout instead.
- If a test uses a real `Clock` combined with hardcoded absolute fixture dates and a time-window filter, it will eventually fail once wall-clock time drifts past the window — inject `MockClock` with a fixed instant instead.

## Commits

Conventional Commits, scoped by crate name (or `workspace` for cross-cutting changes): `fix(totsukactl): ...`, `feat(github-watcher): ...`, `docs: ...`. PR merges keep the `(#N)` reference in the squashed commit subject.
