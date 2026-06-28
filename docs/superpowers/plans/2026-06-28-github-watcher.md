# github-watcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A polling-only producer that turns GitHub state into the `github.*` DomainEvents that orchestrator consumes — `status_changed` (ProjectsV2 column snapshot diff), `pr_merged_ready` / `pr_verification_passed` / `pr_verification_failed` (PR linkage), `release_published`, `issue_updated` — with snapshot UPSERT, cursor update, and bus publish atomic in one Postgres transaction.

**Architecture:** Crate `crates/github-watcher/` (bin + lib). The lib is layered: `gh_client/` (HTTPS to GitHub: GraphQL ProjectsV2 + REST issues / PRs / releases) → `snapshot` (`gh_item_status` repository with same-tx publish) + `cursor` (`catchup_cursor` get/set) → `column_map` (display name → `ColumnId` via `totsuka-core::ColumnMap`) + `linkage` (branch-name + `Totsuka-Task:` trailer → `task_id` via `tasks`) → `polling/` (one tokio loop per stream: project / issues / prs / releases). The bin wires them, plus telemetry/lifecycle from foundation. All time goes through `Arc<dyn Clock>`; all secrets through `Secret<String>`.

**Tech Stack:** Rust stable / tokio / sqlx (postgres + chrono) / axum (healthz over TCP loopback) / reqwest (rustls, GitHub HTTPS) / serde + serde_json / anyhow (bin) / thiserror (lib) / async-trait / chrono

## Global Constraints

(spec §11 verbatim, plus §8.3)

- Rust toolchain: **stable**, `[profile.release] panic = "abort"`, `tokio::task::block_in_place` clippy-denied at workspace level
- Schema versioning (spec §11.1): `const MIN_SCHEMA_VERSION: i32 = 6; const TARGET_SCHEMA_VERSION: i32 = 6;` at the bin entry. Mismatch → `SchemaOutOfRange` from `WatcherError` + exit 1
- Time (spec §11.5): all `DateTime` via `Arc<dyn Clock>`; `Utc::now()` direct call is clippy-denied. Storage UTC, display Asia/Tokyo. `published_at` on every envelope comes from `Clock::now()`
- Errors (spec §11.6): lib `thiserror`, bin `anyhow`; HTTP errors → RFC7807 `/errors/<kind>`
- Secrets (spec §11.7): `Secret<String>` for `github_token`; `.expose()` only at the outbound reqwest call site. Token is never logged
- Bounded channels (spec §11.8): `watcher: ProjectsV2 diff → bus publish` bounded at `graphql_page_size = 100`, full → block (one page committed atomically before the next page is fetched)
- Blocking isolation (spec §11.10): none expected — all GitHub I/O is async reqwest, all DB I/O is async sqlx
- ColumnId mapping (spec §11.4): display names from `[github].columns` are canonical; unknown display name on a project item → readyz NG (`column_map: unknown display "🚧 ???"`) + `config_error` notification
- Snapshot + cursor + bus publish atomicity (spec §8.3 & §9.3): every diff row is committed in one Postgres transaction via `Publisher::send_in_tx`
- Idempotency (spec §8.3 & §11.15): event_keys are deterministic so re-publish is absorbed by orchestrator's `processed_events`
  - status: `gh:status:{item_id}:{to_status_hash}` (`to_status_hash` = first 8 hex chars of md5(to_status_snake_case))
  - issue: `gh:issue:{issue_node_id}:{updated_at_ms}`
  - pr: `gh:pr:{pr_node_id}:{updated_at_ms}` and event-specific suffixes (`pr_merged`, `pr_verif_pass`, `pr_verif_fail`)
  - release: `gh:release:{release_node_id}`
- PR ↔ task linkage (spec §11.14): primary = branch name parse `totsuka/{task_id_short}/{phase_short}`; fallback = `Totsuka-Task: {full_task_id}` trailer at end of PR body. Both consulted; mismatch → log warn + prefer trailer. No match → publish PR event with `task_id = null`, orchestrator ignores it
- Rate limit & backoff: 4xx → fail fast & log; 5xx → exp backoff (max 30 s, 3 retries); 403 with `X-RateLimit-Remaining: 0` → sleep until `X-RateLimit-Reset`; 429 → respect `Retry-After`
- HTTP listener: spec §7 IPC matrix says watcher is the **only** bin that uses TCP loopback (`127.0.0.1:7802`) — not UDS — because it is the cloud-migration candidate
- GraphQL injection prevention: every user-supplied or config-supplied value (project id, repo names, cursors) MUST go through GraphQL `variables` — never `format!`-interpolated into the document. Same pattern as orchestrator's `gh_writeback/http.rs` after PR #4
- Pre-flight: foundation (PR #1) + agent-adapter (PR #2) + orchestrator (PR #3 / #4) merged into main. This bin's e2e tests need only the pgmq container (no agent-adapter, no orchestrator process — the bus + DB suffice)

---

## File Structure

```
crates/github-watcher/
├── Cargo.toml                          [Create] bin + lib
└── src/
    ├── main.rs                         [Create] anyhow entry, wiring
    ├── lib.rs                          [Create] WatcherApp + module re-exports
    ├── error.rs                        [Create] WatcherError + code()
    ├── schema_check.rs                 [Create] MIN/TARGET_SCHEMA_VERSION + check_schema_version
    ├── column_map.rs                   [Create] load ColumnMap from [github].columns
    ├── cursor.rs                       [Create] catchup_cursor get/set helpers
    ├── snapshot/
    │   ├── mod.rs                      [Create] SnapshotStore trait + ItemSnapshot type
    │   └── postgres.rs                 [Create] PgSnapshotStore (diff + UPSERT in send_in_tx)
    ├── gh_client/
    │   ├── mod.rs                      [Create] GhClient trait + types (RepoSlug, ProjectItemPage, IssueUpdate, PrUpdate, ReleaseUpdate)
    │   ├── http.rs                     [Create] HttpGhClient (reqwest, Secret<String> token)
    │   ├── graphql.rs                  [Create] ProjectsV2 query string (const, variables-based)
    │   ├── rest.rs                     [Create] REST endpoints (issues since, pulls, releases)
    │   ├── backoff.rs                  [Create] exp_backoff + rate_limit_wait helpers
    │   └── mock.rs                     [Create] MockGhClient for tests
    ├── linkage.rs                      [Create] branch parse + Totsuka-Task trailer extraction
    ├── polling/
    │   ├── mod.rs                      [Create] re-exports + RepoTracker (Arc<RwLock<HashSet<RepoSlug>>>)
    │   ├── project.rs                  [Create] run_project_loop (ProjectsV2 status diff)
    │   ├── issues.rs                   [Create] run_issues_loop (per-repo since-pull)
    │   ├── prs.rs                      [Create] run_prs_loop (per-repo since-pull + linkage)
    │   └── releases.rs                 [Create] run_releases_loop (per-repo)
    ├── lifecycle.rs                    [Create] readyz probes (db + github) + wait_for_signals
    └── listener.rs                     [Create] TCP listener for healthz/readyz/metrics
crates/github-watcher/tests/
├── column_map.rs                       [Create] unit: missing/duplicate display
├── cursor.rs                           [Create] integration: get/set against real DB
├── snapshot.rs                         [Create] integration: diff + UPSERT + same-tx publish
├── linkage.rs                          [Create] unit: branch parse + trailer parse
├── graphql_injection.rs                [Create] regression: malicious project_id in variables.input
├── e2e_project_loop.rs                 [Create] MockGhClient → full diff cycle → bus has status_changed
├── e2e_pr_linkage.rs                   [Create] MockGhClient → PR merged + tasks row → pr_merged_ready with item_id
└── e2e_cursor_resume.rs                [Create] stop loop, restart, cursor honored
```

Workspace edits: add `"crates/github-watcher"` to members.

---

## Tasks

### Task 1: Crate scaffold + bin/lib split

**Files:**
- Create: `crates/github-watcher/Cargo.toml`
- Create: `crates/github-watcher/src/main.rs`
- Create: `crates/github-watcher/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: foundation crates (`totsuka-core`, `totsuka-config`, `totsuka-telemetry`, `totsuka-bus`)
- Produces: `github_watcher::WatcherApp::new(config, clock) -> Self` and `async fn run(self) -> anyhow::Result<()>` (stub)

- [ ] **Step 1: Add to workspace**

In root `Cargo.toml [workspace] members`, append `"crates/github-watcher"` (alphabetical between `crates/agent-adapter` and `crates/orchestrator`).

- [ ] **Step 2: Crate Cargo.toml**

`crates/github-watcher/Cargo.toml`:
```toml
[package]
name = "github-watcher"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "github-watcher"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
totsuka-core      = { path = "../totsuka-core",      version = "0.1.0" }
totsuka-config    = { path = "../totsuka-config",    version = "0.1.0" }
totsuka-telemetry = { path = "../totsuka-telemetry", version = "0.1.0" }
totsuka-bus       = { path = "../totsuka-bus",       version = "0.1.0" }

tokio       = { workspace = true, features = ["rt-multi-thread", "macros", "signal", "fs", "net", "sync", "time"] }
axum        = { workspace = true }
hyper       = { workspace = true }
tower       = { workspace = true }
serde       = { workspace = true }
serde_json  = { workspace = true }
chrono      = { workspace = true }
tracing     = { workspace = true }
anyhow      = { workspace = true }
thiserror   = { workspace = true }
async-trait = { workspace = true }
sqlx        = { workspace = true }
reqwest     = { workspace = true }
regex       = { workspace = true }
tracing-subscriber = { workspace = true }
tokio-util  = { version = "0.7", features = ["rt"] }
hyper-util  = { version = "0.1", features = ["tokio", "server-auto"] }
md5         = "0.7"

[dev-dependencies]
tokio    = { workspace = true, features = ["test-util"] }
tempfile = "3.12"
```

- [ ] **Step 3: lib.rs stub**

`crates/github-watcher/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub struct WatcherApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl WatcherApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("github-watcher stub: nothing to do yet");
        Ok(())
    }
}
```

- [ ] **Step 4: main.rs stub**

`crates/github-watcher/src/main.rs`:
```rust
use std::sync::Arc;

use github_watcher::WatcherApp;
use totsuka_config::Config;
use totsuka_core::SystemClock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(Config::load(&config_path)?);
    tracing_subscriber::fmt().with_env_filter("info").init();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    WatcherApp::new(config, clock).run().await
}
```

- [ ] **Step 5: Verify**

```bash
cargo check --workspace
cargo build -p github-watcher
```
Expected: both succeed with no errors.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): bin/lib scaffold + workspace wire-up"
```

---

### Task 2: WatcherError + RFC7807 mapping

**Files:**
- Create: `crates/github-watcher/src/error.rs`
- Modify: `crates/github-watcher/src/lib.rs` (`pub mod error;`)

**Interfaces:**
- Produces: `pub enum WatcherError` with variants (`Sqlx`, `Bus`, `Http`, `GraphQl`, `RateLimited { reset_at }`, `SchemaOutOfRange { got, min, target }`, `ColumnMap`, `UnknownColumn(String)`, `Internal(String)`) + `code()` returning `/errors/<kind>` + `From` impls for `sqlx::Error` / `totsuka_bus::pgmq::BusError` / `reqwest::Error`.

- [ ] **Step 1: Implement + tests**

`crates/github-watcher/src/error.rs`:
```rust
use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("graphql: {0}")]
    GraphQl(String),
    #[error("rate limited until {reset_at}")]
    RateLimited { reset_at: DateTime<Utc> },
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("column map: {0}")]
    ColumnMap(String),
    #[error("unknown column display: {0}")]
    UnknownColumn(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl WatcherError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Http(_) => "/errors/http",
            Self::GraphQl(_) => "/errors/graphql",
            Self::RateLimited { .. } => "/errors/rate_limited",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::ColumnMap(_) => "/errors/column_map",
            Self::UnknownColumn(_) => "/errors/unknown_column",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn schema_oor_codes() {
        let e = WatcherError::SchemaOutOfRange { got: 3, min: 6, target: 6 };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
    #[test]
    fn rate_limited_codes() {
        let t = Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap();
        let e = WatcherError::RateLimited { reset_at: t };
        assert_eq!(e.code(), "/errors/rate_limited");
    }
    #[test]
    fn unknown_column_codes() {
        assert_eq!(
            WatcherError::UnknownColumn("🚧 ???".into()).code(),
            "/errors/unknown_column"
        );
    }
}
```

- [ ] **Step 2: Wire + run**

Add `pub mod error;` to `crates/github-watcher/src/lib.rs`.

Run:
```bash
cargo test -p github-watcher error::
```
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/github-watcher/src/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): WatcherError + RFC7807 code()"
```

---

### Task 3: Schema-version handshake

**Files:**
- Create: `crates/github-watcher/src/schema_check.rs`
- Create: `crates/github-watcher/tests/schema_check.rs`
- Modify: `crates/github-watcher/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const MIN_SCHEMA_VERSION: i32 = 6;`
  - `pub const TARGET_SCHEMA_VERSION: i32 = 6;`
  - `pub async fn check_schema_version(pool: &PgPool) -> Result<i32, WatcherError>`

- [ ] **Step 1: Implement**

`crates/github-watcher/src/schema_check.rs`:
```rust
//! spec §11.1 bin↔DB handshake. Reads the highest version from schema_meta
//! and validates it against the bin's compiled range.

use crate::error::WatcherError;
use sqlx::PgPool;

pub const MIN_SCHEMA_VERSION: i32 = 6;
pub const TARGET_SCHEMA_VERSION: i32 = 6;

pub async fn check_schema_version(pool: &PgPool) -> Result<i32, WatcherError> {
    let row: (Option<i32>,) = sqlx::query_as("SELECT max(version) FROM schema_meta")
        .fetch_one(pool)
        .await?;
    let got = row.0.ok_or_else(|| {
        WatcherError::Internal("schema_meta is empty; run sqlx migrate".into())
    })?;
    if got < MIN_SCHEMA_VERSION || got > TARGET_SCHEMA_VERSION {
        return Err(WatcherError::SchemaOutOfRange {
            got,
            min: MIN_SCHEMA_VERSION,
            target: TARGET_SCHEMA_VERSION,
        });
    }
    Ok(got)
}
```

`crates/github-watcher/tests/schema_check.rs`:
```rust
use github_watcher::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn returns_target_version_against_migrated_db() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    let v = check_schema_version(&pool).await.unwrap();
    assert_eq!(v, TARGET_SCHEMA_VERSION);
}
```

Add `pub mod schema_check;` to `lib.rs`.

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test schema_check
```
Expected: 1 passed (or 1 ignored if `DATABASE_URL` unset locally).

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): schema_meta version handshake (MIN/TARGET = 6)"
```

---

### Task 4: ColumnMap loader

**Files:**
- Create: `crates/github-watcher/src/column_map.rs`
- Modify: `crates/github-watcher/src/lib.rs`

**Interfaces:**
- Consumes: `totsuka_config::Config` (`config.github.columns: HashMap<ColumnId, String>`)
- Produces:
  - `pub fn build(config: &Config) -> Result<ColumnMap, WatcherError>` — wraps `totsuka_core::ColumnMap::try_new` and converts `ColumnMapError` into `WatcherError::ColumnMap(...)`

- [ ] **Step 1: Failing test**

`crates/github-watcher/src/column_map.rs`:
```rust
//! spec §11.4: build the display-name ↔ ColumnId map from [github].columns.
//! Unknown display names returned by the GitHub API are surfaced as
//! WatcherError::UnknownColumn at resolve time (see polling/project.rs).

use crate::error::WatcherError;
use totsuka_config::Config;
use totsuka_core::{ColumnMap, ColumnMapError};

pub fn build(config: &Config) -> Result<ColumnMap, WatcherError> {
    ColumnMap::try_new(config.github.columns.clone()).map_err(|e: ColumnMapError| {
        WatcherError::ColumnMap(e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_core::ColumnId;

    fn cfg_with(columns: HashMap<ColumnId, String>) -> Config {
        // Minimal Config construction is awkward; assemble TOML and parse.
        let mut toml = String::from(
            r#"
[totsuka]
state_dir = "/tmp/s"
data_dir  = "/tmp/d"

[supervisor]
[supervisor.heartbeat]

[postgres]
image = "x"
container = "x"
host = "127.0.0.1"
port = 5432
database = "totsuka"
user = "postgres"
volume = "/tmp/v"
compose_file = "/tmp/c"

[bus]
queue_name = "q"

[agent_adapter]
uds_path     = "/tmp/a.sock"
herdr_socket = "/tmp/h.sock"
node_capacity = 4
repos_root   = "/tmp/repos"
auto_clone   = false

[orchestrator]
uds_path                    = "/tmp/o.sock"
wip_global                  = 4
phase_timeout_default_secs  = 3600
adapter_uds                 = "/tmp/a.sock"

[orchestrator.claude_argv]

[github]
project_owner  = "acme"
project_number = 1

[github.columns]
"#,
        );
        for (id, name) in &columns {
            toml.push_str(&format!("{} = \"{}\"\n", id.as_snake(), name));
        }
        toml.push_str(
            r#"
[github_watcher]
bind = "127.0.0.1:7802"

[qa_service]
uds_path         = "/tmp/q.sock"
allowed_user_ids = []
catchup_channels = []
reaction_trigger = "memo"
default_mode     = "auto"
adapter_uds      = "/tmp/a.sock"

[qa_service.classifier]
provider  = "anthropic"
model     = "claude-haiku-4-5-20251001"

[qa_service.answer]

[notifications]
[notifications.slack]
[notifications.github]

[retention]
[telemetry]
"#,
        );
        Config::from_toml_str(&toml).unwrap()
    }

    fn full_map() -> HashMap<ColumnId, String> {
        let mut m = HashMap::new();
        m.insert(ColumnId::Inbox,            "📥 Inbox".into());
        m.insert(ColumnId::Ready,            "📋 Ready".into());
        m.insert(ColumnId::Design,           "🤖 調査・設計".into());
        m.insert(ColumnId::DesignReview,     "🚧 設計レビュー".into());
        m.insert(ColumnId::ImplVerify,       "🤖 実装・受入検証".into());
        m.insert(ColumnId::FinalReview,      "🚧 最終レビュー".into());
        m.insert(ColumnId::AwaitingRelease,  "🚀 リリース待ち".into());
        m.insert(ColumnId::Released,         "🏁 完了".into());
        m
    }

    #[test]
    fn build_succeeds_with_full_map() {
        let c = cfg_with(full_map());
        let m = build(&c).unwrap();
        assert_eq!(m.resolve("📥 Inbox"), Some(ColumnId::Inbox));
        assert_eq!(m.resolve("🏁 完了"),  Some(ColumnId::Released));
    }

    #[test]
    fn build_errors_when_a_column_is_missing() {
        let mut partial = full_map();
        partial.remove(&ColumnId::Inbox);
        let c = cfg_with(partial);
        let err = build(&c).unwrap_err();
        assert!(matches!(err, WatcherError::ColumnMap(_)), "got: {err:?}");
    }
}
```

Add `pub mod column_map;` to `lib.rs`.

> The test uses `Config::from_toml_str`. If this helper does not yet exist in `totsuka-config`, add it as a one-liner: `pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> { ... }` mirroring `Config::load`. Inspect `crates/totsuka-config/src/lib.rs` for the existing parse path; the parser is already file-driven so you may need a small shim that takes `&str` and runs the same expand/validate pipeline.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p github-watcher column_map::
```
Expected: 2 passed.

```bash
git add crates/github-watcher/ crates/totsuka-config/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): ColumnMap loader from [github].columns"
```

---

### Task 5: catchup_cursor get/set helpers

**Files:**
- Create: `crates/github-watcher/src/cursor.rs`
- Create: `crates/github-watcher/tests/cursor.rs`
- Modify: `crates/github-watcher/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct CursorKey { source: &'static str, scope: String }` with constructors:
    - `CursorKey::project_items() -> Self` (`source="github", scope="projectv2_items"`)
    - `CursorKey::issues(repo: &str) -> Self` (`scope="issues:{owner}/{repo}"`)
    - `CursorKey::prs(repo: &str) -> Self`
    - `CursorKey::releases(repo: &str) -> Self`
  - `pub async fn get(pool: &PgPool, key: &CursorKey) -> Result<Option<String>, WatcherError>`
  - `pub async fn set(pool: &PgPool, key: &CursorKey, cursor: &str) -> Result<(), WatcherError>`
  - `pub async fn set_in_tx(tx: &mut Transaction<'_, Postgres>, key: &CursorKey, cursor: &str) -> Result<(), WatcherError>` — used in the same-tx publish path

- [ ] **Step 1: Implement**

`crates/github-watcher/src/cursor.rs`:
```rust
//! spec §11.2 + §8.3: catchup_cursor holds per-stream resume points.
//! Updated atomically with snapshot UPSERT + bus publish (use set_in_tx).

use crate::error::WatcherError;
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug, Clone)]
pub struct CursorKey {
    pub source: &'static str,
    pub scope: String,
}

impl CursorKey {
    pub fn project_items() -> Self {
        Self { source: "github", scope: "projectv2_items".into() }
    }
    pub fn issues(repo: &str) -> Self {
        Self { source: "github", scope: format!("issues:{repo}") }
    }
    pub fn prs(repo: &str) -> Self {
        Self { source: "github", scope: format!("prs:{repo}") }
    }
    pub fn releases(repo: &str) -> Self {
        Self { source: "github", scope: format!("releases:{repo}") }
    }
}

pub async fn get(pool: &PgPool, key: &CursorKey) -> Result<Option<String>, WatcherError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT cursor FROM catchup_cursor WHERE source = $1 AND scope = $2",
    )
    .bind(key.source)
    .bind(&key.scope)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn set(pool: &PgPool, key: &CursorKey, cursor: &str) -> Result<(), WatcherError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
            VALUES ($1, $2, $3, now())
            ON CONFLICT (source, scope) DO UPDATE
              SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(key.source)
    .bind(&key.scope)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    key: &CursorKey,
    cursor: &str,
) -> Result<(), WatcherError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
            VALUES ($1, $2, $3, now())
            ON CONFLICT (source, scope) DO UPDATE
              SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(key.source)
    .bind(&key.scope)
    .bind(cursor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
```

Add `pub mod cursor;` to `lib.rs`.

- [ ] **Step 2: Integration test**

`crates/github-watcher/tests/cursor.rs`:
```rust
use github_watcher::cursor::{get, set, set_in_tx, CursorKey};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn round_trip_project_cursor() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let k = CursorKey::project_items();
    set(&pool, &k, "abc").await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("abc".into()));
    set(&pool, &k, "def").await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("def".into()));
}

#[tokio::test]
async fn issues_cursor_is_repo_scoped() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let a = CursorKey::issues("acme/a");
    let b = CursorKey::issues("acme/b");
    set(&pool, &a, "2026-06-01T00:00:00Z").await.unwrap();
    set(&pool, &b, "2026-06-02T00:00:00Z").await.unwrap();
    assert_eq!(get(&pool, &a).await.unwrap(), Some("2026-06-01T00:00:00Z".into()));
    assert_eq!(get(&pool, &b).await.unwrap(), Some("2026-06-02T00:00:00Z".into()));
}

#[tokio::test]
async fn set_in_tx_is_atomic_with_rollback() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let k = CursorKey::prs("acme/tx");
    set(&pool, &k, "baseline").await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    set_in_tx(&mut tx, &k, "should-roll-back").await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(get(&pool, &k).await.unwrap(), Some("baseline".into()));
}
```

- [ ] **Step 3: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test cursor
```
Expected: 3 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): catchup_cursor get/set + set_in_tx helpers"
```

---

### Task 6: SnapshotStore — diff + UPSERT with same-tx publish

**Files:**
- Create: `crates/github-watcher/src/snapshot/mod.rs`
- Create: `crates/github-watcher/src/snapshot/postgres.rs`
- Create: `crates/github-watcher/tests/snapshot.rs`
- Modify: `crates/github-watcher/src/lib.rs`

**Interfaces:**
- Consumes: `totsuka_bus::Publisher::send_in_tx`
- Produces:
  - `pub struct ItemSnapshot { pub item_id: String, pub status: Option<ColumnId>, pub content_ref: Option<String>, pub closed_at: Option<DateTime<Utc>> }`
  - `pub struct Diff { pub item_id: String, pub from_status: Option<ColumnId>, pub to_status: Option<ColumnId>, pub repo: Option<String> }`
  - `pub trait SnapshotStore: Send + Sync { async fn diff_page(&self, page: &[ItemSnapshot]) -> Result<Vec<Diff>, WatcherError>; async fn commit_page(&self, page: &[ItemSnapshot], events: &[(String, DomainEvent)], next_cursor: Option<&str>) -> Result<(), WatcherError>; }`
    - `commit_page` opens one tx; for each `(event_key, DomainEvent)` it calls `publisher.send_in_tx`; UPSERTs each `ItemSnapshot`; calls `set_in_tx` for the project_items cursor when `next_cursor` is `Some`; commits.

- [ ] **Step 1: Implement trait + types**

`crates/github-watcher/src/snapshot/mod.rs`:
```rust
//! spec §8.3: ProjectsV2 snapshot diff. The repository owns the same-tx
//! publish path: every diff row's bus event + every UPSERT + the page's
//! end-cursor update are committed in one transaction (via
//! Publisher::send_in_tx). This is the atomicity guarantee the orchestrator
//! relies on — if any step fails, the next poll re-derives the same diff and
//! the deterministic event_key makes the orchestrator absorb the duplicate.

use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use totsuka_core::{ColumnId, DomainEvent};

pub mod postgres;
pub use postgres::PgSnapshotStore;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemSnapshot {
    pub item_id: String,
    pub status: Option<ColumnId>,
    pub content_ref: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub item_id: String,
    pub from_status: Option<ColumnId>,
    pub to_status: Option<ColumnId>,
    pub repo: Option<String>,
}

#[async_trait]
pub trait SnapshotStore: Send + Sync + 'static {
    async fn diff_page(&self, page: &[ItemSnapshot]) -> Result<Vec<Diff>, WatcherError>;

    async fn commit_page(
        &self,
        page: &[ItemSnapshot],
        events: &[(String /* event_key, currently unused but kept for trace */, DomainEvent)],
        next_cursor: Option<&str>,
    ) -> Result<(), WatcherError>;
}
```

- [ ] **Step 2: PgSnapshotStore impl**

`crates/github-watcher/src/snapshot/postgres.rs`:
```rust
use super::{Diff, ItemSnapshot, SnapshotStore};
use crate::cursor::{set_in_tx, CursorKey};
use crate::error::WatcherError;
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use totsuka_bus::Publisher;
use totsuka_core::{ColumnId, DomainEvent};

pub struct PgSnapshotStore {
    pool: PgPool,
    publisher: Arc<Publisher>,
}

impl PgSnapshotStore {
    pub fn new(pool: PgPool, publisher: Arc<Publisher>) -> Self {
        Self { pool, publisher }
    }
}

fn parse_status(s: Option<String>) -> Option<ColumnId> {
    s.and_then(|raw| serde_json::from_value::<ColumnId>(serde_json::Value::String(raw)).ok())
}

#[async_trait]
impl SnapshotStore for PgSnapshotStore {
    async fn diff_page(&self, page: &[ItemSnapshot]) -> Result<Vec<Diff>, WatcherError> {
        if page.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<String> = page.iter().map(|i| i.item_id.clone()).collect();
        let rows = sqlx::query(
            "SELECT item_id, status FROM gh_item_status WHERE item_id = ANY($1)",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        let mut prev = std::collections::HashMap::<String, Option<ColumnId>>::new();
        for r in rows {
            let id: String = r.get("item_id");
            let s: Option<String> = r.get("status");
            prev.insert(id, parse_status(s));
        }
        let mut out = Vec::with_capacity(page.len());
        for snap in page {
            let prior = prev.get(&snap.item_id).cloned().unwrap_or(None);
            if prior != snap.status {
                let repo = snap.content_ref.as_ref().and_then(|s| s.split('#').next().map(String::from));
                out.push(Diff {
                    item_id: snap.item_id.clone(),
                    from_status: prior,
                    to_status: snap.status,
                    repo,
                });
            }
        }
        Ok(out)
    }

    async fn commit_page(
        &self,
        page: &[ItemSnapshot],
        events: &[(String, DomainEvent)],
        next_cursor: Option<&str>,
    ) -> Result<(), WatcherError> {
        let mut tx = self.pool.begin().await?;
        // 1. Publish every event in the same tx (spec §8.3 atomicity)
        for (_k, ev) in events {
            self.publisher
                .send_in_tx(&mut tx, ev.clone(), None)
                .await?;
        }
        // 2. UPSERT every snapshot row
        for snap in page {
            let status_snake = snap.status.map(|c| c.as_snake().to_string());
            sqlx::query(
                "INSERT INTO gh_item_status (item_id, status, content_ref, closed_at, updated_at)
                    VALUES ($1, $2, $3, $4, now())
                    ON CONFLICT (item_id) DO UPDATE
                      SET status      = EXCLUDED.status,
                          content_ref = EXCLUDED.content_ref,
                          closed_at   = EXCLUDED.closed_at,
                          updated_at  = now()",
            )
            .bind(&snap.item_id)
            .bind(status_snake)
            .bind(snap.content_ref.as_deref())
            .bind(snap.closed_at)
            .execute(&mut *tx)
            .await?;
        }
        // 3. Update the page cursor (if this is the last page, project loop will pass None)
        if let Some(c) = next_cursor {
            set_in_tx(&mut tx, &CursorKey::project_items(), c).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
```

Add `pub mod snapshot;` to `lib.rs`.

- [ ] **Step 3: Integration tests**

`crates/github-watcher/tests/snapshot.rs`:
```rust
use chrono::Utc;
use github_watcher::cursor::{get, CursorKey};
use github_watcher::snapshot::{ItemSnapshot, PgSnapshotStore, SnapshotStore};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_bus::Publisher;
use totsuka_core::{ColumnId, DomainEvent, Source, SystemClock};

async fn pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    Some(PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap())
}

fn unique_queue() -> String {
    format!("ghw_test_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn diff_detects_new_and_changed_items() {
    let Some(pool) = pool().await else { return };
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    // Seed: item A in design
    sqlx::query("INSERT INTO gh_item_status (item_id, status) VALUES ($1, 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_A")
        .execute(&pool).await.unwrap();

    let page = vec![
        ItemSnapshot { item_id: "PVTI_A".into(), status: Some(ColumnId::ImplVerify), content_ref: Some("acme/r#1".into()), closed_at: None },
        ItemSnapshot { item_id: "PVTI_B".into(), status: Some(ColumnId::Ready),     content_ref: Some("acme/r#2".into()), closed_at: None },
    ];
    let diffs = store.diff_page(&page).await.unwrap();
    assert_eq!(diffs.len(), 2);
    let a = diffs.iter().find(|d| d.item_id == "PVTI_A").unwrap();
    assert_eq!(a.from_status, Some(ColumnId::Design));
    assert_eq!(a.to_status,   Some(ColumnId::ImplVerify));
    let b = diffs.iter().find(|d| d.item_id == "PVTI_B").unwrap();
    assert_eq!(b.from_status, None);
    assert_eq!(b.to_status,   Some(ColumnId::Ready));
    assert_eq!(b.repo.as_deref(), Some("acme/r"));
}

#[tokio::test]
async fn commit_page_writes_events_snapshots_and_cursor_atomically() {
    let Some(pool) = pool().await else { return };
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    let page = vec![
        ItemSnapshot { item_id: "PVTI_C".into(), status: Some(ColumnId::Ready),  content_ref: Some("acme/x#9".into()), closed_at: Some(Utc::now()) },
    ];
    let ev = DomainEvent {
        event_key: "gh:status:PVTI_C:abc12345".into(),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({ "item_id": "PVTI_C", "to_status": "ready", "repo": "acme/x" }),
    };
    store.commit_page(&page, &[(ev.event_key.clone(), ev)], Some("endCursor-1")).await.unwrap();

    // Snapshot row was written.
    let row: (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, closed_at FROM gh_item_status WHERE item_id = 'PVTI_C'")
            .fetch_one(&pool).await.unwrap();
    assert_eq!(row.0.as_deref(), Some("ready"));
    assert!(row.1.is_some());

    // Cursor was advanced.
    assert_eq!(get(&pool, &CursorKey::project_items()).await.unwrap(), Some("endCursor-1".into()));

    // The published event sits in pgmq.
    let (_msg_id, env) = totsuka_bus::read_one(&pool, &q, 5).await.unwrap().expect("one envelope");
    assert_eq!(env.event_type, "github.status_changed");
    totsuka_bus::delete(&pool, &q, _msg_id).await.unwrap();
}
```

- [ ] **Step 4: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test snapshot
```
Expected: 2 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): SnapshotStore diff + same-tx commit (publish + UPSERT + cursor)"
```

---

### Task 7: GhClient trait + types + MockGhClient

**Files:**
- Create: `crates/github-watcher/src/gh_client/mod.rs`
- Create: `crates/github-watcher/src/gh_client/mock.rs`
- Modify: `crates/github-watcher/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct RepoSlug { pub owner: String, pub repo: String }` + `Display = "owner/repo"` + `pub fn parse(&str) -> Option<Self>`
  - `pub struct ProjectItemPage { pub items: Vec<ProjectItem>, pub end_cursor: Option<String>, pub has_next: bool }`
  - `pub struct ProjectItem { pub id: String, pub status_display: Option<String>, pub repo: Option<RepoSlug>, pub content_number: Option<u64>, pub closed_at: Option<DateTime<Utc>> }`
  - `pub struct IssueUpdate { pub node_id: String, pub repo: RepoSlug, pub number: u64, pub updated_at: DateTime<Utc>, pub state: String /* "open"|"closed" */ }`
  - `pub struct PrUpdate { pub node_id: String, pub repo: RepoSlug, pub number: u64, pub head_ref: String, pub body: Option<String>, pub merged: bool, pub merged_at: Option<DateTime<Utc>>, pub updated_at: DateTime<Utc> }`
  - `pub struct ReleaseUpdate { pub node_id: String, pub repo: RepoSlug, pub tag: String, pub published_at: DateTime<Utc> }`
  - `#[async_trait] pub trait GhClient: Send + Sync { async fn project_items_page(&self, project_node_id: &str, after: Option<&str>, first: u32) -> Result<ProjectItemPage, WatcherError>; async fn resolve_project_node_id(&self, owner: &str, number: u64) -> Result<String, WatcherError>; async fn issues_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<IssueUpdate>, WatcherError>; async fn prs_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<PrUpdate>, WatcherError>; async fn releases_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<ReleaseUpdate>, WatcherError>; }`
  - `pub struct MockGhClient` with `Mutex<MockState>` fields and constructors:
    - `MockGhClient::new()`
    - `set_project_items_pages(Vec<ProjectItemPage>)` — returned in order
    - `set_issues(repo, Vec<IssueUpdate>)`
    - `set_prs(repo, Vec<PrUpdate>)`
    - `set_releases(repo, Vec<ReleaseUpdate>)`
    - `set_project_node_id(owner, number, node_id)`

- [ ] **Step 1: Implement mod.rs types + trait**

`crates/github-watcher/src/gh_client/mod.rs`:
```rust
//! GitHub HTTPS contract. All shapes carry only what the watcher's
//! downstream pipeline (snapshot diff, PR linkage, event publishing) needs —
//! NOT a 1:1 GraphQL/REST mirror. Keep the surface narrow so MockGhClient is
//! easy to maintain.

use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub mod mock;
pub use mock::MockGhClient;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl RepoSlug {
    pub fn parse(s: &str) -> Option<Self> {
        let (o, r) = s.split_once('/')?;
        if o.is_empty() || r.is_empty() || r.contains('/') {
            return None;
        }
        Some(Self { owner: o.into(), repo: r.into() })
    }
}

impl std::fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectItemPage {
    pub items: Vec<ProjectItem>,
    pub end_cursor: Option<String>,
    pub has_next: bool,
}

#[derive(Debug, Clone)]
pub struct ProjectItem {
    pub id: String,
    pub status_display: Option<String>,
    pub repo: Option<RepoSlug>,
    pub content_number: Option<u64>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct IssueUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub updated_at: DateTime<Utc>,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct PrUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub head_ref: String,
    pub body: Option<String>,
    pub merged: bool,
    pub merged_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ReleaseUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub tag: String,
    pub published_at: DateTime<Utc>,
}

#[async_trait]
pub trait GhClient: Send + Sync + 'static {
    async fn resolve_project_node_id(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<String, WatcherError>;

    async fn project_items_page(
        &self,
        project_node_id: &str,
        after: Option<&str>,
        first: u32,
    ) -> Result<ProjectItemPage, WatcherError>;

    async fn issues_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<IssueUpdate>, WatcherError>;

    async fn prs_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<PrUpdate>, WatcherError>;

    async fn releases_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<ReleaseUpdate>, WatcherError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_slug_round_trip() {
        let s = RepoSlug::parse("acme/widget").unwrap();
        assert_eq!(s.to_string(), "acme/widget");
        assert!(RepoSlug::parse("bad").is_none());
        assert!(RepoSlug::parse("a/b/c").is_none());
        assert!(RepoSlug::parse("/x").is_none());
    }
}
```

- [ ] **Step 2: MockGhClient**

`crates/github-watcher/src/gh_client/mock.rs`:
```rust
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MockState {
    project_node_ids: HashMap<(String, u64), String>,
    project_pages: Vec<ProjectItemPage>,
    project_calls: usize,
    issues: HashMap<RepoSlug, Vec<IssueUpdate>>,
    prs: HashMap<RepoSlug, Vec<PrUpdate>>,
    releases: HashMap<RepoSlug, Vec<ReleaseUpdate>>,
}

pub struct MockGhClient {
    state: Mutex<MockState>,
}

impl Default for MockGhClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockGhClient {
    pub fn new() -> Self {
        Self { state: Mutex::new(MockState::default()) }
    }
    pub fn set_project_node_id(&self, owner: &str, number: u64, node_id: &str) {
        self.state.lock().unwrap().project_node_ids.insert((owner.into(), number), node_id.into());
    }
    pub fn set_project_items_pages(&self, pages: Vec<ProjectItemPage>) {
        let mut s = self.state.lock().unwrap();
        s.project_pages = pages;
        s.project_calls = 0;
    }
    pub fn set_issues(&self, repo: &RepoSlug, list: Vec<IssueUpdate>) {
        self.state.lock().unwrap().issues.insert(repo.clone(), list);
    }
    pub fn set_prs(&self, repo: &RepoSlug, list: Vec<PrUpdate>) {
        self.state.lock().unwrap().prs.insert(repo.clone(), list);
    }
    pub fn set_releases(&self, repo: &RepoSlug, list: Vec<ReleaseUpdate>) {
        self.state.lock().unwrap().releases.insert(repo.clone(), list);
    }
}

#[async_trait]
impl GhClient for MockGhClient {
    async fn resolve_project_node_id(&self, owner: &str, number: u64) -> Result<String, WatcherError> {
        self.state.lock().unwrap().project_node_ids.get(&(owner.into(), number))
            .cloned()
            .ok_or_else(|| WatcherError::Internal(format!("mock has no project for {owner}/{number}")))
    }

    async fn project_items_page(
        &self,
        _project_node_id: &str,
        _after: Option<&str>,
        _first: u32,
    ) -> Result<ProjectItemPage, WatcherError> {
        let mut s = self.state.lock().unwrap();
        if s.project_calls >= s.project_pages.len() {
            // Exhausted: return an empty terminal page so loops can converge.
            return Ok(ProjectItemPage { items: vec![], end_cursor: None, has_next: false });
        }
        let p = s.project_pages[s.project_calls].clone();
        s.project_calls += 1;
        Ok(p)
    }

    async fn issues_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<IssueUpdate>, WatcherError> {
        Ok(self.state.lock().unwrap().issues.get(repo).cloned().unwrap_or_default()
            .into_iter().filter(|u| u.updated_at > since).collect())
    }
    async fn prs_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<PrUpdate>, WatcherError> {
        Ok(self.state.lock().unwrap().prs.get(repo).cloned().unwrap_or_default()
            .into_iter().filter(|u| u.updated_at > since).collect())
    }
    async fn releases_since(&self, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<ReleaseUpdate>, WatcherError> {
        Ok(self.state.lock().unwrap().releases.get(repo).cloned().unwrap_or_default()
            .into_iter().filter(|u| u.published_at > since).collect())
    }
}
```

Add `pub mod gh_client;` to `lib.rs`.

- [ ] **Step 3: Run + commit**

```bash
cargo test -p github-watcher gh_client::
```
Expected: 1 passed (the `repo_slug_round_trip`).

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): GhClient trait + types + MockGhClient"
```

---

### Task 8: GraphQL ProjectsV2 client (variables-based, injection-safe)

**Files:**
- Create: `crates/github-watcher/src/gh_client/http.rs`
- Create: `crates/github-watcher/src/gh_client/graphql.rs`
- Create: `crates/github-watcher/tests/graphql_injection.rs`
- Modify: `crates/github-watcher/src/gh_client/mod.rs` (`pub mod http; pub mod graphql; pub use http::HttpGhClient;`)

**Interfaces:**
- Produces:
  - `pub struct HttpGhClient` with constructors:
    - `pub fn new(token: Secret<String>) -> Self` (defaults: `endpoint_graphql = "https://api.github.com/graphql"`, `endpoint_rest = "https://api.github.com"`)
    - `pub fn with_endpoints(token: Secret<String>, graphql: String, rest: String) -> Self` (tests point this at a stub server)
  - `impl GhClient for HttpGhClient` — Tasks 8/9/10 each fill in one method group; this task implements only `resolve_project_node_id` and `project_items_page`. The remaining methods can `return Err(WatcherError::Internal("not implemented".into()))` placeholders for now.
  - `graphql.rs` exports two `pub const` strings: `PROJECT_NODE_QUERY` (lookup by owner+number) and `PROJECT_ITEMS_QUERY` (page items).

- [ ] **Step 1: GraphQL document strings**

`crates/github-watcher/src/gh_client/graphql.rs`:
```rust
//! GraphQL documents for ProjectsV2 status polling.
//!
//! IMPORTANT: every user-supplied value (project owner, project number,
//! cursor) MUST be passed through GraphQL `variables` — never
//! format!-interpolated into the query string. See orchestrator PR #4 for the
//! reasoning. The regression test `tests/graphql_injection.rs` enforces this.

/// Resolve `(owner, number) -> ProjectV2.node_id`. Owner is either user or org;
/// we try `user(login)` first, fall back to `organization(login)` on miss.
pub const PROJECT_NODE_QUERY_USER: &str = r#"
    query($login: String!, $number: Int!) {
      user(login: $login) {
        projectV2(number: $number) { id }
      }
    }
"#;

pub const PROJECT_NODE_QUERY_ORG: &str = r#"
    query($login: String!, $number: Int!) {
      organization(login: $login) {
        projectV2(number: $number) { id }
      }
    }
"#;

/// Page through ProjectV2 items, extracting the Status single-select value and
/// the issue/PR/DraftIssue content.
pub const PROJECT_ITEMS_QUERY: &str = r#"
    query($projectId: ID!, $first: Int!, $after: String) {
      node(id: $projectId) {
        ... on ProjectV2 {
          items(first: $first, after: $after) {
            pageInfo { hasNextPage endCursor }
            nodes {
              id
              fieldValueByName(name: "Status") {
                ... on ProjectV2ItemFieldSingleSelectValue { name }
              }
              content {
                __typename
                ... on Issue       { number closedAt repository { nameWithOwner } }
                ... on PullRequest { number closedAt repository { nameWithOwner } }
                ... on DraftIssue  { id }
              }
            }
          }
        }
      }
    }
"#;
```

- [ ] **Step 2: HttpGhClient skeleton + project lookups**

`crates/github-watcher/src/gh_client/http.rs`:
```rust
use super::graphql::{PROJECT_ITEMS_QUERY, PROJECT_NODE_QUERY_ORG, PROJECT_NODE_QUERY_USER};
use super::{
    GhClient, IssueUpdate, ProjectItem, ProjectItemPage, PrUpdate, ReleaseUpdate, RepoSlug,
};
use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use totsuka_core::Secret;

pub struct HttpGhClient {
    client: Client,
    token: Secret<String>,
    endpoint_graphql: String,
    endpoint_rest: String,
}

impl HttpGhClient {
    pub fn new(token: Secret<String>) -> Self {
        Self::with_endpoints(
            token,
            "https://api.github.com/graphql".into(),
            "https://api.github.com".into(),
        )
    }

    pub fn with_endpoints(token: Secret<String>, graphql: String, rest: String) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-github-watcher")
                .build()
                .expect("reqwest client"),
            token,
            endpoint_graphql: graphql,
            endpoint_rest: rest,
        }
    }

    async fn graphql(&self, query: &'static str, variables: Value) -> Result<Value, WatcherError> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .client
            .post(&self.endpoint_graphql)
            .bearer_auth(self.token.expose())
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(WatcherError::GraphQl(errors[0].to_string()));
            }
        }
        if !status.is_success() {
            return Err(WatcherError::GraphQl(format!("status={status} body={v}")));
        }
        Ok(v)
    }
}

#[derive(Deserialize)]
struct PageInfoPart {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

#[async_trait]
impl GhClient for HttpGhClient {
    async fn resolve_project_node_id(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<String, WatcherError> {
        let vars = json!({ "login": owner, "number": number });
        // Try user first.
        let v = self.graphql(PROJECT_NODE_QUERY_USER, vars.clone()).await?;
        if let Some(id) = v
            .pointer("/data/user/projectV2/id")
            .and_then(|x| x.as_str())
        {
            return Ok(id.into());
        }
        // Fall back to organization.
        let v = self.graphql(PROJECT_NODE_QUERY_ORG, vars).await?;
        if let Some(id) = v
            .pointer("/data/organization/projectV2/id")
            .and_then(|x| x.as_str())
        {
            return Ok(id.into());
        }
        Err(WatcherError::GraphQl(format!(
            "no ProjectV2 for {owner}/#{number} under user or organization"
        )))
    }

    async fn project_items_page(
        &self,
        project_node_id: &str,
        after: Option<&str>,
        first: u32,
    ) -> Result<ProjectItemPage, WatcherError> {
        let vars = json!({
            "projectId": project_node_id,
            "first": first,
            "after": after,
        });
        let v = self.graphql(PROJECT_ITEMS_QUERY, vars).await?;
        let items_node = v.pointer("/data/node/items").ok_or_else(|| {
            WatcherError::GraphQl("missing data.node.items".into())
        })?;
        let pi: PageInfoPart = serde_json::from_value(
            items_node.get("pageInfo").cloned().unwrap_or(Value::Null),
        )
        .map_err(|e| WatcherError::GraphQl(format!("pageInfo: {e}")))?;
        let nodes = items_node.get("nodes").and_then(|n| n.as_array()).cloned().unwrap_or_default();
        let mut items = Vec::with_capacity(nodes.len());
        for n in nodes {
            let id = n
                .get("id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| WatcherError::GraphQl("item missing id".into()))?
                .to_string();
            let status_display = n
                .pointer("/fieldValueByName/name")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let content = n.get("content").cloned().unwrap_or(Value::Null);
            let repo = content
                .pointer("/repository/nameWithOwner")
                .and_then(|x| x.as_str())
                .and_then(RepoSlug::parse);
            let content_number = content
                .get("number")
                .and_then(|x| x.as_u64());
            let closed_at = content
                .get("closedAt")
                .and_then(|x| x.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc)));
            items.push(ProjectItem {
                id,
                status_display,
                repo,
                content_number,
                closed_at,
            });
        }
        Ok(ProjectItemPage {
            items,
            end_cursor: pi.end_cursor,
            has_next: pi.has_next_page,
        })
    }

    // The remaining methods are filled in by Tasks 9 / 10.
    async fn issues_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<IssueUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::issues_since not yet implemented (Task 9)".into(),
        ))
    }
    async fn prs_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<PrUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::prs_since not yet implemented (Task 10)".into(),
        ))
    }
    async fn releases_since(
        &self,
        _repo: &RepoSlug,
        _since: DateTime<Utc>,
    ) -> Result<Vec<ReleaseUpdate>, WatcherError> {
        Err(WatcherError::Internal(
            "HttpGhClient::releases_since not yet implemented (Task 10)".into(),
        ))
    }
}
```

Wire in `crates/github-watcher/src/gh_client/mod.rs`:
```rust
pub mod graphql;
pub mod http;
pub use http::HttpGhClient;
```

- [ ] **Step 3: GraphQL injection regression test**

`crates/github-watcher/tests/graphql_injection.rs`:
```rust
//! Regression: malicious project_node_id / cursor must land in
//! `variables.input`, never in the `query` string. Same shape as
//! crates/orchestrator/src/gh_writeback/http.rs after PR #4.

use github_watcher::gh_client::{GhClient, HttpGhClient};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

#[tokio::test]
async fn malicious_project_id_lands_in_variables_not_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" { break; }
            if let Some(v) = line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")) {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).await.unwrap();
        stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 31\r\n\r\n\
              {\"data\":{\"node\":{\"items\":{}}}}",
        ).await.unwrap();
        buf
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );

    let evil = r#""}}, fakeField: 1, x:"#;
    // Will return GraphQl error because the fake server's response shape is incomplete,
    // but that's fine — we only care about the WIRE BODY this method emits.
    let _ = client.project_items_page(evil, Some(evil), 100).await;

    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let q = body["query"].as_str().expect("query field present");
    assert!(!q.contains("fakeField"), "query string was contaminated: {q}");
    assert!(!q.contains(evil), "query string echoed evil verbatim: {q}");
    assert_eq!(body["variables"]["projectId"], evil);
    assert_eq!(body["variables"]["after"],     evil);
    assert_eq!(body["variables"]["first"],     100);
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p github-watcher --test graphql_injection
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): GraphQL ProjectsV2 client (variables-based) + injection regression"
```

---

### Task 9: REST issues since-pull

**Files:**
- Create: `crates/github-watcher/src/gh_client/rest.rs`
- Modify: `crates/github-watcher/src/gh_client/http.rs` (replace the `issues_since` stub; wire to `rest::list_issues`)
- Modify: `crates/github-watcher/src/gh_client/mod.rs` (`pub mod rest;`)

**Interfaces:**
- Produces:
  - `pub(crate) async fn list_issues(client: &reqwest::Client, endpoint_rest: &str, token: &Secret<String>, repo: &RepoSlug, since: DateTime<Utc>) -> Result<Vec<IssueUpdate>, WatcherError>`
  - Uses GitHub REST: `GET {endpoint_rest}/repos/{owner}/{repo}/issues?since={iso8601}&state=all&per_page=100&page=N` with pagination via `Link: rel="next"` header. Filters out PRs (`pull_request` field present) — those go through Task 10.

- [ ] **Step 1: Implement rest.rs**

`crates/github-watcher/src/gh_client/rest.rs`:
```rust
use super::{IssueUpdate, PrUpdate, ReleaseUpdate, RepoSlug};
use crate::error::WatcherError;
use chrono::{DateTime, Utc};
use reqwest::{header, Client, Response};
use serde::Deserialize;
use totsuka_core::Secret;

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn next_link(resp: &Response) -> Option<String> {
    let link = resp.headers().get(header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let part = part.trim();
        if part.ends_with("rel=\"next\"") {
            let lt = part.find('<')?;
            let gt = part.find('>')?;
            return Some(part[lt + 1..gt].to_string());
        }
    }
    None
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &Client,
    url: &str,
    token: &Secret<String>,
) -> Result<(T, Option<String>), WatcherError> {
    let resp = client
        .get(url)
        .bearer_auth(token.expose())
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(WatcherError::Internal(format!("REST {s}: {body}")));
    }
    let next = next_link(&resp);
    let body: T = resp.json().await?;
    Ok((body, next))
}

#[derive(Deserialize)]
struct IssueRow {
    node_id: String,
    number: u64,
    state: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>, // present iff this row is actually a PR
}

pub(crate) async fn list_issues(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<IssueUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/issues?since={}&state=all&per_page=100",
        repo.owner,
        repo.repo,
        rfc3339(since),
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<IssueRow>, _) = get_json(client, &url, token).await?;
        for r in rows {
            if r.pull_request.is_some() { continue; }
            out.push(IssueUpdate {
                node_id: r.node_id,
                repo: repo.clone(),
                number: r.number,
                updated_at: r.updated_at,
                state: r.state,
            });
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

// Tasks 10 fills these in.
pub(crate) async fn list_prs(
    _client: &Client,
    _endpoint_rest: &str,
    _token: &Secret<String>,
    _repo: &RepoSlug,
    _since: DateTime<Utc>,
) -> Result<Vec<PrUpdate>, WatcherError> {
    Err(WatcherError::Internal("rest::list_prs not yet implemented (Task 10)".into()))
}

pub(crate) async fn list_releases(
    _client: &Client,
    _endpoint_rest: &str,
    _token: &Secret<String>,
    _repo: &RepoSlug,
    _since: DateTime<Utc>,
) -> Result<Vec<ReleaseUpdate>, WatcherError> {
    Err(WatcherError::Internal("rest::list_releases not yet implemented (Task 10)".into()))
}
```

- [ ] **Step 2: Wire HttpGhClient::issues_since**

In `crates/github-watcher/src/gh_client/http.rs`, replace the `issues_since` placeholder:
```rust
async fn issues_since(
    &self,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<IssueUpdate>, WatcherError> {
    super::rest::list_issues(&self.client, &self.endpoint_rest, &self.token, repo, since).await
}
```

Add at the top:
```rust
// already in scope via `super::rest`
```

Add `pub mod rest;` to `crates/github-watcher/src/gh_client/mod.rs`.

- [ ] **Step 3: Integration test against a tiny REST stub**

Append to `crates/github-watcher/tests/graphql_injection.rs` (or new file `tests/rest_issues.rs`):

`crates/github-watcher/tests/rest_issues.rs`:
```rust
use chrono::{TimeZone, Utc};
use github_watcher::gh_client::{GhClient, HttpGhClient, RepoSlug};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const PAYLOAD: &str = r#"[
  {"node_id":"I_a","number":1,"state":"open","updated_at":"2026-06-29T01:00:00Z"},
  {"node_id":"I_b","number":2,"state":"closed","updated_at":"2026-06-29T02:00:00Z","pull_request":{"url":"x"}},
  {"node_id":"I_c","number":3,"state":"open","updated_at":"2026-06-29T03:00:00Z"}
]"#;

#[tokio::test]
async fn issues_since_filters_out_pull_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" { break; }
        }
        let body = PAYLOAD;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );
    let repo = RepoSlug::parse("acme/widget").unwrap();
    let since = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let issues = client.issues_since(&repo, since).await.unwrap();
    server.await.unwrap();

    let ids: Vec<&str> = issues.iter().map(|i| i.node_id.as_str()).collect();
    assert_eq!(ids, vec!["I_a", "I_c"]); // I_b is a PR
    assert_eq!(issues[0].number, 1);
    assert_eq!(issues[0].state, "open");
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p github-watcher --test rest_issues
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): REST issues since-pull (PRs filtered out)"
```

---

### Task 10: REST PR + release pulls

**Files:**
- Modify: `crates/github-watcher/src/gh_client/rest.rs` (fill in `list_prs`, `list_releases`)
- Modify: `crates/github-watcher/src/gh_client/http.rs` (wire `prs_since`, `releases_since`)
- Create: `crates/github-watcher/tests/rest_prs.rs`

**Interfaces:**
- Produces filled `list_prs` and `list_releases`. PR endpoint: `GET /repos/{o}/{r}/pulls?state=all&sort=updated&direction=desc&per_page=100`; client-side filter `updated_at > since`. Release endpoint: `GET /repos/{o}/{r}/releases?per_page=100`; client-side filter `published_at > since`.

- [ ] **Step 1: list_prs**

Replace the stub in `crates/github-watcher/src/gh_client/rest.rs`:
```rust
#[derive(Deserialize)]
struct PrRow {
    node_id: String,
    number: u64,
    head: PrHead,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    merged_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct PrHead {
    #[serde(rename = "ref")]
    ref_: String,
}

pub(crate) async fn list_prs(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<PrUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/pulls?state=all&sort=updated&direction=desc&per_page=100",
        repo.owner, repo.repo,
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<PrRow>, _) = get_json(client, &url, token).await?;
        for r in &rows {
            if r.updated_at <= since { continue; }
            out.push(PrUpdate {
                node_id: r.node_id.clone(),
                repo: repo.clone(),
                number: r.number,
                head_ref: r.head.ref_.clone(),
                body: r.body.clone(),
                merged: r.merged_at.is_some(),
                merged_at: r.merged_at,
                updated_at: r.updated_at,
            });
        }
        // Early exit: descending sort means the first page already past `since` ends iteration
        let saw_old = rows.iter().any(|r| r.updated_at <= since);
        match next {
            Some(n) if !saw_old => url = n,
            _ => break,
        }
    }
    Ok(out)
}
```

- [ ] **Step 2: list_releases**

Append:
```rust
#[derive(Deserialize)]
struct ReleaseRow {
    node_id: String,
    tag_name: String,
    published_at: Option<DateTime<Utc>>,
}

pub(crate) async fn list_releases(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<ReleaseUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/releases?per_page=100",
        repo.owner, repo.repo,
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<ReleaseRow>, _) = get_json(client, &url, token).await?;
        for r in rows {
            let Some(p) = r.published_at else { continue }; // draft
            if p <= since { continue; }
            out.push(ReleaseUpdate {
                node_id: r.node_id,
                repo: repo.clone(),
                tag: r.tag_name,
                published_at: p,
            });
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}
```

Replace the old `list_prs` / `list_releases` stubs at the bottom of `rest.rs`.

- [ ] **Step 3: Wire HttpGhClient::prs_since / releases_since**

In `crates/github-watcher/src/gh_client/http.rs`, replace the two placeholders:
```rust
async fn prs_since(
    &self,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<PrUpdate>, WatcherError> {
    super::rest::list_prs(&self.client, &self.endpoint_rest, &self.token, repo, since).await
}

async fn releases_since(
    &self,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<ReleaseUpdate>, WatcherError> {
    super::rest::list_releases(&self.client, &self.endpoint_rest, &self.token, repo, since).await
}
```

- [ ] **Step 4: PR test**

`crates/github-watcher/tests/rest_prs.rs`:
```rust
use chrono::{TimeZone, Utc};
use github_watcher::gh_client::{GhClient, HttpGhClient, RepoSlug};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const PAYLOAD: &str = r#"[
  {"node_id":"PR_a","number":11,"head":{"ref":"totsuka/abc123def456/implv"},"body":"hello\n\nTotsuka-Task: PVTI_full_abc123def456","merged_at":"2026-06-29T05:00:00Z","updated_at":"2026-06-29T05:00:00Z"},
  {"node_id":"PR_b","number":12,"head":{"ref":"totsuka/xyz999/design"},"body":null,"updated_at":"2026-06-28T00:00:00Z"}
]"#;

#[tokio::test]
async fn prs_since_filters_old_updates() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut l = String::new();
            let n = reader.read_line(&mut l).await.unwrap();
            if n == 0 || l == "\r\n" { break; }
        }
        let body = PAYLOAD;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );
    let repo = RepoSlug::parse("acme/widget").unwrap();
    let since = Utc.with_ymd_and_hms(2026, 6, 29, 0, 0, 0).unwrap();
    let prs = client.prs_since(&repo, since).await.unwrap();
    server.await.unwrap();

    assert_eq!(prs.len(), 1);
    let pr = &prs[0];
    assert_eq!(pr.number, 11);
    assert!(pr.merged);
    assert_eq!(pr.head_ref, "totsuka/abc123def456/implv");
    assert!(pr.body.as_ref().unwrap().contains("Totsuka-Task: PVTI_full_abc123def456"));
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p github-watcher --test rest_prs
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): REST PR + release pulls"
```

---

### Task 11: Rate-limit / 5xx backoff wrapper

**Files:**
- Create: `crates/github-watcher/src/gh_client/backoff.rs`
- Modify: `crates/github-watcher/src/gh_client/mod.rs` (`pub mod backoff;`)

**Interfaces:**
- Produces:
  - `pub async fn with_retry<T, F, Fut>(clock: Arc<dyn Clock>, max_attempts: u32, mut op: F) -> Result<T, WatcherError> where F: FnMut() -> Fut, Fut: Future<Output = Result<T, WatcherError>>` — retries `WatcherError::Http(_)` 5xx and `WatcherError::RateLimited{..}` up to `max_attempts`, exp backoff (1s, 4s, 16s, cap 30s); on `RateLimited` sleeps until `reset_at` (capped at 30s, then re-checks).
  - `pub(crate) fn classify_http(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap, now: DateTime<Utc>) -> Option<WatcherError>` — returns `Some(RateLimited{..})` for 403 with `X-RateLimit-Remaining: 0`, `Some(Internal(..))` for 5xx, `None` for success/non-retryable.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/gh_client/backoff.rs`:
```rust
//! Centralised retry / rate-limit logic. Wrap any GhClient call that talks to
//! GitHub through `with_retry` so individual call sites stay clean.

use crate::error::WatcherError;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::{header, StatusCode};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

pub fn classify_http(
    status: StatusCode,
    headers: &header::HeaderMap,
    now: DateTime<Utc>,
) -> Option<WatcherError> {
    if status.is_success() {
        return None;
    }
    if status == StatusCode::FORBIDDEN
        && headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            == Some("0")
    {
        let reset_at = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
            .unwrap_or(now + chrono::Duration::seconds(30));
        return Some(WatcherError::RateLimited { reset_at });
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let secs = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(5);
        return Some(WatcherError::RateLimited {
            reset_at: now + chrono::Duration::seconds(secs),
        });
    }
    if status.is_server_error() {
        return Some(WatcherError::Internal(format!("REST {status}")));
    }
    Some(WatcherError::Internal(format!("REST {status}")))
}

pub async fn with_retry<T, F, Fut>(
    clock: Arc<dyn Clock>,
    max_attempts: u32,
    mut op: F,
) -> Result<T, WatcherError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, WatcherError>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(WatcherError::RateLimited { reset_at }) => {
                let now = clock.now();
                let wait = (reset_at - now).num_seconds().max(0).min(30) as u64;
                tracing::warn!(reset_at=%reset_at, "rate-limited; sleeping {wait}s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                if attempt >= max_attempts {
                    return Err(WatcherError::RateLimited { reset_at });
                }
            }
            Err(e) if attempt < max_attempts && is_retryable(&e) => {
                let backoff = backoff_secs(attempt);
                tracing::warn!(error=%e, "retrying in {backoff}s (attempt {attempt})");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_retryable(e: &WatcherError) -> bool {
    matches!(e, WatcherError::Http(_)) || matches!(e, WatcherError::Internal(s) if s.starts_with("REST 5"))
}

fn backoff_secs(attempt: u32) -> u64 {
    // 1, 4, 16, cap 30
    let s = 4u64.saturating_pow(attempt.saturating_sub(1));
    s.min(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use reqwest::header::HeaderMap;
    use totsuka_core::SystemClock;

    #[test]
    fn classify_rate_limited_403() {
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        h.insert("x-ratelimit-reset",     "1762000000".parse().unwrap());
        let now = Utc.with_ymd_and_hms(2026, 6, 28, 0, 0, 0).unwrap();
        let e = classify_http(StatusCode::FORBIDDEN, &h, now).unwrap();
        assert!(matches!(e, WatcherError::RateLimited { .. }));
    }

    #[test]
    fn classify_5xx_internal() {
        let h = HeaderMap::new();
        let now = Utc.with_ymd_and_hms(2026, 6, 28, 0, 0, 0).unwrap();
        let e = classify_http(StatusCode::INTERNAL_SERVER_ERROR, &h, now).unwrap();
        assert!(matches!(e, WatcherError::Internal(_)));
    }

    #[tokio::test]
    async fn with_retry_succeeds_after_one_500() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let r = with_retry(Arc::new(SystemClock), 3, || async {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Err(WatcherError::Internal("REST 503".into()))
            } else {
                Ok(42u32)
            }
        })
        .await
        .unwrap();
        assert_eq!(r, 42);
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p github-watcher gh_client::backoff
```
Expected: 3 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): retry + rate-limit wrapper"
```

---

### Task 12: RepoTracker — shared set of repos derived from project items

**Files:**
- Create: `crates/github-watcher/src/polling/mod.rs`
- Modify: `crates/github-watcher/src/lib.rs` (`pub mod polling;`)

**Interfaces:**
- Produces:
  - `pub struct RepoTracker { inner: Arc<RwLock<HashSet<RepoSlug>>> }` (`Default`, `Clone`)
  - `pub async fn insert(&self, repo: RepoSlug)`
  - `pub async fn snapshot(&self) -> Vec<RepoSlug>`
  - `pub async fn known(&self, repo: &RepoSlug) -> bool`

- [ ] **Step 1: Implement**

`crates/github-watcher/src/polling/mod.rs`:
```rust
//! Per-poller modules + a shared RepoTracker that the project poller updates
//! as it observes ProjectsV2 items, and the issues/PRs/releases pollers read
//! to know which repos to scan.

use crate::gh_client::RepoSlug;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod project;
pub mod issues;
pub mod prs;
pub mod releases;

#[derive(Default, Clone)]
pub struct RepoTracker {
    inner: Arc<RwLock<HashSet<RepoSlug>>>,
}

impl RepoTracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn insert(&self, repo: RepoSlug) {
        self.inner.write().await.insert(repo);
    }
    pub async fn snapshot(&self) -> Vec<RepoSlug> {
        self.inner.read().await.iter().cloned().collect()
    }
    pub async fn known(&self, repo: &RepoSlug) -> bool {
        self.inner.read().await.contains(repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tracker_collects_and_dedupes() {
        let t = RepoTracker::new();
        t.insert(RepoSlug::parse("a/x").unwrap()).await;
        t.insert(RepoSlug::parse("a/x").unwrap()).await;
        t.insert(RepoSlug::parse("a/y").unwrap()).await;
        let mut got = t.snapshot().await;
        got.sort_by(|a, b| a.repo.cmp(&b.repo));
        assert_eq!(got.len(), 2);
        assert!(t.known(&RepoSlug::parse("a/x").unwrap()).await);
        assert!(!t.known(&RepoSlug::parse("a/z").unwrap()).await);
    }
}
```

Create empty stubs `project.rs`, `issues.rs`, `prs.rs`, `releases.rs` (single `pub fn placeholder() {}` is fine — Tasks 13/15/16/17/18 fill them in).

- [ ] **Step 2: Run + commit**

```bash
cargo test -p github-watcher polling::
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): RepoTracker shared state + polling module scaffold"
```

---

### Task 13: Project polling loop — same-tx publish + UPSERT + cursor

**Files:**
- Create: `crates/github-watcher/src/polling/project.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub async fn run_project_loop(client: Arc<dyn GhClient>, store: Arc<dyn SnapshotStore>, column_map: Arc<ColumnMap>, tracker: RepoTracker, clock: Arc<dyn Clock>, health: HealthState, cfg: ProjectLoopConfig, shutdown: CancellationToken) -> Result<(), WatcherError>`
  - `pub struct ProjectLoopConfig { pub project_node_id: String, pub page_size: u32, pub poll_interval: Duration }`
  - Each tick: resume from `CursorKey::project_items()` (if any); loop pages; for each page, run `store.diff_page` → build `(event_key, DomainEvent)` per diff → `store.commit_page` with the page's `end_cursor`; insert observed repos into the tracker. After the last page (`has_next = false`), clear the cursor by writing `""` (next tick starts from the beginning).
  - `to_status_hash`: `format!("{:x}", md5::compute(to_snake.as_bytes()))[..8]` (first 8 hex). Use `totsuka_core::event_key::event_key_gh_status`.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/polling/project.rs`:
```rust
use super::RepoTracker;
use crate::column_map::build as _build; // unused but documents the source
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::{GhClient, ProjectItem};
use crate::snapshot::{ItemSnapshot, SnapshotStore};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use totsuka_core::{event_key_gh_status, Clock, ColumnId, ColumnMap, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct ProjectLoopConfig {
    pub project_node_id: String,
    pub page_size: u32,
    pub poll_interval: Duration,
}

pub async fn run_project_loop(
    pool: PgPool,
    client: Arc<dyn GhClient>,
    store: Arc<dyn SnapshotStore>,
    column_map: Arc<ColumnMap>,
    tracker: RepoTracker,
    _clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: ProjectLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if let Err(e) = run_one_pass(&pool, &client, &store, &column_map, &tracker, &cfg).await {
                    tracing::error!(error=%e, "project loop tick failed");
                    health.set_check("github", &format!("fail: {e}")).await;
                } else {
                    health.set_check("github", "ok").await;
                }
            }
        }
    }
}

async fn run_one_pass(
    pool: &PgPool,
    client: &Arc<dyn GhClient>,
    store: &Arc<dyn SnapshotStore>,
    column_map: &Arc<ColumnMap>,
    tracker: &RepoTracker,
    cfg: &ProjectLoopConfig,
) -> Result<(), WatcherError> {
    let mut after: Option<String> = get(pool, &CursorKey::project_items()).await?;
    if after.as_deref() == Some("") { after = None; }
    loop {
        let page = client
            .project_items_page(&cfg.project_node_id, after.as_deref(), cfg.page_size)
            .await?;
        // 1. Translate ProjectItem → ItemSnapshot
        let mut snapshots = Vec::with_capacity(page.items.len());
        let mut item_repos = Vec::with_capacity(page.items.len());
        for it in &page.items {
            let status: Option<ColumnId> = match &it.status_display {
                None => None,
                Some(display) => match column_map.resolve(display) {
                    Some(c) => Some(c),
                    None => return Err(WatcherError::UnknownColumn(display.clone())),
                },
            };
            let content_ref = it.repo.as_ref().zip(it.content_number)
                .map(|(r, n)| format!("{r}#{n}"));
            snapshots.push(ItemSnapshot {
                item_id: it.id.clone(),
                status,
                content_ref,
                closed_at: it.closed_at,
            });
            if let Some(r) = &it.repo { item_repos.push(r.clone()); }
        }
        // 2. Diff against current snapshot
        let diffs = store.diff_page(&snapshots).await?;
        // 3. Build events
        let mut events: Vec<(String, DomainEvent)> = Vec::with_capacity(diffs.len());
        for d in &diffs {
            let Some(to) = d.to_status else { continue }; // skip transitions to "no status"
            let snake = to.as_snake();
            let hash_full = format!("{:x}", md5::compute(snake.as_bytes()));
            let hash = &hash_full[..8];
            let key = event_key_gh_status(&d.item_id, hash);
            let ev = DomainEvent {
                event_key: key.clone(),
                source: Source::Github,
                event_type: "github.status_changed".into(),
                payload: serde_json::json!({
                    "item_id": d.item_id,
                    "to_status": snake,
                    "repo": d.repo.clone().unwrap_or_default(),
                }),
            };
            events.push((key, ev));
        }
        // 4. Atomic commit (events + UPSERTs + cursor in one tx)
        store.commit_page(&snapshots, &events, page.end_cursor.as_deref()).await?;
        // 5. RepoTracker bookkeeping (after commit — we don't want to leak repos on failure)
        for r in item_repos { tracker.insert(r).await; }

        if !page.has_next { break; }
        after = page.end_cursor;
    }
    // Reset cursor so next tick walks from page 1 (ProjectsV2 has no since;
    // the snapshot/diff layer absorbs the no-op cost via deterministic event_key).
    set(pool, &CursorKey::project_items(), "").await?;
    Ok(())
}
```

- [ ] **Step 2: Build check**

```bash
cargo check -p github-watcher
```

- [ ] **Step 3: Commit**

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): project polling loop (same-tx publish + UPSERT + cursor)"
```

---

### Task 14: PR ↔ task linkage (branch parse + Totsuka-Task trailer)

**Files:**
- Create: `crates/github-watcher/src/linkage.rs`
- Create: `crates/github-watcher/tests/linkage.rs`
- Modify: `crates/github-watcher/src/lib.rs` (`pub mod linkage;`)

**Interfaces:**
- Produces:
  - `pub fn task_id_short_from_branch(branch: &str) -> Option<String>` — extracts `task_id_short` from `totsuka/{task_id_short}/{phase_short}`; returns None for malformed branches.
  - `pub fn task_id_from_trailer(body: &str) -> Option<String>` — finds the **last** line matching `^Totsuka-Task: (\S+)\s*$`; if multiple, the last wins (spec §11.14: trailer is the human-overridable source of truth).
  - `pub async fn resolve_task_id(pool: &PgPool, branch: &str, body: Option<&str>) -> Result<Option<String>, WatcherError>` — primary: branch's `task_id_short` → SELECT `tasks.id`; fallback: trailer → SELECT `tasks.id WHERE id = $1`. If both yield different `task_id`, warn-log and prefer the trailer.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/linkage.rs`:
```rust
//! spec §11.14: a PR is linked to a task by either its branch name or a
//! `Totsuka-Task:` trailer in its body. Both are consulted; trailer wins on
//! mismatch because humans can rename branches but trailers come from the
//! Claude system prompt.

use crate::error::WatcherError;
use regex::Regex;
use sqlx::PgPool;
use std::sync::OnceLock;

pub fn task_id_short_from_branch(branch: &str) -> Option<String> {
    let mut parts = branch.split('/');
    if parts.next() != Some("totsuka") { return None; }
    let short = parts.next()?;
    let _phase = parts.next()?;
    if parts.next().is_some() { return None; } // exactly 3 segments
    if short.is_empty() { return None; }
    Some(short.to_string())
}

fn trailer_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^Totsuka-Task:\s+(\S+)\s*$").unwrap())
}

pub fn task_id_from_trailer(body: &str) -> Option<String> {
    trailer_re().captures_iter(body).last().map(|c| c[1].to_string())
}

pub async fn resolve_task_id(
    pool: &PgPool,
    branch: &str,
    body: Option<&str>,
) -> Result<Option<String>, WatcherError> {
    let by_branch = if let Some(short) = task_id_short_from_branch(branch) {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM tasks WHERE task_id_short = $1",
        )
        .bind(&short)
        .fetch_optional(pool)
        .await?;
        row.map(|r| r.0)
    } else {
        None
    };
    let by_trailer = if let Some(b) = body {
        if let Some(tid) = task_id_from_trailer(b) {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM tasks WHERE id = $1",
            )
            .bind(&tid)
            .fetch_optional(pool)
            .await?;
            row.map(|r| r.0)
        } else { None }
    } else { None };

    match (by_branch, by_trailer) {
        (Some(b), Some(t)) if b != t => {
            tracing::warn!(branch_task=%b, trailer_task=%t, "PR linkage mismatch; preferring trailer");
            Ok(Some(t))
        }
        (_, Some(t)) => Ok(Some(t)),
        (Some(b), None) => Ok(Some(b)),
        (None, None) => Ok(None),
    }
}
```

- [ ] **Step 2: Unit + integration tests**

`crates/github-watcher/tests/linkage.rs`:
```rust
use github_watcher::linkage::{resolve_task_id, task_id_from_trailer, task_id_short_from_branch};
use sqlx::postgres::PgPoolOptions;

#[test]
fn branch_extracts_short() {
    assert_eq!(
        task_id_short_from_branch("totsuka/abc123def456/implv").as_deref(),
        Some("abc123def456"),
    );
}

#[test]
fn branch_rejects_malformed() {
    assert!(task_id_short_from_branch("feature/foo").is_none());
    assert!(task_id_short_from_branch("totsuka//implv").is_none());
    assert!(task_id_short_from_branch("totsuka/abc").is_none());
    assert!(task_id_short_from_branch("totsuka/abc/implv/extra").is_none());
}

#[test]
fn trailer_picks_last() {
    let body = "intro\n\nTotsuka-Task: PVTI_first\n\nmore\nTotsuka-Task: PVTI_last\n";
    assert_eq!(task_id_from_trailer(body).as_deref(), Some("PVTI_last"));
}

#[test]
fn trailer_no_match_returns_none() {
    assert!(task_id_from_trailer("hello world").is_none());
}

#[tokio::test]
async fn resolve_prefers_trailer_on_mismatch() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    // seed two tasks
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_full_xxxxxxxxxxxx").bind("xxxxxxxxxxxx")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_full_yyyyyyyyyyyy").bind("yyyyyyyyyyyy")
        .execute(&pool).await.unwrap();

    let r = resolve_task_id(
        &pool,
        "totsuka/xxxxxxxxxxxx/implv",
        Some("Totsuka-Task: PVTI_full_yyyyyyyyyyyy\n"),
    ).await.unwrap();
    assert_eq!(r.as_deref(), Some("PVTI_full_yyyyyyyyyyyy"));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p github-watcher --test linkage
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test linkage
```
Expected: 4 unit + 1 integration = 5 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): PR↔task linkage (branch parse + Totsuka-Task trailer)"
```

---

### Task 15: PR polling loop (merged → pr_merged_ready, verifications → passed/failed)

**Files:**
- Create: `crates/github-watcher/src/polling/prs.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub async fn run_prs_loop(pool: PgPool, publisher: Arc<Publisher>, client: Arc<dyn GhClient>, tracker: RepoTracker, clock: Arc<dyn Clock>, health: HealthState, cfg: PrsLoopConfig, shutdown: CancellationToken) -> Result<(), WatcherError>`
  - `pub struct PrsLoopConfig { pub poll_interval: Duration, pub catchup_window: chrono::Duration }`
  - For each tracker-known repo, resume cursor from `CursorKey::prs(repo)`, default = `now - catchup_window`. Query PRs updated since. For each PR:
    - resolve task_id via `linkage::resolve_task_id(pool, head_ref, body.as_deref())`
    - publish `github.pr_merged_ready` if `pr.merged && pr.merged_at > since`
    - (Future) verification events come from check-runs; for now emit none — orchestrator's verification events are pushed by a different path. Leave a TODO comment but DO NOT add stub events.
  - Advance cursor to `max(updated_at)` only after successful publish (per-repo).
  - PR events use a non-tx `Publisher::send` (no snapshot to atomically link with).

- [ ] **Step 1: Implement**

`crates/github-watcher/src/polling/prs.rs`:
```rust
use super::RepoTracker;
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::{GhClient, PrUpdate};
use crate::linkage::resolve_task_id;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::Publisher;
use totsuka_core::{Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct PrsLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

pub async fn run_prs_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: PrsLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                for repo in tracker.snapshot().await {
                    if let Err(e) = poll_repo(&pool, &publisher, &client, &repo, &cfg, &clock).await {
                        tracing::warn!(repo=%repo, error=%e, "prs poll failed");
                        health.set_check("github_prs", &format!("fail: {e}")).await;
                    }
                }
            }
        }
    }
}

async fn poll_repo(
    pool: &PgPool,
    publisher: &Publisher,
    client: &Arc<dyn GhClient>,
    repo: &crate::gh_client::RepoSlug,
    cfg: &PrsLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::prs(&repo.to_string());
    let since = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad pr cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let prs = client.prs_since(repo, since).await?;
    let mut high_water = since;
    for pr in prs {
        if let Some(merged_at) = pr.merged_at {
            if merged_at > since {
                publish_pr_merged(pool, publisher, &pr).await?;
            }
        }
        if pr.updated_at > high_water { high_water = pr.updated_at; }
    }
    if high_water > since {
        set(pool, &key, &high_water.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).await?;
    }
    Ok(())
}

async fn publish_pr_merged(
    pool: &PgPool,
    publisher: &Publisher,
    pr: &PrUpdate,
) -> Result<(), WatcherError> {
    let task_id = resolve_task_id(pool, &pr.head_ref, pr.body.as_deref()).await?;
    let event_key = format!(
        "gh:pr:{}:{}:pr_merged",
        pr.node_id,
        pr.merged_at.unwrap_or(pr.updated_at).timestamp_millis(),
    );
    let payload = serde_json::json!({
        "item_id":     task_id.clone().unwrap_or_default(),
        "pr_node_id":  pr.node_id,
        "repo":        pr.repo.to_string(),
        "pr_number":   pr.number,
        "pr_diff":     "",
    });
    if task_id.is_none() {
        tracing::info!(pr_node=%pr.node_id, head=%pr.head_ref, "PR has no task linkage; skipping publish");
        return Ok(());
    }
    let ev = DomainEvent {
        event_key: event_key.clone(),
        source: Source::Github,
        event_type: "github.pr_merged_ready".into(),
        payload,
    };
    publisher.send(pool, ev, None).await.map_err(WatcherError::Bus)?;
    Ok(())
}
```

- [ ] **Step 2: Build check + commit**

```bash
cargo check -p github-watcher
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): PR polling loop → github.pr_merged_ready with task linkage"
```

---

### Task 16: Release polling loop → github.release_published

**Files:**
- Create: `crates/github-watcher/src/polling/releases.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub async fn run_releases_loop(pool: PgPool, publisher: Arc<Publisher>, client: Arc<dyn GhClient>, tracker: RepoTracker, clock: Arc<dyn Clock>, health: HealthState, cfg: ReleasesLoopConfig, shutdown: CancellationToken) -> Result<(), WatcherError>`
  - `pub struct ReleasesLoopConfig { pub poll_interval: Duration, pub catchup_window: chrono::Duration }`
  - For each tracker-known repo, resume cursor; publish `github.release_published { repo }` per new release with `event_key = "gh:release:{node_id}"`. Advance cursor to max `published_at`.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/polling/releases.rs`:
```rust
use super::RepoTracker;
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::GhClient;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::Publisher;
use totsuka_core::{Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct ReleasesLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

pub async fn run_releases_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: ReleasesLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                for repo in tracker.snapshot().await {
                    if let Err(e) = poll_repo(&pool, &publisher, &client, &repo, &cfg, &clock).await {
                        tracing::warn!(repo=%repo, error=%e, "releases poll failed");
                        health.set_check("github_releases", &format!("fail: {e}")).await;
                    }
                }
            }
        }
    }
}

async fn poll_repo(
    pool: &PgPool,
    publisher: &Publisher,
    client: &Arc<dyn GhClient>,
    repo: &crate::gh_client::RepoSlug,
    cfg: &ReleasesLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::releases(&repo.to_string());
    let since = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad release cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let releases = client.releases_since(repo, since).await?;
    let mut high_water = since;
    for rel in releases {
        let event_key = format!("gh:release:{}", rel.node_id);
        let payload = serde_json::json!({ "repo": rel.repo.to_string(), "tag": rel.tag });
        let ev = DomainEvent {
            event_key,
            source: Source::Github,
            event_type: "github.release_published".into(),
            payload,
        };
        publisher.send(pool, ev, None).await.map_err(WatcherError::Bus)?;
        if rel.published_at > high_water { high_water = rel.published_at; }
    }
    if high_water > since {
        set(pool, &key, &high_water.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Build check + commit**

```bash
cargo check -p github-watcher
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): release polling loop → github.release_published"
```

---

### Task 17: Issues polling loop → github.issue_updated

**Files:**
- Create: `crates/github-watcher/src/polling/issues.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub async fn run_issues_loop(pool: PgPool, publisher: Arc<Publisher>, client: Arc<dyn GhClient>, tracker: RepoTracker, clock: Arc<dyn Clock>, health: HealthState, cfg: IssuesLoopConfig, shutdown: CancellationToken) -> Result<(), WatcherError>`
  - `pub struct IssuesLoopConfig { pub poll_interval: Duration, pub catchup_window: chrono::Duration }`
  - For each tracker-known repo, resume cursor; publish `github.issue_updated { issue_node_id, repo, number, state, updated_at }`; event_key = `gh:issue:{node_id}:{updated_at_ms}`. Advance cursor to max `updated_at`.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/polling/issues.rs`:
```rust
use super::RepoTracker;
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::GhClient;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::Publisher;
use totsuka_core::{event_key_gh_issue, Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct IssuesLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

pub async fn run_issues_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: IssuesLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                for repo in tracker.snapshot().await {
                    if let Err(e) = poll_repo(&pool, &publisher, &client, &repo, &cfg, &clock).await {
                        tracing::warn!(repo=%repo, error=%e, "issues poll failed");
                        health.set_check("github_issues", &format!("fail: {e}")).await;
                    }
                }
            }
        }
    }
}

async fn poll_repo(
    pool: &PgPool,
    publisher: &Publisher,
    client: &Arc<dyn GhClient>,
    repo: &crate::gh_client::RepoSlug,
    cfg: &IssuesLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::issues(&repo.to_string());
    let since = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad issue cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let issues = client.issues_since(repo, since).await?;
    let mut high_water = since;
    for u in issues {
        let event_key = event_key_gh_issue(&u.node_id, u.updated_at.timestamp_millis());
        let payload = serde_json::json!({
            "issue_node_id": u.node_id,
            "repo": u.repo.to_string(),
            "number": u.number,
            "state": u.state,
            "updated_at": u.updated_at,
        });
        let ev = DomainEvent {
            event_key,
            source: Source::Github,
            event_type: "github.issue_updated".into(),
            payload,
        };
        publisher.send(pool, ev, None).await.map_err(WatcherError::Bus)?;
        if u.updated_at > high_water { high_water = u.updated_at; }
    }
    if high_water > since {
        set(pool, &key, &high_water.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Build check + commit**

```bash
cargo check -p github-watcher
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): issues polling loop → github.issue_updated"
```

---

### Task 18: TCP listener for healthz/readyz/metrics

**Files:**
- Create: `crates/github-watcher/src/listener.rs`
- Modify: `crates/github-watcher/src/lib.rs` (`pub mod listener;`)

**Interfaces:**
- Produces:
  - `pub async fn bind_tcp(addr: &str) -> Result<TcpListener, WatcherError>` — parses `addr` (e.g. `"127.0.0.1:7802"`), binds with `SO_REUSEADDR`.
  - `pub async fn serve_tcp(listener: TcpListener, router: axum::Router) -> Result<(), WatcherError>` — accept loop, one task per connection, hyper-util `auto::Builder::serve_connection`.

(Watcher uses TCP loopback per spec §7 — UDS is **not** correct here.)

- [ ] **Step 1: Implement**

`crates/github-watcher/src/listener.rs`:
```rust
//! TCP loopback HTTP listener (spec §7 IPC matrix: github-watcher uses TCP,
//! not UDS, so the same bin can run in a cloud environment later).

use crate::error::WatcherError;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use tokio::net::TcpListener;
use tower::Service;

pub async fn bind_tcp(addr: &str) -> Result<TcpListener, WatcherError> {
    TcpListener::bind(addr)
        .await
        .map_err(|e| WatcherError::Internal(format!("bind {addr}: {e}")))
}

pub async fn serve_tcp(listener: TcpListener, router: axum::Router) -> Result<(), WatcherError> {
    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| WatcherError::Internal(format!("accept: {e}")))?;
        let io = TokioIo::new(stream);
        let tower_service = svc
            .call(())
            .await
            .map_err(|e| WatcherError::Internal(format!("svc.call: {e}")))?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "tcp connection error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_tcp_picks_arbitrary_port() {
        let l = bind_tcp("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0);
    }
}
```

Add `pub mod listener;` to `lib.rs`.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p github-watcher listener::
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): TCP listener for healthz/readyz/metrics"
```

---

### Task 19: Lifecycle probes + signal handling

**Files:**
- Create: `crates/github-watcher/src/lifecycle.rs`
- Modify: `crates/github-watcher/src/lib.rs` (`pub mod lifecycle;`)

**Interfaces:**
- Produces:
  - `pub async fn probe_db(pool: &PgPool, health: &HealthState)` — `SELECT 1` (mirror orchestrator).
  - `pub async fn probe_github(client: &Arc<dyn GhClient>, owner: &str, number: u64, health: &HealthState)` — calls `resolve_project_node_id`; ok → `health.set_check("github", "ok")`.
  - `pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), WatcherError>` — SIGTERM + SIGINT → cancel + 15 s grace.

- [ ] **Step 1: Implement**

`crates/github-watcher/src/lifecycle.rs`:
```rust
use crate::error::WatcherError;
use crate::gh_client::GhClient;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_)  => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_github(
    client: &Arc<dyn GhClient>,
    owner: &str,
    number: u64,
    health: &HealthState,
) {
    match client.resolve_project_node_id(owner, number).await {
        Ok(_)  => health.set_check("github", "ok").await,
        Err(e) => health.set_check("github", &format!("fail: {e}")).await,
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), WatcherError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| WatcherError::Internal(format!("install SIGTERM: {e}")))?;
    let mut int = signal(SignalKind::interrupt())
        .map_err(|e| WatcherError::Internal(format!("install SIGINT: {e}")))?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received; initiating graceful shutdown"),
        _ = int.recv()  => tracing::info!("SIGINT received; initiating graceful shutdown"),
    }
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
```

Add `pub mod lifecycle;` to `lib.rs`.

- [ ] **Step 2: Build + commit**

```bash
cargo check -p github-watcher
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): readyz probes + signal handling"
```

---

### Task 20: Main wiring — all four loops + listener + shutdown

**Files:**
- Modify: `crates/github-watcher/src/main.rs` (replace stub)
- Modify: `crates/github-watcher/src/lib.rs` (re-export WatcherApp / module groups)

**Interfaces:**
- Produces: `main()` that loads config, opens `PgPool`, runs schema check, builds `ColumnMap`, builds `HttpGhClient` from `[github_watcher].github_token`, resolves the project node id, spawns four polling loops + one listener task + one signals task, and joins on the first to exit.

- [ ] **Step 1: Replace main.rs**

`crates/github-watcher/src/main.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;

use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, HttpGhClient};
use github_watcher::lifecycle::{probe_db, probe_github, wait_for_signals};
use github_watcher::listener::{bind_tcp, serve_tcp};
use github_watcher::polling::{
    issues::{run_issues_loop, IssuesLoopConfig},
    prs::{run_prs_loop, PrsLoopConfig},
    project::{run_project_loop, ProjectLoopConfig},
    releases::{run_releases_loop, ReleasesLoopConfig},
    RepoTracker,
};
use github_watcher::schema_check::check_schema_version;
use github_watcher::snapshot::PgSnapshotStore;
use github_watcher::{column_map, WatcherApp};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_bus::Publisher;
use totsuka_core::{ColumnMap, SystemClock};
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + tracing
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);
    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard = totsuka_telemetry::init_tracing(
        &state_dir,
        "github-watcher",
        &config.totsuka.log_level,
    );

    // 2. DB
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:totsuka@{}:{}/{}",
            config.postgres.user,
            config.postgres.host,
            config.postgres.port,
            config.postgres.database,
        )
    });
    let pool = PgPoolOptions::new().max_connections(8).connect(&db_url).await?;
    check_schema_version(&pool).await?;
    create_queue(&pool, &config.bus.queue_name).await?;

    // 3. ColumnMap + clock + publisher
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let column_map: Arc<ColumnMap> = Arc::new(column_map::build(&config)?);
    let publisher = Arc::new(Publisher::new(config.bus.queue_name.clone(), clock.clone()));

    // 4. GhClient
    let token = config.github_watcher.github_token.clone();
    let client: Arc<dyn GhClient> = Arc::new(HttpGhClient::new(token));
    let project_node_id = client
        .resolve_project_node_id(&config.github.project_owner, config.github.project_number)
        .await?;

    // 5. Health + probes
    let health = HealthState::new();
    probe_db(&pool, &health).await;
    probe_github(&client, &config.github.project_owner, config.github.project_number, &health).await;
    health.set_ready(true).await;

    // 6. Shared state
    let tracker = RepoTracker::new();
    let snapshot = Arc::new(PgSnapshotStore::new(pool.clone(), publisher.clone()));
    let shutdown = CancellationToken::new();

    // 7. Loops
    let project_h = {
        let cfg = ProjectLoopConfig {
            project_node_id,
            page_size: config.github_watcher.graphql_page_size,
            poll_interval: Duration::from_secs(config.github_watcher.project_poll_interval_secs),
        };
        let s = shutdown.clone();
        let pool = pool.clone();
        let client = client.clone();
        let snapshot = snapshot.clone();
        let column_map = column_map.clone();
        let tracker = tracker.clone();
        let clock = clock.clone();
        let health = health.clone();
        tokio::spawn(async move {
            run_project_loop(pool, client, snapshot, column_map, tracker, clock, health, cfg, s).await
        })
    };

    let catchup = chrono::Duration::hours(config.github_watcher.catchup_window_hours as i64);
    let issues_h = spawn_loop("issues", &shutdown, async {
        run_issues_loop(
            pool.clone(),
            publisher.clone(),
            client.clone(),
            tracker.clone(),
            clock.clone(),
            health.clone(),
            IssuesLoopConfig {
                poll_interval: Duration::from_secs(config.github_watcher.issues_poll_interval_secs),
                catchup_window: catchup,
            },
            shutdown.clone(),
        ).await
    });
    let prs_h = spawn_loop("prs", &shutdown, async {
        run_prs_loop(
            pool.clone(),
            publisher.clone(),
            client.clone(),
            tracker.clone(),
            clock.clone(),
            health.clone(),
            PrsLoopConfig {
                poll_interval: Duration::from_secs(config.github_watcher.issues_poll_interval_secs),
                catchup_window: catchup,
            },
            shutdown.clone(),
        ).await
    });
    let releases_h = spawn_loop("releases", &shutdown, async {
        run_releases_loop(
            pool.clone(),
            publisher.clone(),
            client.clone(),
            tracker.clone(),
            clock.clone(),
            health.clone(),
            ReleasesLoopConfig {
                poll_interval: Duration::from_secs(config.github_watcher.issues_poll_interval_secs),
                catchup_window: catchup,
            },
            shutdown.clone(),
        ).await
    });

    // 8. Listener
    let listener = bind_tcp(&config.github_watcher.bind).await?;
    let router = totsuka_telemetry::http::router(health.clone())
        .layer(axum::middleware::from_fn(totsuka_telemetry::request_id::middleware));
    let listener_h = tokio::spawn(async move { serve_tcp(listener, router).await });

    // 9. Signals
    let _signals = tokio::spawn(wait_for_signals(shutdown.clone()));

    // 10. WatcherApp::new() is left as a no-op constructor for tests
    let _app = WatcherApp::new(config, clock);

    // 11. Wait on first
    tokio::select! {
        r = project_h  => { let _ = r?; },
        r = issues_h   => { let _ = r?; },
        r = prs_h      => { let _ = r?; },
        r = releases_h => { let _ = r?; },
        r = listener_h => { let _ = r?; },
    }
    // 12. Probe: did the cursor get persisted? (smoke for logs)
    if let Ok(Some(c)) = get(&pool, &CursorKey::project_items()).await {
        tracing::info!(cursor=%c, "project_items cursor at exit");
    }
    Ok(())
}

fn spawn_loop<F>(
    name: &'static str,
    _shutdown: &CancellationToken,
    fut: F,
) -> tokio::task::JoinHandle<Result<(), github_watcher::error::WatcherError>>
where
    F: std::future::Future<Output = Result<(), github_watcher::error::WatcherError>> + Send + 'static,
{
    tokio::spawn(async move {
        tracing::info!(loop_name = name, "starting");
        let r = fut.await;
        tracing::info!(loop_name = name, "exited");
        r
    })
}
```

Re-export in `crates/github-watcher/src/lib.rs`:
```rust
pub use error::WatcherError;
```

- [ ] **Step 2: Build**

```bash
cargo build -p github-watcher
```
Expected: succeeds with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(github-watcher): main wiring — four loops + tcp listener + signals"
```

---

### Task 21: e2e — ProjectsV2 status_changed against MockGhClient + real Postgres

**Files:**
- Create: `crates/github-watcher/tests/e2e_project_loop.rs`

**Interfaces:**
- Consumes: `MockGhClient`, `PgSnapshotStore`, `run_project_loop`
- Produces: one passing e2e test that boots the project loop with a `MockGhClient` returning two pages of items, runs **one tick**, then asserts:
  - `gh_item_status` has every item's status
  - `pgmq` queue has one envelope per diff
  - `catchup_cursor` has the final empty-string sentinel (the loop resets after the last page)

- [ ] **Step 1: Implement**

`crates/github-watcher/tests/e2e_project_loop.rs`:
```rust
use chrono::Utc;
use github_watcher::column_map;
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, MockGhClient, ProjectItem, ProjectItemPage, RepoSlug};
use github_watcher::polling::project::{run_project_loop, ProjectLoopConfig};
use github_watcher::polling::RepoTracker;
use github_watcher::snapshot::PgSnapshotStore;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, read_one, Publisher};
use totsuka_core::{ColumnMap, SystemClock};
use totsuka_telemetry::HealthState;

fn map() -> ColumnMap {
    use std::collections::HashMap;
    use totsuka_core::ColumnId;
    let mut m = HashMap::new();
    m.insert(ColumnId::Inbox,           "📥 Inbox".into());
    m.insert(ColumnId::Ready,           "📋 Ready".into());
    m.insert(ColumnId::Design,          "🤖 調査・設計".into());
    m.insert(ColumnId::DesignReview,    "🚧 設計レビュー".into());
    m.insert(ColumnId::ImplVerify,      "🤖 実装・受入検証".into());
    m.insert(ColumnId::FinalReview,     "🚧 最終レビュー".into());
    m.insert(ColumnId::AwaitingRelease, "🚀 リリース待ち".into());
    m.insert(ColumnId::Released,        "🏁 完了".into());
    ColumnMap::try_new(m).unwrap()
}

#[tokio::test]
async fn project_loop_publishes_status_changed_for_every_diff() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();

    // Clean slate
    sqlx::query("DELETE FROM gh_item_status WHERE item_id LIKE 'E2E_%'").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='projectv2_items'")
        .execute(&pool).await.unwrap();

    let queue = format!("ghw_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let r1 = RepoSlug::parse("acme/x").unwrap();
    mock.set_project_items_pages(vec![
        ProjectItemPage {
            items: vec![
                ProjectItem { id: "E2E_A".into(), status_display: Some("📋 Ready".into()),       repo: Some(r1.clone()), content_number: Some(1), closed_at: None },
                ProjectItem { id: "E2E_B".into(), status_display: Some("🤖 調査・設計".into()), repo: Some(r1.clone()), content_number: Some(2), closed_at: None },
            ],
            end_cursor: Some("p1".into()),
            has_next: true,
        },
        ProjectItemPage {
            items: vec![
                ProjectItem { id: "E2E_C".into(), status_display: Some("🏁 完了".into()), repo: Some(r1.clone()), content_number: Some(3), closed_at: Some(Utc::now()) },
            ],
            end_cursor: Some("p2".into()),
            has_next: false,
        },
    ]);

    let snapshot = Arc::new(PgSnapshotStore::new(pool.clone(), publisher.clone()));
    let tracker = RepoTracker::new();
    let column_map = Arc::new(map());
    let health = HealthState::new();
    let cfg = ProjectLoopConfig {
        project_node_id: "PVT_x".into(),
        page_size: 100,
        poll_interval: Duration::from_millis(50),
    };
    let shutdown = CancellationToken::new();
    let pool2 = pool.clone();
    let s2 = shutdown.clone();
    let h = tokio::spawn(async move {
        run_project_loop(pool2, mock.clone() as Arc<dyn GhClient>, snapshot, column_map, tracker, Arc::new(SystemClock), health, cfg, s2).await
    });

    // Allow one tick
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;

    // Snapshot rows present
    let a: (Option<String>,) = sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_A'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(a.0.as_deref(), Some("ready"));
    let c: (Option<String>,) = sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_C'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(c.0.as_deref(), Some("released"));

    // Three envelopes published
    let mut seen = 0;
    for _ in 0..10 {
        if let Some((mid, env)) = read_one(&pool, &queue, 1).await.unwrap() {
            assert_eq!(env.event_type, "github.status_changed");
            totsuka_bus::delete(&pool, &queue, mid).await.unwrap();
            seen += 1;
        } else { break; }
    }
    assert_eq!(seen, 3);

    // Cursor reset after last page
    assert_eq!(get(&pool, &CursorKey::project_items()).await.unwrap(), Some("".into()));
}
```

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test e2e_project_loop
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(github-watcher): e2e — project loop publishes status_changed for every diff"
```

---

### Task 22: e2e — PR merged → pr_merged_ready with task linkage

**Files:**
- Create: `crates/github-watcher/tests/e2e_pr_linkage.rs`

**Interfaces:**
- Consumes: `MockGhClient`, `run_prs_loop`, `tasks` table
- Produces: e2e test that seeds a `tasks` row, points the mock at a merged PR whose `head_ref` matches the task's `task_id_short`, runs one tick of `run_prs_loop`, and asserts:
  - one `pgmq` envelope of type `github.pr_merged_ready` with `payload.item_id` = the full `tasks.id`
  - the per-repo `prs` cursor advanced

- [ ] **Step 1: Implement**

`crates/github-watcher/tests/e2e_pr_linkage.rs`:
```rust
use chrono::{TimeZone, Utc};
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, MockGhClient, PrUpdate, RepoSlug};
use github_watcher::polling::prs::{run_prs_loop, PrsLoopConfig};
use github_watcher::polling::RepoTracker;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, read_one, Publisher};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::test]
async fn pr_merged_publishes_with_task_id_from_branch() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();

    let task_id = "PVTI_full_aaaaaaaaaaaa";
    let task_short = "aaaaaaaaaaaa";
    sqlx::query("DELETE FROM tasks WHERE id = $1").bind(task_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tasks (id, task_id_short, repo, current_column) VALUES ($1, $2, 'acme/r', 'impl_verify')")
        .bind(task_id).bind(task_short).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='prs:acme/r'").execute(&pool).await.unwrap();

    let queue = format!("ghw_pr_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let repo = RepoSlug::parse("acme/r").unwrap();
    let merged_at = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    mock.set_prs(&repo, vec![PrUpdate {
        node_id: "PR_node_1".into(),
        repo: repo.clone(),
        number: 7,
        head_ref: format!("totsuka/{task_short}/implv"),
        body: None,
        merged: true,
        merged_at: Some(merged_at),
        updated_at: merged_at,
    }]);

    let tracker = RepoTracker::new();
    tracker.insert(repo.clone()).await;

    let cfg = PrsLoopConfig {
        poll_interval: Duration::from_millis(50),
        catchup_window: chrono::Duration::hours(48),
    };
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    let pool2 = pool.clone();
    let h = tokio::spawn(async move {
        run_prs_loop(pool2, publisher, mock.clone() as Arc<dyn GhClient>, tracker, Arc::new(SystemClock), HealthState::new(), cfg, s2).await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;

    let (mid, env) = read_one(&pool, &queue, 1).await.unwrap().expect("one envelope");
    assert_eq!(env.event_type, "github.pr_merged_ready");
    assert_eq!(env.payload["item_id"], task_id);
    assert_eq!(env.payload["repo"],    "acme/r");
    totsuka_bus::delete(&pool, &queue, mid).await.unwrap();

    let cur = get(&pool, &CursorKey::prs("acme/r")).await.unwrap();
    assert!(cur.is_some());
}
```

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test e2e_pr_linkage
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(github-watcher): e2e — PR merged → pr_merged_ready with task linkage"
```

---

### Task 23: e2e — cursor resumes across loop restart

**Files:**
- Create: `crates/github-watcher/tests/e2e_cursor_resume.rs`

**Interfaces:**
- Consumes: `run_issues_loop`, `MockGhClient`, `catchup_cursor`
- Produces: e2e test that runs the issues loop once with 3 updated issues, asserts cursor = max(updated_at). Then restarts with a fresh loop but the same DB; the mock returns one new (later) issue + the same 3 old issues — only the new one publishes (cursor honored).

- [ ] **Step 1: Implement**

`crates/github-watcher/tests/e2e_cursor_resume.rs`:
```rust
use chrono::{TimeZone, Utc};
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, IssueUpdate, MockGhClient, RepoSlug};
use github_watcher::polling::issues::{run_issues_loop, IssuesLoopConfig};
use github_watcher::polling::RepoTracker;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, read_one, Publisher};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

async fn run_once(
    pool: sqlx::PgPool,
    publisher: Arc<Publisher>,
    mock: Arc<MockGhClient>,
    tracker: RepoTracker,
    catchup: chrono::Duration,
) {
    let cfg = IssuesLoopConfig {
        poll_interval: Duration::from_millis(50),
        catchup_window: catchup,
    };
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    let h = tokio::spawn(async move {
        run_issues_loop(pool, publisher, mock as Arc<dyn GhClient>, tracker, Arc::new(SystemClock), HealthState::new(), cfg, s2).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
}

#[tokio::test]
async fn issues_cursor_resumes_and_skips_already_seen() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();

    let repo = RepoSlug::parse("acme/cur").unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='issues:acme/cur'")
        .execute(&pool).await.unwrap();

    let queue = format!("ghw_cur_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let t0 = Utc.with_ymd_and_hms(2026, 6, 29, 10, 0, 0).unwrap();
    let t1 = Utc.with_ymd_and_hms(2026, 6, 29, 11, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let t3 = Utc.with_ymd_and_hms(2026, 6, 29, 13, 0, 0).unwrap();
    mock.set_issues(&repo, vec![
        IssueUpdate { node_id: "I1".into(), repo: repo.clone(), number: 1, updated_at: t0, state: "open".into() },
        IssueUpdate { node_id: "I2".into(), repo: repo.clone(), number: 2, updated_at: t1, state: "open".into() },
        IssueUpdate { node_id: "I3".into(), repo: repo.clone(), number: 3, updated_at: t2, state: "open".into() },
    ]);

    let tracker = RepoTracker::new();
    tracker.insert(repo.clone()).await;

    run_once(pool.clone(), publisher.clone(), mock.clone(), tracker.clone(), chrono::Duration::hours(48)).await;
    // Drain queue (should have 3)
    let mut drained = 0;
    while let Some((mid, _)) = read_one(&pool, &queue, 1).await.unwrap() {
        totsuka_bus::delete(&pool, &queue, mid).await.unwrap();
        drained += 1;
    }
    assert_eq!(drained, 3);
    let cur = get(&pool, &CursorKey::issues("acme/cur")).await.unwrap().unwrap();
    assert!(cur.starts_with("2026-06-29T12:00:00"));

    // Add a new later issue + same 3 old ones. Only I4 should publish.
    mock.set_issues(&repo, vec![
        IssueUpdate { node_id: "I1".into(), repo: repo.clone(), number: 1, updated_at: t0, state: "open".into() },
        IssueUpdate { node_id: "I2".into(), repo: repo.clone(), number: 2, updated_at: t1, state: "open".into() },
        IssueUpdate { node_id: "I3".into(), repo: repo.clone(), number: 3, updated_at: t2, state: "open".into() },
        IssueUpdate { node_id: "I4".into(), repo: repo.clone(), number: 4, updated_at: t3, state: "open".into() },
    ]);
    run_once(pool.clone(), publisher.clone(), mock.clone(), tracker.clone(), chrono::Duration::hours(48)).await;

    let mut drained = 0;
    while let Some((mid, env)) = read_one(&pool, &queue, 1).await.unwrap() {
        assert_eq!(env.payload["issue_node_id"], "I4");
        totsuka_bus::delete(&pool, &queue, mid).await.unwrap();
        drained += 1;
    }
    assert_eq!(drained, 1);
    let cur2 = get(&pool, &CursorKey::issues("acme/cur")).await.unwrap().unwrap();
    assert!(cur2.starts_with("2026-06-29T13:00:00"));
}
```

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test -p github-watcher --test e2e_cursor_resume
```
Expected: 1 passed.

```bash
git add crates/github-watcher/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(github-watcher): e2e — cursor resumes and skips already-seen issues"
```

---

### Task 24: CI gate — full test sweep + clippy + fmt

**Files:**
- None to modify (CI workflow already runs cargo test / clippy / fmt for the workspace per the pattern set in PRs #1 / #2 / #3).

**Interfaces:** none.

- [ ] **Step 1: Local full sweep**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
DATABASE_URL=postgres://postgres:totsuka@127.0.0.1:5432/totsuka cargo test --workspace
cargo deny check
```
Expected: all green.

- [ ] **Step 2: Push branch + open PR**

```bash
git push -u origin <branch-name>
gh pr create --title "feat(github-watcher): ProjectsV2 polling + GitHub event producer" --body "$(cat <<'EOF'
## Summary
- Adds `crates/github-watcher/` (bin + lib) — 4 polling loops + TCP healthz/readyz listener
- `project` loop: ProjectsV2 GraphQL snapshot diff → `github.status_changed` with same-tx publish + UPSERT + cursor (no GraphQL injection — variables-based)
- `prs` loop: REST `/pulls` → `github.pr_merged_ready` with branch-name + `Totsuka-Task:` trailer linkage
- `issues` loop: REST `/issues?since=` → `github.issue_updated`
- `releases` loop: REST `/releases` → `github.release_published`

## Test plan
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace` (with pgmq + DATABASE_URL)
- [ ] `cargo deny check`
- [ ] e2e: `--test e2e_project_loop` (snapshot diff → bus → cursor)
- [ ] e2e: `--test e2e_pr_linkage`     (PR merged → pr_merged_ready with task_id)
- [ ] e2e: `--test e2e_cursor_resume`  (issues cursor honored across restarts)
- [ ] regression: `--test graphql_injection`
EOF
)"
```

- [ ] **Step 3: Watch CI**

`gh pr checks --watch` until green; then merge with `gh pr merge --merge --delete-branch`.

---

## Self-Review Notes (for the controller before kick-off)

- **Spec coverage** (§8.3): every bullet is covered — ProjectsV2 status loop (Tasks 6/13/21), Issues since-pull (9/17/23), PR/release loops (10/15/16/22), PR linkage (14/15), same-tx publish/cursor (6/13), rate limit + 5xx backoff (11), webhook mode left for the future (cfg only).
- **Schema** (§11.1 / §11.4): handled in Task 3 (MIN/TARGET = 6 since current top migration is 0006) and Task 4 (ColumnMap from `[github].columns`).
- **GraphQL safety** (PR #4): Task 8 enforces variables-only with a wire-body regression test, same shape as orchestrator's fix.
- **Atomicity guarantee** (§9.3): `SnapshotStore::commit_page` opens one tx; all `Publisher::send_in_tx` + UPSERT + cursor `set_in_tx` calls happen inside it (Task 6, exercised by Task 21).
- **Determinism / idempotency** (§11.15): `event_key_gh_status` / `event_key_gh_issue` are deterministic, so a partial-tick retry on the next interval re-derives the same keys and orchestrator's `processed_events` absorbs the duplicate.
- **No format!-interpolation into GraphQL** anywhere — `graphql.rs` is `const &str` documents, every variable goes through `json!({ ... })`.
- **No `Utc::now()` direct** anywhere — all timestamps go through `Arc<dyn Clock>` (Tasks 13/15/16/17 take a `clock` parameter; the snapshot publisher's `published_at` comes from `Publisher`'s clock injected in `main.rs`).
- **Secret discipline**: `github_token` is `Secret<String>`; only `HttpGhClient` calls `.expose()`; Debug never leaks it (the wrapper's hand-written impl is in `totsuka-core`).
- **CI shape mirrors PR #3**: Task 24 reuses the same gates (deny / clippy --locked / fmt / workspace test / typos).
- **No half-finished bin**: Tasks 9 / 10 fill in placeholder methods on `HttpGhClient` introduced in Task 8, but no committed step leaves the bin not building. Each task ends green.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-28-github-watcher.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration via `superpowers:subagent-driven-development`.

**2. Inline Execution** — execute tasks in this session with checkpoint review via `superpowers:executing-plans`.

Which approach?
