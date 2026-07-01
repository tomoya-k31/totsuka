# totsuka

A local-PC agent orchestration stack that drives Claude Code agents through [herdr](https://github.com/) (an existing, separately-installed tool that manages Claude Code panes over a Unix socket), using a GitHub Project (v2) board as the source of truth for task state.

The full architecture — startup/shutdown sequence, IPC matrix, state machines, and config schema — is documented in [`docs/superpowers/specs/2026-06-28-rust-app-decomposition-design.md`](docs/superpowers/specs/2026-06-28-rust-app-decomposition-design.md). This README covers only what's needed to build, configure, and run the stack locally.

## Architecture

Five binaries, each a thin `src/main.rs` over a testable `src/lib.rs`:

| Binary | Role |
|---|---|
| `totsukactl` | Supervisor + CLI. Boots/shuts down the whole stack in dependency order, manages Postgres via `docker compose`, exposes `up`/`down`/`status`/`restart`/`logs`. |
| `agent-adapter` | HTTP → herdr bridge; manages git worktrees for agent runs. |
| `orchestrator` | Task state machine; pulls work off the bus and drives `agent-adapter`. |
| `github-watcher` | Polls GitHub Projects V2 / Issues, publishes domain events to the bus. |
| `qa-service` | Slack Socket Mode bot — reaction-triggered issue creation, LLM-based repo classification, spawns Claude agents for Q&A threads. |

Four shared libraries underpin all five binaries:

- `totsuka-core` — domain types, event/effect keys, the `Clock` abstraction.
- `totsuka-bus` — a `pgmq` (Postgres message queue) publish/pull/ack wrapper.
- `totsuka-config` — TOML config schema, `${section.key}` and `~` expansion, validation.
- `totsuka-telemetry` — tracing setup, `/healthz`/`/readyz`, request-id propagation.

`totsuka-foundation-e2e` is a test-only crate (no library surface) exercising the shared libs together.

Boot order (`totsukactl up`): Postgres (docker compose) → preflight (config validation, `pgmq` version check, migration diff, herdr socket probe) → `agent-adapter` → `orchestrator` → `github-watcher` + `qa-service` in parallel, each gated on its own `/readyz`. Shutdown (`totsukactl down`) reverses this order with a graceful SIGTERM → escalate → SIGKILL sequence per stage.

## Prerequisites

- [mise](https://mise.jdx.dev/) — pins `rust` (stable), `cargo:sqlx-cli` (0.8.2), and `just`; see `mise.toml`. Run `mise install` once.
- Docker (for the local Postgres/`pgmq` container).
- herdr and Claude Code, already installed and running — this project assumes them as external, pre-existing tools, not something it manages.

## Local setup

1. Start the local Postgres (`pgmq`-enabled) container:

   ```bash
   just pgmq-up
   ```

   This runs `docker compose -f deploy/docker-compose.yml up -d pgmq`, publishing `127.0.0.1:5432` with database `totsuka` (see `deploy/docker-compose.yml`).

2. Bootstrap a config (writes `~/.config/totsuka/config.toml` + `secrets.toml`, brings up `pgmq`, applies migrations):

   ```bash
   cargo run --bin totsukactl -- init
   ```

   The generated `config.toml` documents its own schema inline — paths starting with `~/` are tilde-expanded at load, and values may reference `${other_section.key}`. Secrets (tokens, passwords) belong in the separate `secrets.toml` (created `chmod 0600`), never in `config.toml`. `examples/totsuka.toml.example` is a fully worked example of the schema.

3. Build and boot the stack:

   ```bash
   cargo build --release --bin totsukactl
   ./target/release/totsukactl up
   ./target/release/totsukactl status
   ./target/release/totsukactl down
   ```

## Build, test, lint

```bash
just test    # cargo test --workspace --all-features
just lint    # cargo clippy --workspace --all-targets --all-features -- -D warnings ; cargo fmt --check
```

Database-dependent tests need `DATABASE_URL` pointed at a Postgres instance with the `pgmq` extension and this project's migrations applied (`migrations/`, applied via `just db-migrate` or `sqlx migrate run --source migrations`). Tests that need a database check for `DATABASE_URL` and skip silently if it's unset, so `just test` passes locally without Postgres running — set `DATABASE_URL` to actually exercise those tests.

CI additionally runs `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (note the `--locked`, which `just lint` omits), `cargo fmt --all -- --check`, `cargo-deny`, and `typos` — see `.github/workflows/ci.yml`.

## Project history

Implementation work in this repo follows a spec → plan → execute cycle: design specs live under `docs/superpowers/specs/`, and per-feature/per-fix implementation plans (one per crate's initial build, plus later bugfix/tuning plans) live under `docs/superpowers/plans/`, tracked chronologically as a project log.
