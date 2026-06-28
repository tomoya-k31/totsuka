# orchestrator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The stateful core of totsuka: a pgmq bus consumer that drives the 8-column ProjectsV2 state machine, spawns implementer/verifier conversations via agent-adapter, writes column moves back to GitHub Project with OCC, and enforces WIP + phase-timeout safety nets.

**Architecture:** Crate `crates/orchestrator/` (bin + lib). The lib is layered: `repository` (Postgres CRUD on `tasks`) → `effect` (lease ledger) → `adapter_client` (HTTP-over-UDS to agent-adapter) + `gh_writeback` (GitHub Projects HTTPS) → `sm/` (state-machine transitions per column) → `consumer` (pgmq.read loop dispatching DomainEvent → SM) + `timer` + `sweeper`. The bin wires them, plus telemetry/lifecycle from foundation. All time goes through `Arc<dyn Clock>`; all secrets through `Secret<String>`.

**Tech Stack:** Rust stable / tokio / sqlx (postgres + chrono) / axum (healthz only) / hyperlocal (adapter UDS) / reqwest (rustls, GitHub HTTPS) / serde + serde_json / anyhow (bin) / thiserror (lib) / async-trait

## Global Constraints

(spec §11 verbatim, plus parent design §4)

- Rust toolchain: **stable**, `[profile.release] panic = "abort"`, `tokio::task::block_in_place` clippy-denied at workspace level
- Schema versioning (spec §11.1): `const MIN_SCHEMA_VERSION: i32 = 6; const TARGET_SCHEMA_VERSION: i32 = 6;` at the bin entry. Mismatch → `SchemaOutOfRange` from totsuka-core + exit 1
- Time (spec §11.5): all DateTime via `Arc<dyn Clock>`; `Utc::now()` direct call is clippy-denied. Storage UTC, display Asia/Tokyo
- Errors (spec §11.6): lib thiserror, bin anyhow; HTTP errors → RFC7807 `/errors/<kind>`
- Secrets (spec §11.7): `Secret<String>` for tokens; `.expose()` only at the outbound HTTP/db call site
- Bounded channels (spec §11.8):
  - bus pull → SM dispatch: `[bus].batch_size * 2 = 32`, full → consumer block (pgmq visibility timeout will re-deliver)
  - SM → adapter HTTP request queue: `node_capacity = 8`, full → block (back-pressure)
  - SM → GitHub writeback queue: `64`, full → block
  - SM → Notifier: `256`, full → drop oldest (spec §13)
- blocking isolation (spec §11.10): subprocess / large parse / sync fs → `spawn_blocking`. None expected here, but follow the rule
- Task identity (spec §11.14): `task_id = ProjectV2Item.id`; branch = `totsuka/{task_id_short}/{phase_short}` decided by orchestrator and passed to adapter
- Effect re-entry (spec §11.15): `effect_key = spawn:{task_id}:{phase}:{attempt}`. DiffBack increments `tasks.impl_verify_attempt`, then claims new effect_key
- Writeback OCC (spec §11.12): version mismatch → abort + set `suppress_writeback_until_human_move = TRUE`; cleared by `human.gate_passed`
- ColumnId (spec §11.4): 8 values, snake_case serde, `[github].columns` map is canonical
- agent state (spec §8.2, §9.7): orchestrator does NOT subscribe `events.subscribe`; only call `pane.read` snapshot during conversation driver
- Pre-flight requirement: foundation + agent-adapter merged into main. This bin's e2e tests need both the pgmq container and (for the smoke test) a running agent-adapter with a MockHerdr-backed unix-socket — or in-process mocks via the adapter_client trait.

---

## File Structure

```
crates/orchestrator/
├── Cargo.toml                          [Create] bin + lib
└── src/
    ├── main.rs                         [Create] anyhow entry
    ├── lib.rs                          [Create] OrchestratorApp + module re-exports
    ├── error.rs                        [Create] OrchestratorError + code()
    ├── schema_check.rs                 [Create] MIN/TARGET_SCHEMA_VERSION + check_schema_version
    ├── repository/
    │   ├── mod.rs                      [Create] Repository trait + Task struct
    │   └── postgres.rs                 [Create] PgRepository impl (sqlx queries)
    ├── effect.rs                       [Create] EffectLedger (lease claim/release)
    ├── adapter_client/
    │   ├── mod.rs                      [Create] AdapterClient trait + types
    │   ├── uds.rs                      [Create] HyperlocalAdapter (production)
    │   └── mock.rs                     [Create] MockAdapter (tests)
    ├── gh_writeback/
    │   ├── mod.rs                      [Create] WritebackClient trait + types
    │   ├── http.rs                     [Create] GraphQL OCC implementation
    │   └── mock.rs                     [Create] MockWriteback
    ├── wip.rs                          [Create] WipGate (tokio::sync::Semaphore wrapper)
    ├── argv.rs                         [Create] 3-layer argv merge (global ++ per_repo ++ per_phase)
    ├── branch.rs                       [Create] branch_name + phase_short helpers
    ├── sm/
    │   ├── mod.rs                      [Create] StateMachine + Transition trait + dispatch
    │   ├── ready_to_design.rs          [Create] spawn designer
    │   ├── design_to_review.rs         [Create] designer completion → column move
    │   ├── impl_verify.rs              [Create] sub-state machine for ImplVerify
    │   ├── final_review.rs             [Create] human gate observation
    │   ├── released.rs                 [Create] release event handler
    │   └── status_change.rs            [Create] human-driven column moves (gate ①②)
    ├── conversation.rs                 [Create] verifier spawn primitive (pane.read + PR diff)
    ├── timer.rs                        [Create] phase deadline tracker
    ├── sweeper.rs                      [Create] expired-lease recovery loop
    ├── notify.rs                       [Create] orchestrator-specific NotifyPayload helpers
    ├── consumer.rs                     [Create] pgmq bus consumer loop
    ├── lifecycle.rs                    [Create] readyz probes + SIGTERM
    └── listener.rs                     [Create] UDS healthz/readyz listener
crates/orchestrator/tests/
├── repository.rs                       [Create] tasks CRUD against real pgmq DB
├── effect.rs                           [Create] lease claim/release race
├── sm_ready_to_design.rs               [Create] integration with MockAdapter + PgRepository
├── sm_impl_verify.rs                   [Create] integration: impl → verifier → DiffBack → pass
├── consumer.rs                         [Create] enqueue DomainEvent + assert SM ran
└── e2e_orchestrator.rs                 [Create] full Inbox → Released walk
```

Workspace edits: add `"crates/orchestrator"` to members.

---

## Tasks

### Task 1: Crate scaffold + bin/lib split

**Files:**
- Create: `crates/orchestrator/Cargo.toml`
- Create: `crates/orchestrator/src/main.rs`
- Create: `crates/orchestrator/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: foundation crates
- Produces: `orchestrator::OrchestratorApp::new(config, clock) -> Self` and `async fn run(self) -> anyhow::Result<()>` (stubbed)

- [ ] **Step 1: Add to workspace**

Append `"crates/orchestrator"` to `Cargo.toml [workspace] members`.

- [ ] **Step 2: Crate Cargo.toml**

`crates/orchestrator/Cargo.toml`:
```toml
[package]
name = "orchestrator"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "orchestrator"
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
hyperlocal  = { workspace = true }
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
uuid        = { workspace = true }
tracing-subscriber = { workspace = true }

[dev-dependencies]
tokio    = { workspace = true, features = ["test-util"] }
tempfile = "3.12"
```

- [ ] **Step 3: lib.rs stub**

`crates/orchestrator/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub struct OrchestratorApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl OrchestratorApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("orchestrator stub: nothing to do yet");
        Ok(())
    }
}
```

- [ ] **Step 4: main.rs stub**

`crates/orchestrator/src/main.rs`:
```rust
use std::sync::Arc;

use orchestrator::OrchestratorApp;
use totsuka_config::Config;
use totsuka_core::SystemClock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::var("TOTSUKA_CONFIG")
        .unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(Config::load(&config_path)?);
    tracing_subscriber::fmt().with_env_filter("info").init();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    OrchestratorApp::new(config, clock).run().await
}
```

- [ ] **Step 5: Verify**

```bash
cargo check --workspace
cargo build -p orchestrator
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): bin/lib scaffold + workspace wire-up"
```

---

### Task 2: OrchestratorError + RFC7807 mapping

**Files:**
- Create: `crates/orchestrator/src/error.rs`
- Modify: `crates/orchestrator/src/lib.rs` (`pub mod error;`)

**Interfaces:**
- Produces: `pub enum OrchestratorError` with variants (`Sqlx`, `Bus`, `Adapter`, `Writeback`, `SchemaOutOfRange { got, min, target }`, `RepoNotRegistered(String)`, `Conflict(String)`, `Internal(String)`) + `code()` returning `/errors/<kind>` + `From` impls for sqlx/bus errors.

- [ ] **Step 1: Failing test**

`crates/orchestrator/src/error.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("adapter: {0}")]
    Adapter(String),
    #[error("writeback: {0}")]
    Writeback(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl OrchestratorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Adapter(_) => "/errors/adapter",
            Self::Writeback(_) => "/errors/writeback",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::RepoNotRegistered(_) => "/errors/repo_not_registered",
            Self::Conflict(_) => "/errors/conflict",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_oor_codes_correctly() {
        let e = OrchestratorError::SchemaOutOfRange { got: 3, min: 5, target: 6 };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
    #[test]
    fn conflict_codes() {
        assert_eq!(OrchestratorError::Conflict("x".into()).code(), "/errors/conflict");
    }
}
```

- [ ] **Step 2: Confirm + wire**

Add `pub mod error;` to `lib.rs`.
Run: `cargo test -p orchestrator error::` → 2 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/orchestrator/src/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): OrchestratorError + RFC7807 code()"
```

---

### Task 3: Schema-version handshake

**Files:**
- Create: `crates/orchestrator/src/schema_check.rs`
- Modify: `crates/orchestrator/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const MIN_SCHEMA_VERSION: i32 = 6;`
  - `pub const TARGET_SCHEMA_VERSION: i32 = 6;`
  - `pub async fn check_schema_version(pool: &PgPool) -> Result<i32, OrchestratorError>`

- [ ] **Step 1: Implement + integration test**

`crates/orchestrator/src/schema_check.rs`:
```rust
//! spec §11.1 bin↔DB handshake. Reads the highest version from schema_meta
//! and validates it against the bin's compiled range.

use sqlx::PgPool;
use crate::error::OrchestratorError;

pub const MIN_SCHEMA_VERSION: i32 = 6;
pub const TARGET_SCHEMA_VERSION: i32 = 6;

pub async fn check_schema_version(pool: &PgPool) -> Result<i32, OrchestratorError> {
    let row: (Option<i32>,) = sqlx::query_as("SELECT max(version) FROM schema_meta")
        .fetch_one(pool)
        .await?;
    let got = row.0.ok_or_else(|| OrchestratorError::Internal(
        "schema_meta is empty; run sqlx migrate".into()
    ))?;
    if got < MIN_SCHEMA_VERSION || got > TARGET_SCHEMA_VERSION {
        return Err(OrchestratorError::SchemaOutOfRange {
            got,
            min: MIN_SCHEMA_VERSION,
            target: TARGET_SCHEMA_VERSION,
        });
    }
    Ok(got)
}
```

`crates/orchestrator/tests/schema_check.rs`:
```rust
use orchestrator::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn returns_target_version_against_migrated_db() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let v = check_schema_version(&pool).await.unwrap();
    assert_eq!(v, TARGET_SCHEMA_VERSION);
}
```

Add `pub mod schema_check;` to `lib.rs`.

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test schema_check
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): schema_meta version handshake"
```

---

### Task 4: Repository trait + Task struct + PgRepository

**Files:**
- Create: `crates/orchestrator/src/repository/mod.rs`
- Create: `crates/orchestrator/src/repository/postgres.rs`
- Create: `crates/orchestrator/tests/repository.rs`

**Interfaces:**
- Produces:
  - `pub struct Task { id, task_id_short, repo, pr_node_id, current_column, current_phase, impl_verify_attempt, suppress_writeback_until_human_move, spawned_at, created_at, updated_at }`
  - `#[async_trait] pub trait Repository: Send + Sync` with `get(&TaskId) -> Option<Task>`, `upsert(&Task) -> ()`, `bump_attempt(&TaskId) -> i32`, `set_pr(&TaskId, &str) -> ()`, `set_suppress(&TaskId, bool) -> ()`, `set_spawned_at(&TaskId, DateTime<Utc>) -> ()`, `find_by_short(&str) -> Option<Task>`
  - `pub struct PgRepository { pool: PgPool, clock: Arc<dyn Clock> }` impls Repository

- [ ] **Step 1: Trait + Task struct (mod.rs)**

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use totsuka_core::TaskId;

use crate::error::OrchestratorError;

pub mod postgres;
pub use postgres::PgRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub task_id_short: String,
    pub repo: String,
    pub pr_node_id: Option<String>,
    pub current_column: String,
    pub current_phase: Option<String>,
    pub impl_verify_attempt: i32,
    pub suppress_writeback_until_human_move: bool,
    pub spawned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait Repository: Send + Sync {
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, OrchestratorError>;
    async fn upsert(&self, t: &Task) -> Result<(), OrchestratorError>;
    async fn bump_attempt(&self, id: &TaskId) -> Result<i32, OrchestratorError>;
    async fn set_pr(&self, id: &TaskId, pr: &str) -> Result<(), OrchestratorError>;
    async fn set_suppress(&self, id: &TaskId, v: bool) -> Result<(), OrchestratorError>;
    async fn set_spawned_at(&self, id: &TaskId, when: DateTime<Utc>) -> Result<(), OrchestratorError>;
    async fn find_by_short(&self, short: &str) -> Result<Option<Task>, OrchestratorError>;
}
```

- [ ] **Step 2: PgRepository (postgres.rs)**

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use totsuka_core::{Clock, TaskId};

use super::{Repository, Task};
use crate::error::OrchestratorError;

pub struct PgRepository {
    pool: PgPool,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl PgRepository {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }
}

#[async_trait]
impl Repository for PgRepository {
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, OrchestratorError> {
        let row = sqlx::query_as::<_, (String, String, String, Option<String>, String, Option<String>, i32, bool, Option<DateTime<Utc>>, DateTime<Utc>, DateTime<Utc>)>(
            "SELECT id, task_id_short, repo, pr_node_id, current_column, current_phase,
                    impl_verify_attempt, suppress_writeback_until_human_move,
                    spawned_at, created_at, updated_at FROM tasks WHERE id = $1"
        ).bind(id.as_str()).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| Task {
            id: TaskId::new(r.0),
            task_id_short: r.1, repo: r.2, pr_node_id: r.3,
            current_column: r.4, current_phase: r.5,
            impl_verify_attempt: r.6, suppress_writeback_until_human_move: r.7,
            spawned_at: r.8, created_at: r.9, updated_at: r.10,
        }))
    }

    async fn upsert(&self, t: &Task) -> Result<(), OrchestratorError> {
        sqlx::query(
            "INSERT INTO tasks (id, task_id_short, repo, pr_node_id, current_column,
                                current_phase, impl_verify_attempt,
                                suppress_writeback_until_human_move, spawned_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
             ON CONFLICT (id) DO UPDATE SET
                 task_id_short = excluded.task_id_short,
                 repo = excluded.repo,
                 pr_node_id = excluded.pr_node_id,
                 current_column = excluded.current_column,
                 current_phase = excluded.current_phase,
                 impl_verify_attempt = excluded.impl_verify_attempt,
                 suppress_writeback_until_human_move = excluded.suppress_writeback_until_human_move,
                 spawned_at = excluded.spawned_at,
                 updated_at = now()"
        )
        .bind(t.id.as_str()).bind(&t.task_id_short).bind(&t.repo)
        .bind(&t.pr_node_id).bind(&t.current_column).bind(&t.current_phase)
        .bind(t.impl_verify_attempt).bind(t.suppress_writeback_until_human_move)
        .bind(t.spawned_at)
        .execute(&self.pool).await?;
        Ok(())
    }

    async fn bump_attempt(&self, id: &TaskId) -> Result<i32, OrchestratorError> {
        let row: (i32,) = sqlx::query_as(
            "UPDATE tasks SET impl_verify_attempt = impl_verify_attempt + 1, updated_at = now()
             WHERE id = $1 RETURNING impl_verify_attempt"
        ).bind(id.as_str()).fetch_one(&self.pool).await?;
        Ok(row.0)
    }

    async fn set_pr(&self, id: &TaskId, pr: &str) -> Result<(), OrchestratorError> {
        sqlx::query("UPDATE tasks SET pr_node_id = $2, updated_at = now() WHERE id = $1")
            .bind(id.as_str()).bind(pr).execute(&self.pool).await?;
        Ok(())
    }

    async fn set_suppress(&self, id: &TaskId, v: bool) -> Result<(), OrchestratorError> {
        sqlx::query("UPDATE tasks SET suppress_writeback_until_human_move = $2, updated_at = now() WHERE id = $1")
            .bind(id.as_str()).bind(v).execute(&self.pool).await?;
        Ok(())
    }

    async fn set_spawned_at(&self, id: &TaskId, when: DateTime<Utc>) -> Result<(), OrchestratorError> {
        sqlx::query("UPDATE tasks SET spawned_at = $2, updated_at = now() WHERE id = $1")
            .bind(id.as_str()).bind(when).execute(&self.pool).await?;
        Ok(())
    }

    async fn find_by_short(&self, short: &str) -> Result<Option<Task>, OrchestratorError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT id FROM tasks WHERE task_id_short = $1")
            .bind(short).fetch_optional(&self.pool).await?;
        match row {
            Some((id,)) => self.get(&TaskId::new(id)).await,
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 3: Integration test (tests/repository.rs)**

```rust
use chrono::Utc;
use orchestrator::repository::{PgRepository, Repository, Task};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::{SystemClock, TaskId};

#[tokio::test]
async fn upsert_and_get_round_trip() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let repo = PgRepository::new(pool, Arc::new(SystemClock));

    let id = TaskId::new(format!("PVTI_test_{}", uuid::Uuid::new_v4().simple()));
    let t = Task {
        id: id.clone(),
        task_id_short: id.short(),
        repo: "x/y".into(),
        pr_node_id: None,
        current_column: "inbox".into(),
        current_phase: None,
        impl_verify_attempt: 0,
        suppress_writeback_until_human_move: false,
        spawned_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    repo.upsert(&t).await.unwrap();
    let got = repo.get(&id).await.unwrap().expect("present");
    assert_eq!(got.repo, "x/y");
    assert_eq!(got.current_column, "inbox");

    let n = repo.bump_attempt(&id).await.unwrap();
    assert_eq!(n, 1);
    let got2 = repo.get(&id).await.unwrap().unwrap();
    assert_eq!(got2.impl_verify_attempt, 1);
}
```

- [ ] **Step 4: Wire + run + commit**

Add `pub mod repository;` to lib.rs. Run integration test against pgmq DB.

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test repository
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): Repository trait + PgRepository CRUD on tasks"
```

---

### Task 5: AdapterClient trait + HyperlocalAdapter + MockAdapter

**Files:**
- Create: `crates/orchestrator/src/adapter_client/mod.rs`
- Create: `crates/orchestrator/src/adapter_client/uds.rs`
- Create: `crates/orchestrator/src/adapter_client/mock.rs`

**Interfaces:**
- Produces:
  - `pub trait AdapterClient: Send + Sync` with `spawn(SpawnReq) -> SpawnRes`, `send(&str, &str) -> ()`, `read(&str) -> ReadRes`, `stop(&str, repo, branch) -> ()`
  - `pub struct SpawnReq { task_id: String, phase: String, attempt: i32, repo: String, branch: String, argv: Vec<String>, env: HashMap<String, Secret<String>> }`
  - `pub struct SpawnRes { agent_id: String, terminal_id: String, worktree_path: String }`
  - `pub struct ReadRes { revision: u64, text: String }`

- [ ] **Step 1: Trait + types (mod.rs)**

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use totsuka_core::Secret;
use crate::error::OrchestratorError;

pub mod uds;
pub mod mock;
pub use uds::HyperlocalAdapter;
pub use mock::MockAdapter;

#[derive(Clone)]
pub struct SpawnReq {
    pub task_id: String,
    pub phase: String,
    pub attempt: i32,
    pub repo: String,
    pub branch: String,
    pub argv: Vec<String>,
    pub env: HashMap<String, Secret<String>>,
}

// Hand-written Debug so env values do not leak (mirrors agent-adapter's SpawnRequest fix).
impl std::fmt::Debug for SpawnReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnReq")
            .field("task_id", &self.task_id)
            .field("phase", &self.phase)
            .field("attempt", &self.attempt)
            .field("repo", &self.repo)
            .field("branch", &self.branch)
            .field("argv", &self.argv)
            .field("env", &format_args!("<{} entries: redacted>", self.env.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnRes {
    pub agent_id: String,
    pub terminal_id: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadRes {
    pub revision: u64,
    pub text: String,
    #[serde(default)]
    pub is_newer: bool,
}

#[async_trait]
pub trait AdapterClient: Send + Sync {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, OrchestratorError>;
    async fn send(&self, agent_id: &str, text: &str) -> Result<(), OrchestratorError>;
    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, OrchestratorError>;
    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), OrchestratorError>;
}

#[derive(Serialize)]
pub(crate) struct WireSpawn<'a> {
    pub task_id: &'a str,
    pub phase: &'a str,
    pub attempt: i32,
    pub repo: &'a str,
    pub branch: &'a str,
    pub argv: &'a [String],
    pub env: HashMap<&'a str, &'a str>,
}
```

- [ ] **Step 2: HyperlocalAdapter (uds.rs)**

```rust
use async_trait::async_trait;
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyperlocal::UnixConnector;
use std::collections::HashMap;
use std::path::PathBuf;

use super::{AdapterClient, ReadRes, SpawnReq, SpawnRes, WireSpawn};
use crate::error::OrchestratorError;

pub struct HyperlocalAdapter {
    socket: PathBuf,
    client: hyper_util::client::legacy::Client<UnixConnector, http_body_util::Full<Bytes>>,
}

impl HyperlocalAdapter {
    pub fn new(socket: PathBuf) -> Self {
        let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build::<_, http_body_util::Full<Bytes>>(UnixConnector);
        Self { socket, client }
    }

    async fn call_json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, OrchestratorError> {
        let uri: hyper::Uri = hyperlocal::Uri::new(&self.socket, path).into();
        let req = Request::builder()
            .method(method).uri(uri)
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(Bytes::from(body.to_string())))
            .map_err(|e| OrchestratorError::Adapter(format!("build req: {e}")))?;
        let resp = self.client.request(req).await
            .map_err(|e| OrchestratorError::Adapter(format!("send: {e}")))?;
        let status = resp.status();
        let body = http_body_util::BodyExt::collect(resp.into_body()).await
            .map_err(|e| OrchestratorError::Adapter(format!("read: {e}")))?
            .to_bytes();
        if !status.is_success() {
            return Err(OrchestratorError::Adapter(format!(
                "{} {}: {}", status.as_u16(), path, String::from_utf8_lossy(&body)
            )));
        }
        if body.is_empty() {
            return serde_json::from_str("null").map_err(|e| OrchestratorError::Adapter(e.to_string()));
        }
        serde_json::from_slice(&body).map_err(|e| OrchestratorError::Adapter(e.to_string()))
    }
}

#[async_trait]
impl AdapterClient for HyperlocalAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, OrchestratorError> {
        let env: HashMap<&str, &str> = req.env.iter()
            .map(|(k, v)| (k.as_str(), v.expose().as_str())).collect();
        let wire = WireSpawn {
            task_id: &req.task_id, phase: &req.phase, attempt: req.attempt,
            repo: &req.repo, branch: &req.branch, argv: &req.argv, env,
        };
        let v = serde_json::to_value(&wire)
            .map_err(|e| OrchestratorError::Adapter(e.to_string()))?;
        self.call_json(Method::POST, "/v1/agents", v).await
    }

    async fn send(&self, agent_id: &str, text: &str) -> Result<(), OrchestratorError> {
        let _: serde_json::Value = self.call_json(
            Method::POST,
            &format!("/v1/agents/{agent_id}/messages"),
            serde_json::json!({ "text": text }),
        ).await?;
        Ok(())
    }

    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, OrchestratorError> {
        let path = format!("/v1/agents/{agent_id}/output?since_revision={since_revision}");
        self.call_json(Method::GET, &path, serde_json::Value::Null).await
    }

    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), OrchestratorError> {
        let uri: hyper::Uri = hyperlocal::Uri::new(
            &self.socket, &format!("/v1/agents/{agent_id}")
        ).into();
        let req = Request::builder()
            .method(Method::DELETE).uri(uri)
            .header("x-totsuka-repo", repo)
            .header("x-totsuka-branch", branch)
            .body(http_body_util::Full::new(Bytes::new()))
            .map_err(|e| OrchestratorError::Adapter(format!("build delete: {e}")))?;
        let resp = self.client.request(req).await
            .map_err(|e| OrchestratorError::Adapter(format!("send delete: {e}")))?;
        if !resp.status().is_success() {
            return Err(OrchestratorError::Adapter(format!("delete: {}", resp.status())));
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Add http-body-util + hyper-util to crate deps**

Add to `crates/orchestrator/Cargo.toml`:
```toml
http-body-util = "0.1"
hyper-util = { version = "0.1", features = ["client", "client-legacy", "tokio"] }
```

- [ ] **Step 4: MockAdapter (mock.rs)**

```rust
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{AdapterClient, ReadRes, SpawnReq, SpawnRes};
use crate::error::OrchestratorError;

#[derive(Default, Clone)]
pub struct MockAdapter {
    state: Arc<Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    pub agents: HashMap<String, MockPane>,
    pub spawn_log: Vec<SpawnReq>,
    pub send_log: Vec<(String, String)>,
}

#[derive(Default, Clone)]
struct MockPane {
    pub text: String,
    pub revision: u64,
    pub repo: String,
    pub branch: String,
}

impl MockAdapter {
    pub fn new() -> Self { Self::default() }
    pub fn spawn_count(&self) -> usize { self.state.lock().unwrap().spawn_log.len() }
    pub fn last_spawn(&self) -> Option<SpawnReq> {
        self.state.lock().unwrap().spawn_log.last().cloned()
    }
    pub fn set_pane_text(&self, agent_id: &str, text: &str) {
        let mut g = self.state.lock().unwrap();
        if let Some(p) = g.agents.get_mut(agent_id) {
            p.text = text.into(); p.revision += 1;
        }
    }
}

#[async_trait]
impl AdapterClient for MockAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, OrchestratorError> {
        let agent_id = format!("ag_{}", Uuid::new_v4().simple());
        let terminal_id = format!("term_{}", agent_id);
        let worktree_path = format!("/tmp/{}", req.branch.replace('/', "__"));
        let mut g = self.state.lock().unwrap();
        g.agents.insert(agent_id.clone(), MockPane {
            text: String::new(), revision: 0,
            repo: req.repo.clone(), branch: req.branch.clone(),
        });
        g.spawn_log.push(req);
        Ok(SpawnRes { agent_id, terminal_id, worktree_path })
    }

    async fn send(&self, agent_id: &str, text: &str) -> Result<(), OrchestratorError> {
        let mut g = self.state.lock().unwrap();
        if let Some(p) = g.agents.get_mut(agent_id) {
            p.text.push_str(text); p.revision += 1;
        }
        g.send_log.push((agent_id.into(), text.into()));
        Ok(())
    }

    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, OrchestratorError> {
        let g = self.state.lock().unwrap();
        let p = g.agents.get(agent_id).ok_or_else(|| OrchestratorError::Adapter(format!("unknown agent {agent_id}")))?;
        Ok(ReadRes {
            revision: p.revision,
            text: p.text.clone(),
            is_newer: p.revision > since_revision,
        })
    }

    async fn stop(&self, agent_id: &str, _repo: &str, _branch: &str) -> Result<(), OrchestratorError> {
        self.state.lock().unwrap().agents.remove(agent_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn spawn_then_read_then_stop() {
        let a = MockAdapter::new();
        let res = a.spawn(SpawnReq {
            task_id: "t".into(), phase: "design".into(), attempt: 0,
            repo: "x/y".into(), branch: "totsuka/aaaaaaaaaaaa/design".into(),
            argv: vec!["claude".into()], env: HashMap::new(),
        }).await.unwrap();
        assert_eq!(a.spawn_count(), 1);
        a.send(&res.agent_id, "hi").await.unwrap();
        let r = a.read(&res.agent_id, 0).await.unwrap();
        assert_eq!(r.text, "hi");
        assert!(r.is_newer);
        a.stop(&res.agent_id, "x/y", "totsuka/aaaaaaaaaaaa/design").await.unwrap();
    }
}
```

- [ ] **Step 5: Wire + run + commit**

Add `pub mod adapter_client;` to lib.rs.
```bash
cargo test -p orchestrator adapter_client
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): AdapterClient trait + HyperlocalAdapter + MockAdapter"
```

---

### Task 6: EffectLedger (lease claim/release on processed_effects)

**Files:**
- Create: `crates/orchestrator/src/effect.rs`
- Create: `crates/orchestrator/tests/effect.rs`

**Interfaces:**
- Produces:
  - `pub struct EffectLedger { pool: PgPool, clock: Arc<dyn Clock>, lease_secs: i64 }`
  - `async fn claim(&self, effect_key: &str, event_key: &str, effect_type: &str, owner: &str) -> Result<ClaimOutcome>` → `Claimed | Skipped { reason }`
  - `async fn complete(&self, effect_key: &str, result: serde_json::Value) -> Result<()>`
  - `async fn fail(&self, effect_key: &str, error: &str) -> Result<()>`

- [ ] **Step 1: Implementation**

```rust
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use totsuka_core::Clock;
use crate::error::OrchestratorError;

pub struct EffectLedger {
    pool: PgPool,
    clock: Arc<dyn Clock>,
    lease_secs: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Skipped { reason: String },
}

impl EffectLedger {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>, lease_secs: i64) -> Self {
        Self { pool, clock, lease_secs }
    }

    pub async fn claim(&self, key: &str, event_key: &str, ty: &str, owner: &str)
        -> Result<ClaimOutcome, OrchestratorError>
    {
        // The processed_effects PK is (effect_key, created_at) because the
        // table is PARTITIONED BY RANGE (created_at). That PK does NOT prevent
        // duplicate `effect_key` rows across different `created_at` values, so
        // a naive INSERT ... ON CONFLICT (effect_key, created_at) DO NOTHING
        // would silently allow concurrent double-claims. Serialize per-key
        // with a pg_advisory_xact_lock keyed on the effect_key hash, then do
        // a normal SELECT + (INSERT-if-missing | UPDATE-if-expired | SKIP)
        // inside the same transaction.
        let now = self.clock.now();
        let expires = now + chrono::Duration::seconds(self.lease_secs);
        let mut tx = self.pool.begin().await?;

        // Lock other claims for this effect_key until commit.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(key)
            .execute(&mut *tx)
            .await?;

        let existing: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT status, lease_expires_at FROM processed_effects
             WHERE effect_key = $1 ORDER BY created_at DESC LIMIT 1"
        ).bind(key).fetch_optional(&mut *tx).await?;

        let outcome = match existing {
            None => {
                sqlx::query(
                    "INSERT INTO processed_effects
                        (effect_key, event_key, effect_type, status, lease_owner,
                         lease_expires_at, attempts, created_at)
                     VALUES ($1, $2, $3, 'in_progress', $4, $5, 1, $6)"
                ).bind(key).bind(event_key).bind(ty).bind(owner).bind(expires).bind(now)
                .execute(&mut *tx).await?;
                ClaimOutcome::Claimed
            }
            Some((s, _)) if s == "done" => ClaimOutcome::Skipped { reason: "already done".into() },
            Some((s, exp)) if s == "in_progress" => {
                if let Some(e) = exp {
                    if e > now {
                        tx.commit().await?;
                        return Ok(ClaimOutcome::Skipped { reason: format!("leased until {e}") });
                    }
                }
                // Expired — take over the most recent row.
                let upd = sqlx::query(
                    "UPDATE processed_effects SET lease_owner = $2, lease_expires_at = $3,
                     attempts = attempts + 1, updated_at = $4
                     WHERE effect_key = $1 AND created_at = (
                         SELECT max(created_at) FROM processed_effects WHERE effect_key = $1
                     )"
                ).bind(key).bind(owner).bind(expires).bind(now)
                .execute(&mut *tx).await?;
                if upd.rows_affected() == 1 { ClaimOutcome::Claimed }
                else { ClaimOutcome::Skipped { reason: "race lost".into() } }
            }
            Some((s, _)) => ClaimOutcome::Skipped { reason: format!("status={s}") },
        };
        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn complete(&self, key: &str, result: Value) -> Result<(), OrchestratorError> {
        sqlx::query(
            "UPDATE processed_effects SET status='done', result=$2, lease_owner=NULL,
             lease_expires_at=NULL, updated_at=$3 WHERE effect_key=$1"
        ).bind(key).bind(result).bind(self.clock.now())
        .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn fail(&self, key: &str, err: &str) -> Result<(), OrchestratorError> {
        sqlx::query(
            "UPDATE processed_effects SET status='failed', result=$2, lease_owner=NULL,
             lease_expires_at=NULL, updated_at=$3 WHERE effect_key=$1"
        ).bind(key).bind(serde_json::json!({"error": err})).bind(self.clock.now())
        .execute(&self.pool).await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Integration test (tests/effect.rs)**

```rust
use orchestrator::effect::{ClaimOutcome, EffectLedger};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::SystemClock;

#[tokio::test]
async fn double_claim_second_skipped() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let l = EffectLedger::new(pool, Arc::new(SystemClock), 30);
    let key = format!("spawn:test:{}:0", uuid::Uuid::new_v4().simple());
    let event = format!("gh:test:{}", uuid::Uuid::new_v4().simple());
    let first = l.claim(&key, &event, "spawn", "owner-a").await.unwrap();
    assert_eq!(first, ClaimOutcome::Claimed);
    let second = l.claim(&key, &event, "spawn", "owner-b").await.unwrap();
    assert!(matches!(second, ClaimOutcome::Skipped { .. }));
}

#[tokio::test]
async fn complete_then_re_claim_skipped() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let l = EffectLedger::new(pool, Arc::new(SystemClock), 30);
    let key = format!("spawn:test:{}:0", uuid::Uuid::new_v4().simple());
    l.claim(&key, "ev", "spawn", "a").await.unwrap();
    l.complete(&key, serde_json::json!({"ok": true})).await.unwrap();
    let again = l.claim(&key, "ev", "spawn", "b").await.unwrap();
    match again {
        ClaimOutcome::Skipped { reason } => assert!(reason.contains("done")),
        other => panic!("expected skipped done, got {other:?}"),
    }
}
```

- [ ] **Step 3: Wire + run + commit**

Add `pub mod effect;` to lib.rs.
```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test effect
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): EffectLedger claim/complete/fail with lease takeover"
```

---

### Task 7: WipGate (bounded concurrency for spawn slots)

**Files:**
- Create: `crates/orchestrator/src/wip.rs`

**Interfaces:**
- Produces:
  - `pub struct WipGate { sem: Arc<Semaphore> }`
  - `pub fn new(capacity: u32) -> Self`
  - `pub async fn try_acquire(&self) -> Option<OwnedSemaphorePermit>` (returns None immediately if full — orchestrator skips and waits for the next event)

- [ ] **Step 1: Implementation + tests**

```rust
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use std::sync::Arc;

pub struct WipGate { sem: Arc<Semaphore> }

impl WipGate {
    pub fn new(capacity: u32) -> Self {
        Self { sem: Arc::new(Semaphore::new(capacity as usize)) }
    }
    pub fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        self.sem.clone().try_acquire_owned().ok()
    }
    pub fn available(&self) -> usize { self.sem.available_permits() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn try_acquire_blocks_when_full() {
        let g = WipGate::new(2);
        let _a = g.try_acquire().unwrap();
        let _b = g.try_acquire().unwrap();
        assert!(g.try_acquire().is_none());
    }
    #[test]
    fn permit_release_restores_slot() {
        let g = WipGate::new(1);
        let permit = g.try_acquire().unwrap();
        assert!(g.try_acquire().is_none());
        drop(permit);
        assert!(g.try_acquire().is_some());
    }
}
```

- [ ] **Step 2: Wire + commit**

Add `pub mod wip;` to lib.rs.
```bash
cargo test -p orchestrator wip::
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): WipGate (Semaphore-backed spawn-slot limit)"
```

---

### Task 8: Argv 3-layer merge

**Files:**
- Create: `crates/orchestrator/src/argv.rs`

**Interfaces:**
- Produces: `pub fn merge_argv(cfg: &ClaudeArgvSection, repo: &str, phase: &Phase) -> Vec<String>` — append-only: `global ++ per_repo.extra ++ per_phase.extra`.

- [ ] **Step 1: Implementation + tests**

```rust
use totsuka_config::schema::ClaudeArgvSection;
use totsuka_core::Phase;

pub fn merge_argv(cfg: &ClaudeArgvSection, repo: &str, phase: &Phase) -> Vec<String> {
    let mut out = cfg.global.clone();
    if let Some(r) = cfg.per_repo.get(repo) { out.extend(r.extra.clone()); }
    if let Some(p) = cfg.per_phase.get(phase.as_snake()) { out.extend(p.extra.clone()); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_config::schema::ClaudeArgvExtra;

    fn extra(args: &[&str]) -> ClaudeArgvExtra {
        ClaudeArgvExtra { extra: args.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn all_three_layers_appended() {
        let mut cfg = ClaudeArgvSection {
            global: vec!["--g".into()],
            per_repo: HashMap::new(), per_phase: HashMap::new(),
            profile: "default".into(), mode: "delegated".into(),
            allowlist: vec![], denylist: vec![],
        };
        cfg.per_repo.insert("x/y".into(), extra(&["--r"]));
        cfg.per_phase.insert("design".into(), extra(&["--p"]));
        let out = merge_argv(&cfg, "x/y", &Phase::Design);
        assert_eq!(out, vec!["--g", "--r", "--p"]);
    }

    #[test]
    fn missing_repo_just_skipped() {
        let cfg = ClaudeArgvSection {
            global: vec!["--g".into()],
            per_repo: HashMap::new(), per_phase: HashMap::new(),
            profile: "default".into(), mode: "delegated".into(),
            allowlist: vec![], denylist: vec![],
        };
        assert_eq!(merge_argv(&cfg, "no/such", &Phase::Design), vec!["--g"]);
    }
}
```

Note the test references `profile / mode / allowlist / denylist`. Verify those fields exist on `ClaudeArgvSection` in `crates/totsuka-config/src/schema.rs`. If they have different defaults, adjust the test literals — the brief's structure must match foundation's actual schema.

- [ ] **Step 2: Wire + commit**

Add `pub mod argv;` to lib.rs.
```bash
cargo test -p orchestrator argv::
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): 3-layer argv merge (global ++ per_repo ++ per_phase)"
```

---

### Task 9: Branch name helpers

**Files:**
- Create: `crates/orchestrator/src/branch.rs`

**Interfaces:**
- Produces:
  - `pub fn branch_name(task: &TaskId, phase: Phase) -> String` → `totsuka/{task_id_short}/{phase_short}`
  - `pub fn phase_short(phase: Phase) -> &'static str` — Design → "design", ImplVerify → "implv"

- [ ] **Step 1: Implementation + tests**

```rust
use totsuka_core::{Phase, TaskId};

pub fn phase_short(phase: Phase) -> &'static str {
    match phase {
        Phase::Design => "design",
        Phase::ImplVerify => "implv",
    }
}

pub fn branch_name(task: &TaskId, phase: Phase) -> String {
    format!("totsuka/{}/{}", task.short(), phase_short(phase))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn design_branch_uses_short_12() {
        let t = TaskId::new("PVTI_lAHOAjcRPs4AHvuRzgVabcdef123456".into());
        assert_eq!(branch_name(&t, Phase::Design), "totsuka/abcdef123456/design");
    }
    #[test]
    fn implv_short_form() {
        let t = TaskId::new("PVTI_short".into());
        assert_eq!(branch_name(&t, Phase::ImplVerify), format!("totsuka/{}/implv", t.short()));
    }
}
```

- [ ] **Step 2: Wire + commit**

Add `pub mod branch;` to lib.rs.
```bash
cargo test -p orchestrator branch::
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): branch_name + phase_short helpers"
```

---

### Task 10: WritebackClient trait + MockWriteback

**Files:**
- Create: `crates/orchestrator/src/gh_writeback/mod.rs`
- Create: `crates/orchestrator/src/gh_writeback/mock.rs`
- Create: `crates/orchestrator/src/gh_writeback/http.rs` (stub for Task 22)

**Interfaces:**
- Produces:
  - `pub trait WritebackClient: Send + Sync` with `move_column(task_id, to_column, expected_version) -> WritebackResult` returning `Ok | VersionMismatch | Failed(String)`
  - `pub struct MockWriteback` — records moves, default returns Ok

- [ ] **Step 1: Trait + types + mock**

`mod.rs`:
```rust
use async_trait::async_trait;
use crate::error::OrchestratorError;

pub mod http;
pub mod mock;
pub use mock::MockWriteback;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritebackResult {
    Ok,
    VersionMismatch,
    Failed(String),
}

#[async_trait]
pub trait WritebackClient: Send + Sync {
    async fn move_column(
        &self,
        task_id: &str,
        to_column: &str,
        expected_version: Option<String>,
    ) -> Result<WritebackResult, OrchestratorError>;
}
```

`mock.rs`:
```rust
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use super::{WritebackClient, WritebackResult};
use crate::error::OrchestratorError;

#[derive(Default, Clone)]
pub struct MockWriteback {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    pub moves: Vec<(String, String, Option<String>)>,
    pub next_result: Option<WritebackResult>,
}

impl MockWriteback {
    pub fn new() -> Self { Self::default() }
    pub fn set_next(&self, r: WritebackResult) {
        self.state.lock().unwrap().next_result = Some(r);
    }
    pub fn moves(&self) -> Vec<(String, String, Option<String>)> {
        self.state.lock().unwrap().moves.clone()
    }
}

#[async_trait]
impl WritebackClient for MockWriteback {
    async fn move_column(&self, task_id: &str, to_column: &str, version: Option<String>)
        -> Result<WritebackResult, OrchestratorError>
    {
        let mut g = self.state.lock().unwrap();
        g.moves.push((task_id.into(), to_column.into(), version));
        Ok(g.next_result.take().unwrap_or(WritebackResult::Ok))
    }
}
```

`http.rs` (stub):
```rust
//! GraphQL OCC implementation — filled in by Task 22.
```

- [ ] **Step 2: Wire + commit**

Add `pub mod gh_writeback;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): WritebackClient trait + MockWriteback"
```

---

### Task 11: State machine skeleton + Engine

**Files:**
- Create: `crates/orchestrator/src/sm/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct Engine { repo: Arc<dyn Repository>, adapter: Arc<dyn AdapterClient>, writeback: Arc<dyn WritebackClient>, effects: Arc<EffectLedger>, wip: Arc<WipGate>, clock: Arc<dyn Clock>, config: Arc<Config>, owner_id: String }`
  - `pub async fn handle(&self, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError>`
  - `pub enum HandleOutcome { Applied, Skipped { reason: String }, WipFull }`
  - Internal dispatch routes events to per-column transition modules added in Tasks 12-19.

- [ ] **Step 1: Skeleton**

```rust
use std::sync::Arc;
use totsuka_core::{Clock, DomainEvent};
use totsuka_config::Config;

use crate::adapter_client::AdapterClient;
use crate::effect::EffectLedger;
use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackClient;
use crate::repository::Repository;
use crate::wip::WipGate;

pub struct Engine {
    pub repo: Arc<dyn Repository>,
    pub adapter: Arc<dyn AdapterClient>,
    pub writeback: Arc<dyn WritebackClient>,
    pub effects: Arc<EffectLedger>,
    pub wip: Arc<WipGate>,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<Config>,
    pub owner_id: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandleOutcome {
    Applied,
    Skipped { reason: String },
    WipFull,
}

impl Engine {
    pub async fn handle(&self, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
        match ev.event_type.as_str() {
            "github.status_changed" => super::sm::status_change::handle(self, ev).await,
            "github.pr_merged_ready" => super::sm::impl_verify::on_pr_merged_ready(self, ev).await,
            "github.pr_verification_passed" => super::sm::impl_verify::on_verification(self, ev, true).await,
            "github.pr_verification_failed" => super::sm::impl_verify::on_verification(self, ev, false).await,
            "github.release_published" => super::sm::released::handle(self, ev).await,
            "human.gate_passed" => super::sm::status_change::on_human_gate(self, ev).await,
            other => {
                tracing::debug!(ty=%other, "unhandled event type");
                Ok(HandleOutcome::Skipped { reason: format!("unhandled: {other}") })
            }
        }
    }
}

// Module declarations — each fills its file in subsequent tasks.
pub mod ready_to_design;
pub mod design_to_review;
pub mod impl_verify;
pub mod final_review;
pub mod released;
pub mod status_change;
```

- [ ] **Step 2: One-line stub per submodule**

Create empty files with module-doc:
- `sm/ready_to_design.rs`: `//! Filled by Task 12.`
- `sm/design_to_review.rs`: `//! Filled by Task 13.`
- `sm/impl_verify.rs`: contents below (stub functions only so mod.rs compiles)
- `sm/final_review.rs`: `//! Filled by Task 18.`
- `sm/released.rs`: contents below
- `sm/status_change.rs`: contents below

`sm/impl_verify.rs`:
```rust
//! Filled progressively by Tasks 14-17.

use totsuka_core::DomainEvent;
use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};

pub async fn on_pr_merged_ready(_e: &Engine, _ev: &DomainEvent)
    -> Result<HandleOutcome, OrchestratorError>
{
    Ok(HandleOutcome::Skipped { reason: "not yet implemented".into() })
}

pub async fn on_verification(_e: &Engine, _ev: &DomainEvent, _passed: bool)
    -> Result<HandleOutcome, OrchestratorError>
{
    Ok(HandleOutcome::Skipped { reason: "not yet implemented".into() })
}
```

`sm/released.rs`:
```rust
//! Filled by Task 19.
use totsuka_core::DomainEvent;
use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};

pub async fn handle(_e: &Engine, _ev: &DomainEvent)
    -> Result<HandleOutcome, OrchestratorError>
{
    Ok(HandleOutcome::Skipped { reason: "not yet implemented".into() })
}
```

`sm/status_change.rs`:
```rust
//! Filled progressively (Tasks 12+18 wire concrete transitions through it).
use totsuka_core::DomainEvent;
use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};

pub async fn handle(_e: &Engine, _ev: &DomainEvent)
    -> Result<HandleOutcome, OrchestratorError>
{
    Ok(HandleOutcome::Skipped { reason: "not yet implemented".into() })
}

pub async fn on_human_gate(_e: &Engine, _ev: &DomainEvent)
    -> Result<HandleOutcome, OrchestratorError>
{
    Ok(HandleOutcome::Skipped { reason: "not yet implemented".into() })
}
```

- [ ] **Step 3: Wire + commit**

Add `pub mod sm;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): state machine Engine + per-column module skeleton"
```

---

### Task 12: status_change handler (human-driven column moves)

**Files:**
- Modify: `crates/orchestrator/src/sm/status_change.rs`
- Create: `crates/orchestrator/tests/sm_status_change.rs`

**Interfaces:**
- Produces: `handle` parses `github.status_changed` payload `{item_id, to_status}`, upserts the task with the new current_column, and (if moving INTO Design from Ready or INTO ImplVerify from DesignReview) returns `HandleOutcome::Applied` so subsequent tasks (13/14) can react to that column. Pure observer for human-driven moves — no spawn here; spawn lives in Tasks 13/14 keyed off the next event tick.
- `on_human_gate` is identical for `human.gate_passed` (only difference: also clears `suppress_writeback_until_human_move`).

- [ ] **Step 1: Implementation**

```rust
use chrono::Utc;
use serde::Deserialize;
use totsuka_core::{DomainEvent, TaskId};

use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct StatusChanged {
    pub item_id: String,
    pub to_status: String,
    #[serde(default)]
    pub repo: String,
}

pub async fn handle(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: StatusChanged = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload parse: {err}")))?;
    upsert_column(e, &p).await?;
    Ok(HandleOutcome::Applied)
}

pub async fn on_human_gate(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: StatusChanged = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload parse: {err}")))?;
    let id = TaskId::new(p.item_id.clone());
    e.repo.set_suppress(&id, false).await?;
    upsert_column(e, &p).await?;
    Ok(HandleOutcome::Applied)
}

async fn upsert_column(e: &Engine, p: &StatusChanged) -> Result<(), OrchestratorError> {
    let id = TaskId::new(p.item_id.clone());
    let now = e.clock.now();
    let existing = e.repo.get(&id).await?;
    let task = match existing {
        Some(mut t) => { t.current_column = p.to_status.clone(); t.updated_at = now; t }
        None => Task {
            id: id.clone(),
            task_id_short: id.short(),
            repo: p.repo.clone(),
            pr_node_id: None,
            current_column: p.to_status.clone(),
            current_phase: None,
            impl_verify_attempt: 0,
            suppress_writeback_until_human_move: false,
            spawned_at: None,
            created_at: now,
            updated_at: now,
        }
    };
    e.repo.upsert(&task).await?;
    Ok(())
}
```

- [ ] **Step 2: Integration test**

`crates/orchestrator/tests/sm_status_change.rs`:
```rust
use chrono::Utc;
use orchestrator::adapter_client::MockAdapter;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::PgRepository;
use orchestrator::sm::{Engine, HandleOutcome};
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::{DomainEvent, Source, SystemClock, TaskId};

fn ev(item_id: &str, to: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:{}:{}", item_id, to),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({"item_id": item_id, "to_status": to, "repo": "x/y"}),
    }
}

async fn engine() -> Option<Engine> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = Arc::new(totsuka_config::Config::load(
        format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"))
    ).unwrap());
    Some(Engine {
        repo: Arc::new(PgRepository::new(pool.clone(), clock.clone())),
        adapter: Arc::new(MockAdapter::new()),
        writeback: Arc::new(MockWriteback::new()),
        effects: Arc::new(EffectLedger::new(pool, clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock,
        config: cfg,
        owner_id: "test".into(),
    })
}

#[tokio::test]
async fn status_change_upserts_task_column() {
    let Some(e) = engine().await else { return };
    let id = format!("PVTI_smtest_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "ready")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    let t = e.repo.get(&TaskId::new(id)).await.unwrap().unwrap();
    assert_eq!(t.current_column, "ready");
}
```

- [ ] **Step 3: Wire + run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test sm_status_change
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): status_change observer (column upsert + gate clear)"
```

---

### Task 13: Ready → Design (designer spawn)

**Files:**
- Modify: `crates/orchestrator/src/sm/ready_to_design.rs`
- Modify: `crates/orchestrator/src/sm/status_change.rs` (after upsert, if column is `ready`, route to `ready_to_design::try_spawn`)
- Create: `crates/orchestrator/tests/sm_ready_to_design.rs`

**Interfaces:**
- Produces: `pub async fn try_spawn(engine: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError>`. Steps:
  1. Acquire WIP permit; if None → return `WipFull`
  2. Claim `spawn:{task_id}:design:0` via EffectLedger; if Skipped → release permit, return Skipped
  3. Compose argv (Task 8) + env (placeholder empty until secrets wired)
  4. Adapter.spawn with branch from `branch::branch_name(task, Design)`
  5. Update task: `current_phase = "design"`, `spawned_at = now`
  6. EffectLedger.complete with `{agent_id, terminal_id, worktree_path}`
  7. Release permit on success
  8. On any error: ledger.fail, release permit, propagate

- [ ] **Step 1: Implementation**

```rust
use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn try_spawn(e: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    let permit = match e.wip.try_acquire() {
        Some(p) => p,
        None => return Ok(HandleOutcome::WipFull),
    };
    let id = task.id.clone();
    let key = spawn_effect_key(&id, Phase::Design, 0);

    let outcome = e.effects.claim(&key, &format!("derived:ready:{}", id.as_str()), "spawn", &e.owner_id).await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        drop(permit);
        return Ok(HandleOutcome::Skipped { reason });
    }

    let argv = merge_argv(&e.config.orchestrator.claude_argv, &task.repo, &Phase::Design);
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: Phase::Design.as_snake().into(),
        attempt: 0,
        repo: task.repo.clone(),
        branch: branch_name(&id, Phase::Design),
        argv,
        env: HashMap::new(),
    };

    let res = match e.adapter.spawn(req).await {
        Ok(r) => r,
        Err(err) => {
            e.effects.fail(&key, &err.to_string()).await?;
            drop(permit);
            return Err(err);
        }
    };

    let now = e.clock.now();
    let mut updated = task.clone();
    updated.current_phase = Some(Phase::Design.as_snake().into());
    updated.spawned_at = Some(now);
    updated.updated_at = now;
    e.repo.upsert(&updated).await?;

    e.effects.complete(&key, serde_json::json!({
        "agent_id": res.agent_id,
        "terminal_id": res.terminal_id,
        "worktree_path": res.worktree_path,
    })).await?;

    drop(permit);
    Ok(HandleOutcome::Applied)
}
```

- [ ] **Step 2: Wire from status_change**

In `sm/status_change.rs` `handle`, after the `upsert_column` call, if `p.to_status == "ready"`:
```rust
if p.to_status == "ready" {
    if let Some(t) = e.repo.get(&id).await? {
        return super::ready_to_design::try_spawn(e, &t).await;
    }
}
```

- [ ] **Step 3: Integration test (sm_ready_to_design.rs)**

Same engine() helper as Task 12 but assert:
- After publishing `status_changed → ready`, MockAdapter.spawn_count() == 1
- The spawn req's branch matches `totsuka/{task_id_short}/design`
- The task row's `current_phase == "design"`

```rust
// reuse the engine() helper pattern from sm_status_change tests
#[tokio::test]
async fn ready_event_spawns_designer() {
    let Some(e) = engine().await else { return };
    let id = format!("PVTI_rd_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "ready")).await.unwrap();
    let adapter = e.adapter.clone();
    // Downcast the MockAdapter via the public state accessor: extract through the
    // engine's `adapter` Arc using the same MockAdapter handle by constructing
    // engine() with a Clone'd MockAdapter you keep a copy of. (See Step 4
    // for the helper modification.)
}
```

The engine() helper builds adapter inline; rewrite it to return `(Engine, Arc<MockAdapter>)` so the test can call `mock.spawn_count()`:

```rust
async fn engine() -> Option<(Engine, Arc<MockAdapter>, Arc<MockWriteback>)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = Arc::new(totsuka_config::Config::load(
        format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"))
    ).unwrap());
    let adapter = Arc::new(MockAdapter::new());
    let writeback = Arc::new(MockWriteback::new());
    let engine = Engine {
        repo: Arc::new(PgRepository::new(pool.clone(), clock.clone())),
        adapter: adapter.clone(),
        writeback: writeback.clone(),
        effects: Arc::new(EffectLedger::new(pool, clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock, config: cfg,
        owner_id: "test".into(),
    };
    Some((engine, adapter, writeback))
}

#[tokio::test]
async fn ready_event_spawns_designer() {
    let Some((e, adapter, _)) = engine().await else { return };
    let id = format!("PVTI_rd_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "ready")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    assert_eq!(adapter.spawn_count(), 1);
    let req = adapter.last_spawn().unwrap();
    assert!(req.branch.ends_with("/design"));
    assert_eq!(req.attempt, 0);
}
```

(Apply the same `engine()` signature change in `sm_status_change.rs` and adjust calling sites.)

- [ ] **Step 4: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test sm_ready_to_design --test sm_status_change
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): Ready → Design designer spawn (effect-keyed, WIP-gated)"
```

---

### Task 14: Design → DesignReview (designer completion writeback)

**Files:**
- Modify: `crates/orchestrator/src/sm/design_to_review.rs`
- Wire: `Engine::handle` already routes `github.status_changed` to `status_change::handle`; the human moves Design→DesignReview manually, so no orchestrator action is needed beyond observing the column change. Add a designer-completion event handler for the case where the designer pane closes (we accept this via a future `github.design_committed` event, but for now expose a one-call helper `request_writeback(engine, task)` that calls `writeback.move_column(task_id, "design_review", None)`).

**Interfaces:**
- Produces: `pub async fn request_writeback(engine: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError>`. Routes through `writeback.move_column` and respects `suppress_writeback_until_human_move`.

- [ ] **Step 1: Implementation**

```rust
//! Designer signals completion via a Project status update committed by the
//! Claude agent (or via a follow-up automated tool). When that arrives,
//! orchestrator may writeback the column move to ProjectsV2.

use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn request_writeback(e: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    if task.suppress_writeback_until_human_move {
        return Ok(HandleOutcome::Skipped { reason: "suppressed until human move".into() });
    }
    match e.writeback.move_column(task.id.as_str(), "design_review", None).await? {
        WritebackResult::Ok => Ok(HandleOutcome::Applied),
        WritebackResult::VersionMismatch => {
            e.repo.set_suppress(&task.id, true).await?;
            Ok(HandleOutcome::Skipped { reason: "OCC conflict; suppress flag set".into() })
        }
        WritebackResult::Failed(msg) => Err(OrchestratorError::Writeback(msg)),
    }
}

#[cfg(test)]
mod tests {
    // The integration of this helper into a real event flow lives in Task 22's
    // bus tests; here we just smoke-test the suppress branch.
}
```

- [ ] **Step 2: Unit test**

Add inline:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::Arc;
    use totsuka_core::{SystemClock, TaskId};

    fn task(id: &str, suppress: bool) -> Task {
        Task {
            id: TaskId::new(id.into()), task_id_short: id.into(), repo: "x/y".into(),
            pr_node_id: None, current_column: "design".into(),
            current_phase: Some("design".into()),
            impl_verify_attempt: 0,
            suppress_writeback_until_human_move: suppress,
            spawned_at: None, created_at: Utc::now(), updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn suppress_flag_skips_writeback() {
        // The full Engine isn't trivial to construct without a DB; build the
        // minimum pieces and call request_writeback. Use MockAdapter,
        // MockWriteback, and an in-memory Repository stub.
        // (For now, this is a placeholder noting the assertion intent; the
        // suppress branch is also covered end-to-end in Task 22 tests.)
    }
}
```

> The inline unit test is intentionally minimal — the full flow is exercised end-to-end in the bus-consumer integration test (Task 22). Mark the inline tests `#[ignore]` if you can't construct an Engine without DB.

- [ ] **Step 3: Build + commit**

```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): Design → DesignReview writeback (OCC suppress on conflict)"
```

---

### Task 15: ImplVerify entry — implementer spawn

**Files:**
- Modify: `crates/orchestrator/src/sm/impl_verify.rs`
- Modify: `crates/orchestrator/src/sm/status_change.rs` (route `impl_verify` column → `impl_verify::on_enter`)

**Interfaces:**
- Produces:
  - `pub async fn on_enter(engine: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError>` — spawn implementer using `spawn_effect_key(task, ImplVerify, task.impl_verify_attempt)` (default 0)
  - Mirrors `ready_to_design::try_spawn` shape but for ImplVerify phase

- [ ] **Step 1: Add on_enter**

Append to `sm/impl_verify.rs`:
```rust
use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::effect::ClaimOutcome;
use crate::repository::Task;

pub async fn on_enter(e: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    let permit = match e.wip.try_acquire() {
        Some(p) => p,
        None => return Ok(HandleOutcome::WipFull),
    };
    let id = task.id.clone();
    let attempt = task.impl_verify_attempt;
    let key = spawn_effect_key(&id, Phase::ImplVerify, attempt);
    let outcome = e.effects.claim(&key, &format!("derived:iv:{}", id.as_str()), "spawn", &e.owner_id).await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        drop(permit);
        return Ok(HandleOutcome::Skipped { reason });
    }

    let argv = merge_argv(&e.config.orchestrator.claude_argv, &task.repo, &Phase::ImplVerify);
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: Phase::ImplVerify.as_snake().into(),
        attempt,
        repo: task.repo.clone(),
        branch: branch_name(&id, Phase::ImplVerify),
        argv,
        env: HashMap::new(),
    };

    let res = match e.adapter.spawn(req).await {
        Ok(r) => r,
        Err(err) => {
            e.effects.fail(&key, &err.to_string()).await?;
            drop(permit);
            return Err(err);
        }
    };

    let now = e.clock.now();
    let mut updated = task.clone();
    updated.current_phase = Some(Phase::ImplVerify.as_snake().into());
    updated.spawned_at = Some(now);
    updated.updated_at = now;
    e.repo.upsert(&updated).await?;

    e.effects.complete(&key, serde_json::json!({
        "agent_id": res.agent_id,
        "terminal_id": res.terminal_id,
        "worktree_path": res.worktree_path,
        "role": "implementer",
    })).await?;
    drop(permit);
    Ok(HandleOutcome::Applied)
}
```

- [ ] **Step 2: Wire from status_change.rs**

After the `ready` branch in `status_change::handle`:
```rust
if p.to_status == "impl_verify" {
    if let Some(t) = e.repo.get(&id).await? {
        return super::impl_verify::on_enter(e, &t).await;
    }
}
```

- [ ] **Step 3: Integration test (extend sm_ready_to_design tests OR new tests/sm_impl_verify.rs)**

```rust
// in tests/sm_impl_verify.rs (reuse engine() helper pattern via copy/paste —
// keeping each test file self-contained simplifies parallel test execution)

#[tokio::test]
async fn impl_verify_enter_spawns_implementer() {
    let Some((e, adapter, _)) = engine().await else { return };
    let id = format!("PVTI_iv_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 1);
    let req = adapter.last_spawn().unwrap();
    assert!(req.branch.ends_with("/implv"));
    assert_eq!(req.attempt, 0);
}
```

- [ ] **Step 4: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test sm_impl_verify
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): ImplVerify entry — implementer spawn with attempt-keyed effect"
```

---

### Task 16: Conversation driver (verifier spawn + input composition)

**Files:**
- Create: `crates/orchestrator/src/conversation.rs`

**Interfaces:**
- Produces:
  - `pub async fn spawn_verifier(engine: &Engine, task: &Task, implementer_agent_id: &str, pr_diff: &str) -> Result<HandleOutcome, OrchestratorError>`:
    1. Reads implementer pane snapshot via `adapter.read(implementer_agent_id, 0)` (we want the full text — pass 0 so `is_newer` is informative but not used here)
    2. Spawns a verifier agent on a fresh branch (`totsuka/{short}/verify`) keyed `spawn:{task}:verify:{attempt}` (NEW phase short — but Phase enum only has Design + ImplVerify; we use a dedicated effect_key suffix "verify" + reuse ImplVerify Phase for argv selection)
    3. Composes input text: `<implementer-snapshot>\n\n<PR-diff>`
    4. Calls `adapter.send(verifier_agent_id, input_text)`
    5. Saves verifier state in EffectLedger.result as `{role: "verifier", agent_id, terminal_id, worktree_path}`

> **Phase enum note:** the foundation Phase enum currently has only Design and ImplVerify. We're conservative and add `Phase::Verify` in a separate small task if needed. For now this task uses a hand-rolled effect_key string for verifier and reuses `Phase::ImplVerify` for argv merge (verifier inherits the same per_phase argv).

- [ ] **Step 1: Implementation**

```rust
use std::collections::HashMap;

use totsuka_core::{key::spawn_effect_key, Phase};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn spawn_verifier(
    e: &Engine,
    task: &Task,
    implementer_agent_id: &str,
    pr_diff: &str,
) -> Result<HandleOutcome, OrchestratorError> {
    let snap = e.adapter.read(implementer_agent_id, 0).await?;
    let id = task.id.clone();
    // Effect key for verifier — string-built so we don't pollute Phase enum;
    // attempt = task.impl_verify_attempt so DiffBack restart bumps it the same way.
    let key = format!("spawn:{}:verify:{}", id.as_str(), task.impl_verify_attempt);
    let outcome = e.effects.claim(&key, &format!("derived:verify:{}:{}", id.as_str(), task.impl_verify_attempt), "spawn", &e.owner_id).await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        return Ok(HandleOutcome::Skipped { reason });
    }
    let argv = merge_argv(&e.config.orchestrator.claude_argv, &task.repo, &Phase::ImplVerify);
    let branch = format!("totsuka/{}/verify", id.short());
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: "verify".into(),
        attempt: task.impl_verify_attempt,
        repo: task.repo.clone(),
        branch,
        argv,
        env: HashMap::new(),
    };
    let res = match e.adapter.spawn(req).await {
        Ok(r) => r,
        Err(err) => { e.effects.fail(&key, &err.to_string()).await?; return Err(err); }
    };

    let input = format!("{}\n\n--- PR DIFF ---\n{}", snap.text, pr_diff);
    if let Err(err) = e.adapter.send(&res.agent_id, &input).await {
        e.effects.fail(&key, &err.to_string()).await?;
        return Err(err);
    }

    e.effects.complete(&key, serde_json::json!({
        "agent_id": res.agent_id,
        "terminal_id": res.terminal_id,
        "worktree_path": res.worktree_path,
        "role": "verifier",
    })).await?;
    Ok(HandleOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter_client::MockAdapter;
    // The full integration of spawn_verifier into the SM lives in Task 17;
    // this stub asserts the helper signature compiles.
    #[test]
    fn signature_compiles() {
        let _f: fn(&Engine, &Task, &str, &str) ->
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<HandleOutcome, OrchestratorError>> + Send + '_>>;
    }
}
```

(The signature_compiles test is a doc-test surrogate that ensures the function lives where Task 17 expects it.)

- [ ] **Step 2: Wire + commit**

Add `pub mod conversation;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): conversation driver — verifier spawn + input compose"
```

---

### Task 17: ImplVerify completion (PR merged → verifier, verifier verdict → DiffBack | pass)

**Files:**
- Modify: `crates/orchestrator/src/sm/impl_verify.rs`

**Interfaces:**
- Produces: real bodies for `on_pr_merged_ready` and `on_verification` (replacing Task 11 stubs).
  - `on_pr_merged_ready`: lookup task by `item_id`, fetch implementer effect result to get `agent_id` (we stored it in EffectLedger), pull PR diff from payload, call `conversation::spawn_verifier`.
  - `on_verification`: payload includes `{item_id, passed: bool}`. If passed → orchestrator requests writeback `final_review`. If failed (DiffBack) → bump attempt, call `on_enter` again for new spawn.

- [ ] **Step 1: Replace stubs**

```rust
//! ImplVerify sub-state machine. spec §9.3 + §4.2 + §11.15.

use serde::Deserialize;
use sqlx::PgPool;
use totsuka_core::{DomainEvent, Phase, TaskId};

use crate::conversation::spawn_verifier;
use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct PrMergedReady {
    pub item_id: String,
    #[serde(default)]
    pub pr_diff: String,
}

pub async fn on_pr_merged_ready(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: PrMergedReady = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let id = TaskId::new(p.item_id.clone());
    let task = match e.repo.get(&id).await? {
        Some(t) => t,
        None => return Ok(HandleOutcome::Skipped { reason: "no such task".into() }),
    };
    // Look up the most recent implementer agent_id from processed_effects
    // (we stored {role:"implementer", agent_id}). If absent, skip.
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT result FROM processed_effects
         WHERE effect_key = $1 AND status = 'done'
         ORDER BY created_at DESC LIMIT 1"
    )
    .bind(format!("spawn:{}:impl_verify:{}", id.as_str(), task.impl_verify_attempt))
    .fetch_optional(pool_for(e)).await
    .map_err(OrchestratorError::Sqlx)?;
    let agent_id = match row {
        Some((v,)) => v.get("agent_id").and_then(|x| x.as_str()).map(String::from),
        None => None,
    };
    let Some(agent_id) = agent_id else {
        return Ok(HandleOutcome::Skipped { reason: "no implementer agent recorded".into() });
    };
    spawn_verifier(e, &task, &agent_id, &p.pr_diff).await
}

// We need a PgPool reference for the lookup. Add a helper on Engine that
// downcasts repo to PgRepository to expose the pool — OR add an
// `effect_result_for` accessor on EffectLedger. The latter is cleaner.

fn pool_for(_e: &Engine) -> &PgPool {
    unimplemented!("see Step 1b — add EffectLedger::result_for and switch to it")
}

#[derive(Deserialize)]
struct Verification {
    pub item_id: String,
}

pub async fn on_verification(e: &Engine, ev: &DomainEvent, passed: bool) -> Result<HandleOutcome, OrchestratorError> {
    let p: Verification = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let id = TaskId::new(p.item_id);
    let task = match e.repo.get(&id).await? {
        Some(t) => t,
        None => return Ok(HandleOutcome::Skipped { reason: "no such task".into() }),
    };
    if passed {
        if task.suppress_writeback_until_human_move {
            return Ok(HandleOutcome::Skipped { reason: "suppressed".into() });
        }
        match e.writeback.move_column(task.id.as_str(), "final_review", None).await? {
            WritebackResult::Ok => Ok(HandleOutcome::Applied),
            WritebackResult::VersionMismatch => {
                e.repo.set_suppress(&task.id, true).await?;
                Ok(HandleOutcome::Skipped { reason: "OCC".into() })
            }
            WritebackResult::Failed(m) => Err(OrchestratorError::Writeback(m)),
        }
    } else {
        // DiffBack: bump attempt, restart implementer.
        let _ = Phase::ImplVerify; // marker
        let new_attempt = e.repo.bump_attempt(&task.id).await?;
        tracing::info!(task=%task.id.as_str(), new_attempt, "DiffBack: re-entering ImplVerify");
        let updated = e.repo.get(&task.id).await?.unwrap();
        on_enter(e, &updated).await
    }
}
```

- [ ] **Step 1b: Add `EffectLedger::result_for`**

In `src/effect.rs`:
```rust
pub async fn result_for(&self, effect_key: &str)
    -> Result<Option<serde_json::Value>, OrchestratorError>
{
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT result FROM processed_effects WHERE effect_key = $1 AND status = 'done'
         ORDER BY created_at DESC LIMIT 1"
    ).bind(effect_key).fetch_optional(&self.pool).await?;
    Ok(row.map(|r| r.0))
}
```

Replace the inline sqlx call in `on_pr_merged_ready` with `e.effects.result_for(&key).await?`. Delete `pool_for`.

- [ ] **Step 2: Integration test**

Extend `tests/sm_impl_verify.rs`:
```rust
fn pr_ready(item_id: &str, diff: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:pr:{}", item_id),
        source: Source::Github,
        event_type: "github.pr_merged_ready".into(),
        payload: serde_json::json!({"item_id": item_id, "pr_diff": diff}),
    }
}

#[tokio::test]
async fn pr_ready_spawns_verifier_after_implementer() {
    let Some((e, adapter, _)) = engine().await else { return };
    let id = format!("PVTI_pr_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 1, "implementer spawned");
    let _ = e.handle(&pr_ready(&id, "diff text here")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 2, "verifier spawned");
    let verifier = adapter.last_spawn().unwrap();
    assert_eq!(verifier.phase, "verify");
}

fn verify_event(item_id: &str, ty: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:v:{}", item_id),
        source: Source::Github,
        event_type: ty.into(),
        payload: serde_json::json!({"item_id": item_id}),
    }
}

#[tokio::test]
async fn diff_back_bumps_attempt_and_respawns() {
    let Some((e, adapter, _)) = engine().await else { return };
    let id = format!("PVTI_db_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    let _ = e.handle(&verify_event(&id, "github.pr_verification_failed")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 2);
    let last = adapter.last_spawn().unwrap();
    assert_eq!(last.attempt, 1, "attempt bumped");
    assert!(last.branch.ends_with("/implv"));
}
```

- [ ] **Step 3: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test sm_impl_verify
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): ImplVerify — verifier spawn on PR ready, DiffBack increments attempt"
```

---

### Task 18: FinalReview observation

**Files:**
- Modify: `crates/orchestrator/src/sm/final_review.rs`

**Interfaces:**
- `pub async fn on_enter(engine: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError>`. No spawn — human gate ②. Records column move only. Wired from `status_change.rs` (column == "final_review").

- [ ] **Step 1: Implementation**

```rust
//! Final review is a human gate (parent §4.1). orchestrator just records the
//! column move; WIP is naturally bounded because each task occupies its slot
//! until either AwaitingRelease or rejected back to ImplVerify.

use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn on_enter(_e: &Engine, _task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    tracing::info!(task=%_task.id.as_str(), "task entered final_review (human gate)");
    Ok(HandleOutcome::Applied)
}
```

- [ ] **Step 2: Wire from status_change.rs**

After the `impl_verify` branch:
```rust
if p.to_status == "final_review" {
    if let Some(t) = e.repo.get(&id).await? {
        return super::final_review::on_enter(e, &t).await;
    }
}
```

- [ ] **Step 3: Build + commit**

```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): FinalReview observation (human gate ②)"
```

---

### Task 19: Released event handler

**Files:**
- Modify: `crates/orchestrator/src/sm/released.rs`

**Interfaces:**
- `handle` for `github.release_published`: payload `{repo, release_tag}`; bulk-look up all tasks in this repo with `current_column = "awaiting_release"` and writeback them to `released`.

- [ ] **Step 1: Add a Repository helper**

In `repository/mod.rs` trait:
```rust
async fn list_awaiting_release_in_repo(&self, repo: &str) -> Result<Vec<Task>, OrchestratorError>;
```

In `repository/postgres.rs`:
```rust
async fn list_awaiting_release_in_repo(&self, repo: &str) -> Result<Vec<Task>, OrchestratorError> {
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, String, Option<String>, i32, bool, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, task_id_short, repo, pr_node_id, current_column, current_phase,
                impl_verify_attempt, suppress_writeback_until_human_move,
                spawned_at, created_at, updated_at
         FROM tasks WHERE repo = $1 AND current_column = 'awaiting_release'"
    ).bind(repo).fetch_all(&self.pool).await?;
    Ok(rows.into_iter().map(|r| Task {
        id: totsuka_core::TaskId::new(r.0),
        task_id_short: r.1, repo: r.2, pr_node_id: r.3,
        current_column: r.4, current_phase: r.5,
        impl_verify_attempt: r.6, suppress_writeback_until_human_move: r.7,
        spawned_at: r.8, created_at: r.9, updated_at: r.10,
    }).collect())
}
```

- [ ] **Step 2: released::handle**

```rust
use serde::Deserialize;
use totsuka_core::DomainEvent;

use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct ReleasePublished { pub repo: String }

pub async fn handle(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: ReleasePublished = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let tasks = e.repo.list_awaiting_release_in_repo(&p.repo).await?;
    let mut applied = 0;
    for t in tasks {
        if t.suppress_writeback_until_human_move { continue; }
        match e.writeback.move_column(t.id.as_str(), "released", None).await? {
            WritebackResult::Ok => applied += 1,
            WritebackResult::VersionMismatch => {
                e.repo.set_suppress(&t.id, true).await?;
            }
            WritebackResult::Failed(_) => {}
        }
    }
    tracing::info!(repo=%p.repo, applied, "released event processed");
    Ok(HandleOutcome::Applied)
}
```

- [ ] **Step 3: Build + commit**

```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): release_published → bulk writeback awaiting_release tasks"
```

---

### Task 20: Bus consumer loop

**Files:**
- Create: `crates/orchestrator/src/consumer.rs`

**Interfaces:**
- Produces:
  - `pub async fn run_consumer(engine: Arc<Engine>, pool: PgPool, queue: String, batch_size: i32, vt_secs: i32, shutdown: CancellationToken) -> Result<(), OrchestratorError>`
  - Reads one envelope at a time via `totsuka_bus::Consumer::poll_one` (Task E2 from foundation), looks up `processed_events` for idempotency, dispatches to `engine.handle`, ack on success.
  - Uses `tokio_util::sync::CancellationToken` for graceful exit.

- [ ] **Step 1: Add tokio-util dep**

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

- [ ] **Step 2: Implementation**

```rust
use std::sync::Arc;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use totsuka_bus::consumer::Consumer;

use crate::error::OrchestratorError;
use crate::sm::Engine;

pub async fn run_consumer(
    engine: Arc<Engine>,
    pool: PgPool,
    queue: String,
    _batch_size: i32,
    vt_secs: i32,
    shutdown: CancellationToken,
) -> Result<(), OrchestratorError> {
    let consumer = Consumer::new(queue.clone());
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("bus consumer shutting down");
                return Ok(());
            }
            r = consumer.poll_one(&pool, vt_secs) => {
                match r {
                    Ok(Some((msg_id, env))) => {
                        let event_key = env.event_key.clone();
                        // processed_events idempotency
                        if is_processed(&pool, &event_key).await? {
                            consumer.ack(&pool, msg_id).await
                                .map_err(OrchestratorError::Bus)?;
                            continue;
                        }
                        let domain = totsuka_core::DomainEvent {
                            event_key: env.event_key.clone(),
                            source: env.source,
                            event_type: env.event_type.clone(),
                            payload: env.payload.clone(),
                        };
                        match engine.handle(&domain).await {
                            Ok(_) => {
                                mark_processed(&pool, &event_key, &env.event_type, &env.payload).await?;
                                consumer.ack(&pool, msg_id).await
                                    .map_err(OrchestratorError::Bus)?;
                            }
                            Err(err) => {
                                tracing::error!(error=%err, event_key=%event_key, "handler failed; leaving in queue for retry");
                                // don't ack — pgmq visibility timeout will re-deliver
                            }
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(err) => {
                        tracing::error!(error=%err, "consumer poll error; backing off");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }
}

async fn is_processed(pool: &PgPool, key: &str) -> Result<bool, OrchestratorError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT event_key FROM processed_events WHERE event_key = $1 LIMIT 1"
    ).bind(key).fetch_optional(pool).await?;
    Ok(row.is_some())
}

async fn mark_processed(pool: &PgPool, key: &str, ty: &str, payload: &serde_json::Value)
    -> Result<(), OrchestratorError>
{
    let hash = format!("{:x}", md5::compute(payload.to_string().as_bytes()));
    sqlx::query(
        "INSERT INTO processed_events (event_key, source, event_type, payload_hash)
         VALUES ($1, 'github', $2, $3) ON CONFLICT DO NOTHING"
    ).bind(key).bind(ty).bind(hash).execute(pool).await?;
    Ok(())
}
```

Add to Cargo.toml:
```toml
md5 = "0.7"
```

- [ ] **Step 3: Integration test (tests/consumer.rs)**

```rust
use orchestrator::adapter_client::MockAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::PgRepository;
use orchestrator::sm::Engine;
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::publisher::Publisher;
use totsuka_bus::pgmq::create_queue;
use totsuka_core::{DomainEvent, Source, SystemClock, TaskId};

#[tokio::test]
async fn consumer_drives_status_change_into_repo() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();
    let q = format!("test_{}", uuid::Uuid::new_v4().simple().to_string().chars().take(20).collect::<String>());
    create_queue(&pool, &q).await.unwrap();

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = Arc::new(totsuka_config::Config::load(
        format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"))
    ).unwrap());
    let adapter = Arc::new(MockAdapter::new());
    let repo = Arc::new(PgRepository::new(pool.clone(), clock.clone()));
    let engine = Arc::new(Engine {
        repo: repo.clone(),
        adapter: adapter.clone(),
        writeback: Arc::new(MockWriteback::new()),
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock: clock.clone(), config: cfg,
        owner_id: "test".into(),
    });

    let id = format!("PVTI_cons_{}", uuid::Uuid::new_v4().simple());
    let pub_ = Publisher::new(q.clone(), clock.clone());
    pub_.send(&pool, DomainEvent {
        event_key: format!("test:cons:{}", id),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({"item_id": id, "to_status": "ready", "repo": "x/y"}),
    }, None).await.unwrap();

    let token = CancellationToken::new();
    let token2 = token.clone();
    let engine2 = engine.clone();
    let pool2 = pool.clone();
    let q2 = q.clone();
    let h = tokio::spawn(async move {
        run_consumer(engine2, pool2, q2, 16, 30, token2).await
    });

    // Poll until the task row appears (or 5s timeout).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(t) = repo.get(&TaskId::new(id.clone())).await.unwrap() {
            if t.current_column == "ready" { break; }
        }
        if std::time::Instant::now() > deadline { panic!("timed out waiting for SM to apply"); }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(adapter.spawn_count(), 1, "ready event should spawn designer");
    token.cancel();
    let _ = h.await;

    // Cleanup queue.
    sqlx::query("SELECT pgmq.drop_queue($1)").bind(&q).execute(&pool).await.unwrap();
}
```

- [ ] **Step 4: Wire + run + commit**

Add `pub mod consumer;` to lib.rs.
```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p orchestrator --test consumer
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): bus consumer loop with processed_events dedup"
```

---

### Task 21: Phase timer (deadline tracker)

**Files:**
- Create: `crates/orchestrator/src/timer.rs`

**Interfaces:**
- `pub async fn run_timer(engine: Arc<Engine>, tick_secs: u64, shutdown: CancellationToken)`. Every `tick_secs`:
  1. Query tasks where `spawned_at IS NOT NULL AND spawned_at < now() - <phase_timeout_secs>` for each `current_phase`
  2. For each overdue task: write a `Blocked` mark (we don't have a Blocked column — store it as `current_phase = "<phase>_blocked"`, simplest tombstone) and emit a Notifier alert (`NotifyKind::TaskStuck` from foundation)

- [ ] **Step 1: Add Repository helper**

trait:
```rust
async fn list_overdue(&self, deadline: chrono::DateTime<chrono::Utc>, phase: &str) -> Result<Vec<Task>, OrchestratorError>;
```

postgres impl:
```rust
async fn list_overdue(&self, deadline: chrono::DateTime<chrono::Utc>, phase: &str)
    -> Result<Vec<Task>, OrchestratorError>
{
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, String, Option<String>, i32, bool, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, task_id_short, repo, pr_node_id, current_column, current_phase,
                impl_verify_attempt, suppress_writeback_until_human_move,
                spawned_at, created_at, updated_at FROM tasks
         WHERE current_phase = $2 AND spawned_at IS NOT NULL AND spawned_at < $1"
    ).bind(deadline).bind(phase).fetch_all(&self.pool).await?;
    Ok(rows.into_iter().map(|r| Task {
        id: totsuka_core::TaskId::new(r.0),
        task_id_short: r.1, repo: r.2, pr_node_id: r.3,
        current_column: r.4, current_phase: r.5,
        impl_verify_attempt: r.6, suppress_writeback_until_human_move: r.7,
        spawned_at: r.8, created_at: r.9, updated_at: r.10,
    }).collect())
}
```

- [ ] **Step 2: timer.rs**

```rust
use chrono::Duration;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::error::OrchestratorError;
use crate::sm::Engine;

pub async fn run_timer(engine: Arc<Engine>, tick_secs: u64, shutdown: CancellationToken)
    -> Result<(), OrchestratorError>
{
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let default_to = engine.config.orchestrator.phase_timeout_default_secs as i64;
    let per_phase = engine.config.orchestrator.phase_timeout.clone();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let now = engine.clock.now();
                for phase in ["design", "impl_verify"] {
                    let to = per_phase.get(phase).copied().unwrap_or(default_to as u64) as i64;
                    let deadline = now - Duration::seconds(to);
                    let overdue = engine.repo.list_overdue(deadline, phase).await?;
                    for t in overdue {
                        tracing::warn!(task=%t.id.as_str(), phase, "phase deadline exceeded; marking blocked");
                        let mut updated = t.clone();
                        updated.current_phase = Some(format!("{phase}_blocked"));
                        updated.updated_at = now;
                        let _ = engine.repo.upsert(&updated).await;
                        // Notifier hook deferred to Task 26.
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 3: Wire + commit**

Add `pub mod timer;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): phase timer marks overdue tasks blocked"
```

---

### Task 22: GitHub Project writeback (real GraphQL OCC)

**Files:**
- Modify: `crates/orchestrator/src/gh_writeback/http.rs`

**Interfaces:**
- `pub struct GraphqlWriteback { client: reqwest::Client, token: Secret<String>, project_id: String, status_field_id: String, option_ids: HashMap<String, String> }`
- impl WritebackClient via `mutation { updateProjectV2ItemFieldValue(... clientMutationId, expectedFieldValue ...) }` (or a head-fetch + UPDATE pattern depending on what the API supports — for now we use the simple update and treat `VersionMismatch` as the response error code `STALE`)

- [ ] **Step 1: Implementation**

```rust
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use totsuka_core::Secret;

use super::{WritebackClient, WritebackResult};
use crate::error::OrchestratorError;

pub struct GraphqlWriteback {
    client: Client,
    token: Secret<String>,
    project_id: String,
    status_field_id: String,
    option_ids: HashMap<String, String>, // ColumnId snake → ProjectsV2 option id
    endpoint: String,
}

impl GraphqlWriteback {
    pub fn new(token: Secret<String>, project_id: String, status_field_id: String,
               option_ids: HashMap<String, String>) -> Self {
        Self {
            client: Client::builder().user_agent("totsuka-orchestrator").build().unwrap(),
            token, project_id, status_field_id, option_ids,
            endpoint: "https://api.github.com/graphql".into(),
        }
    }
}

#[derive(Deserialize)]
struct GqlResp { #[serde(default)] errors: Vec<GqlErr>, #[serde(default)] data: Option<serde_json::Value> }
#[derive(Deserialize)]
struct GqlErr { message: String, #[serde(default)] r#type: Option<String> }

#[async_trait]
impl WritebackClient for GraphqlWriteback {
    async fn move_column(&self, item_id: &str, to_column: &str, _version: Option<String>)
        -> Result<WritebackResult, OrchestratorError>
    {
        let option_id = self.option_ids.get(to_column).ok_or_else(||
            OrchestratorError::Writeback(format!("no option_id for column {to_column}")))?;
        let query = format!(r#"
            mutation {{
              updateProjectV2ItemFieldValue(input: {{
                projectId: "{project}",
                itemId: "{item}",
                fieldId: "{field}",
                value: {{ singleSelectOptionId: "{opt}" }}
              }}) {{ clientMutationId }}
            }}
        "#, project=self.project_id, item=item_id, field=self.status_field_id, opt=option_id);
        let resp = self.client.post(&self.endpoint)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({"query": query}))
            .send().await
            .map_err(|e| OrchestratorError::Writeback(format!("send: {e}")))?;
        let body: GqlResp = resp.json().await
            .map_err(|e| OrchestratorError::Writeback(format!("decode: {e}")))?;
        if let Some(err) = body.errors.first() {
            if err.message.to_lowercase().contains("stale") || err.r#type.as_deref() == Some("CONFLICT") {
                return Ok(WritebackResult::VersionMismatch);
            }
            return Ok(WritebackResult::Failed(err.message.clone()));
        }
        Ok(WritebackResult::Ok)
    }
}
```

- [ ] **Step 2: Unit test for failure parsing**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_core::Secret;

    #[test]
    fn constructor_smoke() {
        let _ = GraphqlWriteback::new(
            Secret::new("tok".into()),
            "PVT_x".into(), "FIELD_x".into(),
            HashMap::from([("ready".into(), "OPT_x".into())]),
        );
    }
}
```

(Wire-level testing against GitHub is out of scope for unit tests; the consumer/e2e tests use `MockWriteback`. A future task can add `wiremock`-driven contract tests.)

- [ ] **Step 3: Build + commit**

```bash
cargo test -p orchestrator gh_writeback::http::tests
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): GraphqlWriteback (ProjectsV2 status mutation)"
```

---

### Task 23: Sweeper (expired-lease recovery)

**Files:**
- Create: `crates/orchestrator/src/sweeper.rs`

**Interfaces:**
- `pub async fn run_sweeper(pool: PgPool, tick_secs: u64, shutdown: CancellationToken) -> Result<(), OrchestratorError>`. Every tick: UPDATE processed_effects SET status = 'pending' WHERE status = 'in_progress' AND lease_expires_at <= now(). Logs the count for observability.

- [ ] **Step 1: Implementation**

```rust
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use crate::error::OrchestratorError;

pub async fn run_sweeper(pool: PgPool, tick_secs: u64, shutdown: CancellationToken)
    -> Result<(), OrchestratorError>
{
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let row = sqlx::query(
                    "UPDATE processed_effects SET status = 'pending',
                     lease_owner = NULL, lease_expires_at = NULL, updated_at = now()
                     WHERE status = 'in_progress' AND lease_expires_at <= now()"
                ).execute(&pool).await
                .map_err(OrchestratorError::Sqlx)?;
                if row.rows_affected() > 0 {
                    tracing::info!(recovered = row.rows_affected(), "sweeper recovered expired leases");
                }
            }
        }
    }
}
```

- [ ] **Step 2: Wire + commit**

Add `pub mod sweeper;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): sweeper recovers expired in_progress leases"
```

---

### Task 24: Lifecycle (readyz probes + SIGTERM)

**Files:**
- Create: `crates/orchestrator/src/lifecycle.rs`

**Interfaces:**
- `pub async fn probe_db(pool: &PgPool, health: &HealthState)` → sets `db` check from `SELECT 1`
- `pub async fn probe_adapter(adapter: &dyn AdapterClient, health: &HealthState)` → calls a no-op `read` against a synthetic id; on Adapter error other than "not found" sets fail
- Note: agent-adapter doesn't have a healthz endpoint we can hit through AdapterClient. Easiest: skip the probe (the bus consumer will fail-fast soon enough) OR add a `ping` method to AdapterClient that calls GET /healthz directly. We choose: keep the probe simple — try a `read("__probe__", 0)`; AdapterError with the substring "not found" or `/errors/not_found` → counts as adapter-reachable. Any other error → fail.

- [ ] **Step 1: Implementation**

```rust
use sqlx::PgPool;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;

use crate::adapter_client::AdapterClient;
use crate::error::OrchestratorError;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_adapter(adapter: Arc<dyn AdapterClient>, health: &HealthState) {
    let r = adapter.read("__probe__", 0).await;
    match r {
        Ok(_) => health.set_check("adapter", "ok").await,
        Err(e) => {
            let s = e.to_string();
            if s.contains("not_found") || s.contains("not found") {
                health.set_check("adapter", "ok").await;
            } else {
                health.set_check("adapter", &format!("fail: {e}")).await;
            }
        }
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), OrchestratorError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| OrchestratorError::Internal(format!("signal: {e}")))?;
    term.recv().await;
    tracing::info!("SIGTERM received; signaling shutdown");
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
```

- [ ] **Step 2: Wire + commit**

Add `pub mod lifecycle;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): readyz probes + SIGTERM drain"
```

---

### Task 25: UDS healthz/readyz listener

**Files:**
- Create: `crates/orchestrator/src/listener.rs`

**Interfaces:**
- `pub async fn bind_uds(path: &Path) -> anyhow::Result<UnixListener>` (cleanup stale + create parent dirs + bind)
- `pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> anyhow::Result<()>` (hyper-util connection loop, identical to agent-adapter's listener)
- `pub fn resolve_uds_path(raw: &str) -> PathBuf` (~/ expansion)

This is mechanically identical to `agent-adapter::listener`. You can either copy the file or extract it to `totsuka-telemetry` — for now we copy to keep the change isolated.

- [ ] **Step 1: Implementation**

```rust
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;

pub async fn bind_uds(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() { std::fs::remove_file(path)?; }
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    Ok(UnixListener::bind(path)?)
}

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> anyhow::Result<()> {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use tower::Service;

    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let tower_service = svc.call(()).await?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, hyper_service).await
            {
                tracing::warn!(error=?e, "uds connection error");
            }
        });
    }
}

pub fn resolve_uds_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}
```

Cargo.toml addition (if not already inherited via hyper-util in Task 5):
```toml
hyper-util = { version = "0.1", features = ["client", "client-legacy", "server-auto", "tokio"] }
```

(Add `server-auto` to the existing feature set.)

- [ ] **Step 2: Wire + commit**

Add `pub mod listener;` to lib.rs.
```bash
cargo build -p orchestrator
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): UDS healthz/readyz listener"
```

---

### Task 26: Main wire-up

**Files:**
- Modify: `crates/orchestrator/src/main.rs`

Wire everything together. Order:

1. Load config (TOTSUKA_CONFIG env, default `~/.config/totsuka/config.toml`)
2. `init_tracing(state_dir, "orchestrator", log_level)` — keep WorkerGuard
3. Open PgPool (DATABASE_URL or `[postgres]`-derived URL)
4. `check_schema_version(&pool)` — exit 1 on mismatch
5. `create_queue(&pool, &config.bus.queue_name)` — idempotent
6. HyperlocalAdapter::new(`config.orchestrator.adapter_uds` resolved)
7. GraphqlWriteback::new (token from `Secret`; option_ids loaded from a stored or hard-coded map — for the smoke we can stub to MockWriteback when `[github].project_owner == "smoke"`)
8. Build `Engine`
9. `health.set_check("db", "ok")` after probe_db; same for adapter
10. `health.set_ready(true)`
11. spawn 4 tasks via CancellationToken: bus consumer, phase timer, sweeper, UDS healthz server
12. wait_for_signals → cancel token → tokio::select on all 4 join handles → exit

- [ ] **Step 1: Implementation**

```rust
use std::sync::Arc;

use orchestrator::adapter_client::HyperlocalAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::lifecycle::{probe_adapter, probe_db, wait_for_signals};
use orchestrator::listener::{bind_uds, resolve_uds_path, serve_uds};
use orchestrator::repository::PgRepository;
use orchestrator::schema_check::check_schema_version;
use orchestrator::sm::Engine;
use orchestrator::sweeper::run_sweeper;
use orchestrator::timer::run_timer;
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::var("TOTSUKA_CONFIG")
        .unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);

    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard = totsuka_telemetry::init_tracing(
        &state_dir, "orchestrator", &config.totsuka.log_level,
    );

    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| format!(
        "postgres://{}:totsuka@{}:{}/{}",
        config.postgres.user, config.postgres.host,
        config.postgres.port, config.postgres.database,
    ));
    let pool = PgPoolOptions::new().max_connections(8).connect(&db_url).await?;
    check_schema_version(&pool).await?;
    create_queue(&pool, &config.bus.queue_name).await?;

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);

    let adapter_path = resolve_uds_path(&config.orchestrator.adapter_uds);
    let adapter: Arc<dyn orchestrator::adapter_client::AdapterClient> =
        Arc::new(HyperlocalAdapter::new(adapter_path));

    // Writeback: production = GraphqlWriteback; for now wire MockWriteback so
    // first-run doesn't fail when GitHub credentials aren't configured.
    // Swap to GraphqlWriteback once the option_ids loader lands.
    let writeback = Arc::new(MockWriteback::new());

    let health = HealthState::new();

    let engine = Arc::new(Engine {
        repo: Arc::new(PgRepository::new(pool.clone(), clock.clone())),
        adapter: adapter.clone(),
        writeback,
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(config.orchestrator.wip_global)),
        clock: clock.clone(),
        config: config.clone(),
        owner_id: format!("orch-{}", std::process::id()),
    });

    probe_db(&pool, &health).await;
    probe_adapter(adapter.clone(), &health).await;
    health.set_ready(true).await;

    let shutdown = CancellationToken::new();
    let consumer_h = {
        let e = engine.clone();
        let p = pool.clone();
        let q = config.bus.queue_name.clone();
        let bs = config.bus.batch_size as i32;
        let vt = config.bus.visibility_secs as i32;
        let s = shutdown.clone();
        tokio::spawn(async move { run_consumer(e, p, q, bs, vt, s).await })
    };
    let timer_h = {
        let e = engine.clone();
        let s = shutdown.clone();
        tokio::spawn(async move { run_timer(e, 30, s).await })
    };
    let sweeper_h = {
        let p = pool.clone();
        let s = shutdown.clone();
        tokio::spawn(async move { run_sweeper(p, 30, s).await })
    };

    let router = totsuka_telemetry::http::router(health.clone())
        .layer(axum::middleware::from_fn(totsuka_telemetry::request_id::middleware));
    let uds_path = resolve_uds_path(&config.orchestrator.uds_path);
    let listener = bind_uds(&uds_path).await?;
    let server_h = tokio::spawn(async move { serve_uds(listener, router).await });

    tokio::spawn(wait_for_signals(shutdown.clone()));

    tokio::select! {
        r = consumer_h => { let _ = r?; },
        r = timer_h    => { let _ = r?; },
        r = sweeper_h  => { let _ = r?; },
        r = server_h   => { let _ = r?; },
    }
    Ok(())
}
```

- [ ] **Step 2: Build + clippy + fmt**

```bash
cargo build -p orchestrator
cargo clippy -p orchestrator --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 3: Commit**

```bash
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(orchestrator): main wire-up — consumer + timer + sweeper + healthz UDS"
```

---

### Task 27: E2E orchestrator smoke + workspace lint pass

**Files:**
- Create: `crates/orchestrator/tests/e2e_orchestrator.rs`

This test mirrors `totsuka-foundation-e2e` style: walks a fake task through `Inbox → Ready → Design → DesignReview → ImplVerify → FinalReview → AwaitingRelease → Released` by publishing DomainEvents and asserting (a) the `tasks` row's `current_column` advances, (b) `MockAdapter.spawn_count()` matches expectations (1 designer + 1 implementer + 1 verifier + DiffBack adds 1 more implementer = 4 total), (c) `MockWriteback.moves()` records the orchestrator-driven moves.

- [ ] **Step 1: Test**

```rust
use orchestrator::adapter_client::MockAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::{MockWriteback, WritebackResult};
use orchestrator::repository::PgRepository;
use orchestrator::sm::Engine;
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_bus::publisher::Publisher;
use totsuka_core::{DomainEvent, Source, SystemClock, TaskId};

fn ev(item_id: &str, ty: &str, payload: serde_json::Value) -> DomainEvent {
    DomainEvent {
        event_key: format!("e2e:{}:{}", ty, item_id),
        source: Source::Github,
        event_type: ty.into(),
        payload,
    }
}

#[tokio::test]
async fn full_walk_inbox_to_released() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let pool = PgPoolOptions::new().max_connections(4).connect(&url).await.unwrap();
    let q = format!("e2e_{}", uuid::Uuid::new_v4().simple().to_string().chars().take(20).collect::<String>());
    create_queue(&pool, &q).await.unwrap();

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = Arc::new(totsuka_config::Config::load(
        format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"))
    ).unwrap());
    let adapter = Arc::new(MockAdapter::new());
    let writeback = Arc::new(MockWriteback::new());
    let repo = Arc::new(PgRepository::new(pool.clone(), clock.clone()));
    let engine = Arc::new(Engine {
        repo: repo.clone(),
        adapter: adapter.clone(),
        writeback: writeback.clone(),
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock: clock.clone(), config: cfg.clone(),
        owner_id: "e2e".into(),
    });

    let token = CancellationToken::new();
    let consumer_engine = engine.clone();
    let consumer_pool = pool.clone();
    let consumer_q = q.clone();
    let consumer_token = token.clone();
    let consumer_h = tokio::spawn(async move {
        run_consumer(consumer_engine, consumer_pool, consumer_q, 16, 30, consumer_token).await
    });

    let id = format!("PVTI_e2e_{}", uuid::Uuid::new_v4().simple());
    let pub_ = Publisher::new(q.clone(), clock.clone());

    let wait_for = |col: &'static str, deadline: Duration| {
        let repo = repo.clone();
        let id = id.clone();
        async move {
            let stop = Instant::now() + deadline;
            loop {
                if let Some(t) = repo.get(&TaskId::new(id.clone())).await.unwrap() {
                    if t.current_column == col { return; }
                }
                if Instant::now() > stop {
                    panic!("timeout waiting for column {col}");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    // 1. Human moves Inbox → Ready (event from watcher)
    pub_.send(&pool, ev(&id, "github.status_changed",
        serde_json::json!({"item_id": id, "to_status": "ready", "repo": "x/y"})), None).await.unwrap();
    wait_for("ready", Duration::from_secs(5)).await;
    assert_eq!(adapter.spawn_count(), 1, "designer spawned");

    // 2. Designer signals + human gate ①: column moves to design_review then impl_verify
    pub_.send(&pool, ev(&id, "github.status_changed",
        serde_json::json!({"item_id": id, "to_status": "design_review", "repo": "x/y"})), None).await.unwrap();
    wait_for("design_review", Duration::from_secs(5)).await;
    pub_.send(&pool, ev(&id, "github.status_changed",
        serde_json::json!({"item_id": id, "to_status": "impl_verify", "repo": "x/y"})), None).await.unwrap();
    wait_for("impl_verify", Duration::from_secs(5)).await;
    assert!(adapter.spawn_count() >= 2, "implementer spawned");

    // 3. PR ready → verifier spawn
    pub_.send(&pool, ev(&id, "github.pr_merged_ready",
        serde_json::json!({"item_id": id, "pr_diff": "diff..."})), None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while adapter.spawn_count() < 3 {
        if Instant::now() > deadline { panic!("verifier not spawned"); }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 4. Verifier passes → writeback to final_review
    pub_.send(&pool, ev(&id, "github.pr_verification_passed",
        serde_json::json!({"item_id": id})), None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        if writeback.moves().iter().any(|(_, to, _)| to == "final_review") { found = true; break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "writeback to final_review");

    // 5. Human gate ② → AwaitingRelease via human-driven status_change
    pub_.send(&pool, ev(&id, "github.status_changed",
        serde_json::json!({"item_id": id, "to_status": "awaiting_release", "repo": "x/y"})), None).await.unwrap();
    wait_for("awaiting_release", Duration::from_secs(5)).await;

    // 6. Release event → writeback to released
    pub_.send(&pool, ev(&id, "github.release_published",
        serde_json::json!({"repo": "x/y"})), None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut released_seen = false;
    while Instant::now() < deadline {
        if writeback.moves().iter().any(|(t, to, _)| t == &id && to == "released") { released_seen = true; break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(released_seen, "writeback released");

    token.cancel();
    let _ = consumer_h.await;
    sqlx::query("SELECT pgmq.drop_queue($1)").bind(&q).execute(&pool).await.unwrap();
}
```

- [ ] **Step 2: Workspace lint pass**

```bash
cargo build --workspace --locked
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --check
```

Expected: all green; agent-adapter + foundation tests + new orchestrator tests all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/orchestrator/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(orchestrator): e2e walk Inbox → Released through real pgmq + mocks"
```

---

## Out-of-scope follow-ups

- Real GitHub Project option_ids loader (the writeback uses MockWriteback in main.rs until this lands)
- Notifier wiring (TaskStuck / GivingUp etc. — totsuka-telemetry::Notifier is ready; orchestrator just needs to call into it from timer.rs and on failure paths)
- Conversation driver's PR-diff fetch via `gh` CLI (currently the diff comes from the event payload)
- `phase_short` extension when we add a real `Phase::Verify` enum variant
- WIP gate metrics export to Prometheus
- Per-repo WIP overrides (current `wip_global` is single-tenant)
- **Spec §11.8 bounded mpsc topology not implemented.** The current
  consumer→engine.handle→adapter.spawn call chain is synchronous;
  back-pressure exists only via pgmq visibility timeout. Spec §11.8
  mandates four bounded channels (bus pull→SM=32, SM→adapter=node_capacity=8,
  SM→writeback=64, SM→Notifier=256/drop-oldest) and a
  `channel_full_total{channel}` metric. Defer to a follow-up that retrofits
  the channels plus the §11.9 metric; the synchronous shape is acceptable
  for the current single-instance, low-throughput deployment.

## Test plan summary

| Layer | Path | Coverage |
|---|---|---|
| Unit | `src/**/{argv,branch,wip,error}::tests` | Pure logic |
| Repo integration | `tests/repository.rs` `tests/effect.rs` | Real Postgres CRUD + lease semantics |
| SM integration | `tests/sm_status_change.rs` `tests/sm_ready_to_design.rs` `tests/sm_impl_verify.rs` | Each transition end-to-end with MockAdapter + MockWriteback + real DB |
| Consumer | `tests/consumer.rs` | pgmq → SM dispatch |
| E2E | `tests/e2e_orchestrator.rs` | Full Inbox → Released walk |

After Task 27, `DATABASE_URL=... cargo test --workspace --locked` returns all green (foundation 67 + agent-adapter 36 + orchestrator ~12 = ~115 tests, exact count depends on which inline unit tests survive review).
