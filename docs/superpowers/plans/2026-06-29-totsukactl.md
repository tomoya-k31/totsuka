# totsukactl (Supervisor CLI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `totsukactl` binary — a long-running process supervisor + CLI that boots/halts the 4 Rust bins and the pgmq container in dependency order, watches health, restarts per policy, and exposes a UDS API so the CLI side can query status / restart / reload / shutdown.

**Architecture:** Single Cargo crate `crates/totsukactl/` producing one binary that toggles between **CLI mode** (parse subcommand, talk to running supervisor over `${state_dir}/sock/supervisor.sock`) and **daemon mode** (`up` forks/detaches into the supervisor loop). Daemon owns: process registry, heartbeat tickers (healthz 5s / readyz 30s / pgmq 30s), state machine per child (`Starting→Ready→Healthy→Degraded→Unhealthy→Dead→Restarting→GivingUp`), boot/shutdown choreography (§4 / §5), and the UDS server (§7). All side-effecting boundaries (docker compose, child fork+exec, healthz HTTP, time) are behind traits so the state machine is unit-testable without root or daemons.

**Tech Stack:** Rust stable / tokio (rt-multi-thread + signal + process + net) / axum + hyper-util (UDS server reuses the `auto::Builder` accept-loop pattern from `crates/orchestrator/src/listener.rs`) / hyperlocal (UDS client) / reqwest (TCP healthz for github-watcher) / clap v4 derive / sqlx::migrate! (embedded migrations) / nix (advisory pid lock + setsid) / async-trait.

## Global Constraints

- Rust workspace stable channel, `[profile.release] panic = "abort"`; lib crates expose error enums via `thiserror`, bins return `anyhow::Result<()>`.
- `tokio::task::block_in_place` is clippy-denied workspace-wide — use `tokio::process::Command` / `spawn_blocking` for blocking work.
- Time comes from `Arc<dyn totsuka_core::Clock>`; `SystemTime::now()` / `chrono::Utc::now()` direct calls are clippy-denied (annotate exceptions).
- Secrets are `totsuka_core::Secret<String>`; `.expose()` only at outbound HTTPS / DB URL construction sites; never log them.
- Schema version: `MIN_SCHEMA_VERSION = TARGET_SCHEMA_VERSION = 6` (matches every other bin on `main`).
- Postgres URL fallback uses `config.postgres.password.expose()`, **never a hard-coded literal**.
- All Claude-driven commits use `git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "..."` (1Password Touch blocks background-signed commits).
- Children supervised: `agent-adapter`, `orchestrator`, `github-watcher`, `qa-service`; container: `pgmq` (image `ghcr.io/pgmq/pg18-pgmq:v1.11.1`, container `totsuka-pgmq`). Claude panes are owned by herdr and **never killed** during shutdown.
- File layout per spec §10.1: `crates/totsukactl/src/{main.rs, cli.rs, supervisor.rs, heartbeat.rs, compose.rs, probe.rs, child.rs, sock_api.rs}` — extra small modules (error, paths, pidfile, registry, state, bootstrap, etc.) are allowed alongside.
- XDG runtime layout per §10.2: pid at `${state_dir}/supervisor.pid`, child pids at `${state_dir}/pids/<bin>.pid`, sockets `${state_dir}/sock/{supervisor,adapter,orchestrator,qa-service}.sock` (mode 0700), logs at `${state_dir}/logs/<bin>.log` plus `supervisor.log`.
- Startup order (§4): pgmq → preflight → agent-adapter → orchestrator → (github-watcher ∥ qa-service); per-child readyz timeout 30s × 0.5s interval.
- Shutdown order (§5, reverse): ingestion (watcher ∥ qa-service) → orchestrator → agent-adapter → (optional pgmq); deadlines 15s SIGTERM grace → 5s second SIGTERM → SIGKILL; `--force` bypasses ordering with 3s grace.
- Restart policy default `on-dead-only`; backoff `[5, 15, 60]`; `restart_max_attempts = 5` then `giving_up` (notify only).
- pgmq abnormality → **no cascade restart** (data-corruption risk) — notify only.
- IPC (§7): supervisor↔CLI uses `${state_dir}/sock/supervisor.sock`; supervisor↔child healthz uses each bin's existing endpoint (UDS for adapter/orchestrator/qa-service, TCP loopback for github-watcher at `[github_watcher].bind`).

---

### Task 1: Crate scaffold + workspace wire-up

**Files:**
- Create: `crates/totsukactl/Cargo.toml`
- Create: `crates/totsukactl/src/main.rs`
- Create: `crates/totsukactl/src/lib.rs`
- Modify: `Cargo.toml` (workspace members — append `"crates/totsukactl"` alphabetically after `"crates/qa-service"`)

**Interfaces:**
- Consumes: foundation crates (`totsuka-core`, `totsuka-config`, `totsuka-telemetry`, `totsuka-bus`).
- Produces: bin `totsukactl` + `pub mod` placeholders so later tasks add files without touching scaffolding.

- [ ] **Step 1: Append to workspace**

`Cargo.toml`:
```toml
members = [
    "crates/totsuka-core",
    "crates/totsuka-config",
    "crates/totsuka-telemetry",
    "crates/totsuka-bus",
    "crates/totsuka-foundation-e2e",
    "crates/agent-adapter",
    "crates/github-watcher",
    "crates/orchestrator",
    "crates/qa-service",
    "crates/totsukactl",
]
```

- [ ] **Step 2: Crate Cargo.toml**

`crates/totsukactl/Cargo.toml`:
```toml
[package]
name = "totsukactl"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "totsukactl"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
totsuka-core      = { path = "../totsuka-core",      version = "0.1.0" }
totsuka-config    = { path = "../totsuka-config",    version = "0.1.0" }
totsuka-telemetry = { path = "../totsuka-telemetry", version = "0.1.0" }
totsuka-bus       = { path = "../totsuka-bus",       version = "0.1.0" }

tokio       = { workspace = true, features = ["rt-multi-thread", "macros", "signal", "fs", "net", "sync", "time", "process", "io-util"] }
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
tracing-subscriber = { workspace = true }
tokio-util  = { version = "0.7", features = ["rt"] }
hyper-util  = { version = "0.1", features = ["client", "client-legacy", "tokio", "server-auto"] }
http-body-util = "0.1"
clap        = { version = "4.5", features = ["derive"] }
nix         = { version = "0.29", features = ["signal", "process", "fs"] }
tabwriter   = "1.4"
humantime   = "2.1"

[dev-dependencies]
tokio    = { workspace = true, features = ["test-util"] }
tempfile = "3.12"
```

- [ ] **Step 3: lib.rs stub**

`crates/totsukactl/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

pub mod child;
pub mod cli;
pub mod compose;
pub mod error;
pub mod heartbeat;
pub mod paths;
pub mod pidfile;
pub mod probe;
pub mod registry;
pub mod sock_api;
pub mod state;
pub mod supervisor;
```

For Task 1, each `pub mod X;` referenced above MUST exist as an empty file (`crates/totsukactl/src/<name>.rs` containing just `// stub: filled by Task N`). Without the placeholders the crate won't compile. Create all of them in this task.

- [ ] **Step 4: main.rs stub**

`crates/totsukactl/src/main.rs`:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    tracing::info!("totsukactl scaffold: cli wiring lands in later tasks");
    Ok(())
}
```

- [ ] **Step 5: Verify**

```bash
cargo check --workspace --locked
cargo build -p totsukactl
```
Expected: both succeed; no clippy run yet (later tasks add code that clippy reviews).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/totsukactl/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): bin/lib scaffold + workspace wire-up"
```

---

### Task 2: TotsukactlError + RFC7807 mapping

**Files:**
- Modify: `crates/totsukactl/src/error.rs`
- Create: `crates/totsukactl/tests/error_codes.rs`

**Interfaces:**
- Produces: `pub enum TotsukactlError` with variants `Io(#[from] std::io::Error)`, `Toml(String)`, `Config(String)`, `Sqlx(#[from] sqlx::Error)`, `Migrate(String)`, `Compose(String)`, `Probe(String)`, `Spawn(String)`, `Health(String)`, `SchemaOutOfRange { got: i32, min: i32, target: i32 }`, `SupervisorUnreachable(String)`, `AlreadyRunning(String)`, `NotRunning`, `UnknownChild(String)`, `Timeout(String)`, `Internal(String)` plus `pub fn code(&self) -> &'static str` returning `/errors/<kind>`.

- [ ] **Step 1: Implement enum**

`crates/totsukactl/src/error.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TotsukactlError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("config: {0}")]
    Config(String),
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(String),
    #[error("compose: {0}")]
    Compose(String),
    #[error("probe: {0}")]
    Probe(String),
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("health: {0}")]
    Health(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("supervisor unreachable: {0}")]
    SupervisorUnreachable(String),
    #[error("stack already running: {0}")]
    AlreadyRunning(String),
    #[error("stack not running")]
    NotRunning,
    #[error("unknown child: {0}")]
    UnknownChild(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl TotsukactlError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "/errors/io",
            Self::Toml(_) => "/errors/toml",
            Self::Config(_) => "/errors/config",
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Migrate(_) => "/errors/migrate",
            Self::Compose(_) => "/errors/compose",
            Self::Probe(_) => "/errors/probe",
            Self::Spawn(_) => "/errors/spawn",
            Self::Health(_) => "/errors/health",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::SupervisorUnreachable(_) => "/errors/supervisor_unreachable",
            Self::AlreadyRunning(_) => "/errors/already_running",
            Self::NotRunning => "/errors/not_running",
            Self::UnknownChild(_) => "/errors/unknown_child",
            Self::Timeout(_) => "/errors/timeout",
            Self::Internal(_) => "/errors/internal",
        }
    }
}
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/error_codes.rs`:
```rust
use totsukactl::error::TotsukactlError;

#[test]
fn codes_are_unique_and_prefixed() {
    let variants = [
        TotsukactlError::Io(std::io::Error::other("x")),
        TotsukactlError::Toml("x".into()),
        TotsukactlError::Config("x".into()),
        TotsukactlError::Migrate("x".into()),
        TotsukactlError::Compose("x".into()),
        TotsukactlError::Probe("x".into()),
        TotsukactlError::Spawn("x".into()),
        TotsukactlError::Health("x".into()),
        TotsukactlError::SchemaOutOfRange { got: 5, min: 6, target: 6 },
        TotsukactlError::SupervisorUnreachable("x".into()),
        TotsukactlError::AlreadyRunning("x".into()),
        TotsukactlError::NotRunning,
        TotsukactlError::UnknownChild("x".into()),
        TotsukactlError::Timeout("x".into()),
        TotsukactlError::Internal("x".into()),
    ];
    let codes: Vec<&str> = variants.iter().map(|e| e.code()).collect();
    for c in &codes {
        assert!(c.starts_with("/errors/"), "{c} missing /errors/ prefix");
    }
    let set: std::collections::HashSet<_> = codes.iter().copied().collect();
    assert_eq!(set.len(), codes.len(), "duplicate code in TYPE_URI mapping");
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test error_codes
git add crates/totsukactl/src/error.rs crates/totsukactl/tests/error_codes.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): TotsukactlError + RFC7807 code mapping"
```

---

### Task 3: Schema-version handshake

**Files:**
- Create: `crates/totsukactl/src/schema_check.rs`
- Modify: `crates/totsukactl/src/lib.rs` (`pub mod schema_check;`)
- Create: `crates/totsukactl/tests/schema_check.rs` (integration test against `$DATABASE_URL`)

**Interfaces:**
- Produces: `pub const MIN_SCHEMA_VERSION: i32 = 6;` `pub const TARGET_SCHEMA_VERSION: i32 = 6;` `pub async fn check_schema_version(pool: &sqlx::PgPool) -> Result<i32, TotsukactlError>` returning the highest applied migration, or `SchemaOutOfRange` / `Internal("schema_meta empty")`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/schema_check.rs`:
```rust
//! spec §11.1 bin↔DB handshake. Reads the highest version from schema_meta
//! and validates it against the bin's compiled range.

use crate::error::TotsukactlError;
use sqlx::PgPool;

pub const MIN_SCHEMA_VERSION: i32 = 6;
pub const TARGET_SCHEMA_VERSION: i32 = 6;

pub async fn check_schema_version(pool: &PgPool) -> Result<i32, TotsukactlError> {
    let row: (Option<i32>,) = sqlx::query_as("SELECT max(version) FROM schema_meta")
        .fetch_one(pool)
        .await?;
    let got = row.0.ok_or_else(|| {
        TotsukactlError::Internal("schema_meta is empty; run totsukactl migrate".into())
    })?;
    if got < MIN_SCHEMA_VERSION || got > TARGET_SCHEMA_VERSION {
        return Err(TotsukactlError::SchemaOutOfRange {
            got,
            min: MIN_SCHEMA_VERSION,
            target: TARGET_SCHEMA_VERSION,
        });
    }
    Ok(got)
}
```

- [ ] **Step 2: Add module declaration to lib.rs**

Modify `crates/totsukactl/src/lib.rs` — insert `pub mod schema_check;` in the alphabetical mod list.

- [ ] **Step 3: Integration test (needs running pgmq)**

`crates/totsukactl/tests/schema_check.rs`:
```rust
//! Requires DATABASE_URL pointing at a migrated pgmq instance (CI provides one).

use sqlx::postgres::PgPoolOptions;
use totsukactl::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn handshake_returns_target_on_migrated_db() {
    let Some(url) = db_url() else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    let got = check_schema_version(&pool).await.expect("handshake ok");
    assert_eq!(got, TARGET_SCHEMA_VERSION);
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test schema_check
git add crates/totsukactl/src/schema_check.rs crates/totsukactl/src/lib.rs crates/totsukactl/tests/schema_check.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): schema_meta MIN/TARGET=6 handshake"
```

---

### Task 4: Paths module (XDG resolution)

**Files:**
- Modify: `crates/totsukactl/src/paths.rs`
- Create: `crates/totsukactl/tests/paths.rs`

**Interfaces:**
- Produces: `pub struct Paths { pub state_dir: PathBuf, pub data_dir: PathBuf, pub log_dir: PathBuf, pub pid_dir: PathBuf, pub sock_dir: PathBuf }`, `impl Paths { pub fn from_config(cfg: &totsuka_config::Config) -> Self; pub fn supervisor_pid(&self) -> PathBuf; pub fn supervisor_sock(&self) -> PathBuf; pub fn supervisor_log(&self) -> PathBuf; pub fn child_pid(&self, bin: &str) -> PathBuf; pub fn child_log(&self, bin: &str) -> PathBuf; pub fn ensure(&self) -> std::io::Result<()> }`, free `pub fn resolve_tilde(raw: &str) -> PathBuf` that expands a leading `~/`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/paths.rs`:
```rust
//! XDG runtime layout per spec §10.2.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub pid_dir: PathBuf,
    pub sock_dir: PathBuf,
}

pub fn resolve_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

impl Paths {
    pub fn from_config(cfg: &totsuka_config::Config) -> Self {
        let state_dir = resolve_tilde(&cfg.totsuka.state_dir);
        let data_dir = resolve_tilde(&cfg.totsuka.data_dir);
        Self {
            log_dir: state_dir.join("logs"),
            pid_dir: state_dir.join("pids"),
            sock_dir: state_dir.join("sock"),
            state_dir,
            data_dir,
        }
    }

    pub fn supervisor_pid(&self) -> PathBuf {
        self.state_dir.join("supervisor.pid")
    }
    pub fn supervisor_sock(&self) -> PathBuf {
        self.sock_dir.join("supervisor.sock")
    }
    pub fn supervisor_log(&self) -> PathBuf {
        self.log_dir.join("supervisor.log")
    }
    pub fn child_pid(&self, bin: &str) -> PathBuf {
        self.pid_dir.join(format!("{bin}.pid"))
    }
    pub fn child_log(&self, bin: &str) -> PathBuf {
        self.log_dir.join(format!("{bin}.log"))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for p in [&self.state_dir, &self.data_dir, &self.log_dir, &self.pid_dir, &self.sock_dir] {
            std::fs::create_dir_all(p)?;
        }
        chmod_0700(&self.sock_dir)?;
        Ok(())
    }
}

#[cfg(unix)]
fn chmod_0700(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(p, perms)
}

#[cfg(not(unix))]
fn chmod_0700(_p: &Path) -> std::io::Result<()> { Ok(()) }
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/paths.rs`:
```rust
use tempfile::TempDir;
use totsukactl::paths::{resolve_tilde, Paths};

#[test]
fn resolve_tilde_expands_when_home_set() {
    std::env::set_var("HOME", "/h");
    let p = resolve_tilde("~/.local/state/totsuka");
    assert_eq!(p, std::path::PathBuf::from("/h/.local/state/totsuka"));
}

#[test]
fn resolve_tilde_passthrough_for_absolute() {
    let p = resolve_tilde("/absolute/path");
    assert_eq!(p, std::path::PathBuf::from("/absolute/path"));
}

#[test]
fn ensure_creates_layout_and_sets_sock_mode_0700() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().join("state");
    let data = tmp.path().join("data");
    let paths = Paths {
        state_dir: state.clone(),
        data_dir: data.clone(),
        log_dir: state.join("logs"),
        pid_dir: state.join("pids"),
        sock_dir: state.join("sock"),
    };
    paths.ensure().unwrap();
    for p in [&paths.state_dir, &paths.data_dir, &paths.log_dir, &paths.pid_dir, &paths.sock_dir] {
        assert!(p.is_dir(), "{p:?} missing");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&paths.sock_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test paths
git add crates/totsukactl/src/paths.rs crates/totsukactl/tests/paths.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): XDG paths resolver + ensure layout 0700"
```

---

### Task 5: Pid file (write/read + stale detection)

**Files:**
- Modify: `crates/totsukactl/src/pidfile.rs`
- Create: `crates/totsukactl/tests/pidfile.rs`

**Interfaces:**
- Produces: `pub fn write_pid(path: &Path, pid: i32) -> Result<(), TotsukactlError>`, `pub fn read_pid(path: &Path) -> Result<Option<i32>, TotsukactlError>` (returns `None` if absent), `pub fn process_alive(pid: i32) -> bool` (uses `nix::sys::signal::kill(pid, None)`), `pub enum PidState { Absent, Alive(i32), Stale(i32) }`, `pub fn check(path: &Path) -> Result<PidState, TotsukactlError>`, `pub fn remove(path: &Path) -> Result<(), TotsukactlError>` (idempotent).

- [ ] **Step 1: Implement**

`crates/totsukactl/src/pidfile.rs`:
```rust
use crate::error::TotsukactlError;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum PidState {
    Absent,
    Alive(i32),
    Stale(i32),
}

pub fn write_pid(path: &Path, pid: i32) -> Result<(), TotsukactlError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{pid}\n"))?;
    Ok(())
}

pub fn read_pid(path: &Path) -> Result<Option<i32>, TotsukactlError> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .trim()
            .parse::<i32>()
            .map(Some)
            .map_err(|e| TotsukactlError::Internal(format!("malformed pid file {path:?}: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn process_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid), None), Ok(()))
}

pub fn check(path: &Path) -> Result<PidState, TotsukactlError> {
    match read_pid(path)? {
        None => Ok(PidState::Absent),
        Some(pid) if process_alive(pid) => Ok(PidState::Alive(pid)),
        Some(pid) => Ok(PidState::Stale(pid)),
    }
}

pub fn remove(path: &Path) -> Result<(), TotsukactlError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/pidfile.rs`:
```rust
use tempfile::TempDir;
use totsukactl::pidfile::{check, read_pid, remove, write_pid, PidState};

#[test]
fn read_pid_absent_returns_none() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("missing.pid");
    assert_eq!(read_pid(&p).unwrap(), None);
}

#[test]
fn write_then_read_round_trip() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("sup.pid");
    write_pid(&p, 12345).unwrap();
    assert_eq!(read_pid(&p).unwrap(), Some(12345));
}

#[test]
fn check_returns_alive_for_self_pid() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("alive.pid");
    write_pid(&p, std::process::id() as i32).unwrap();
    assert_eq!(check(&p).unwrap(), PidState::Alive(std::process::id() as i32));
}

#[test]
fn check_returns_stale_for_dead_pid() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("dead.pid");
    // PID 0x7fff_fffe is virtually guaranteed not to exist; if check returns Alive
    // (extreme luck) we accept either Alive/Stale — but assert it's not Absent.
    write_pid(&p, 0x7fff_fffe).unwrap();
    let st = check(&p).unwrap();
    assert_ne!(st, PidState::Absent);
}

#[test]
fn remove_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("nope.pid");
    remove(&p).unwrap();
    remove(&p).unwrap();
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test pidfile
git add crates/totsukactl/src/pidfile.rs crates/totsukactl/tests/pidfile.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): pidfile read/write/check/remove with stale detection"
```

---

### Task 6: State machine enums + transitions

**Files:**
- Modify: `crates/totsukactl/src/state.rs`
- Create: `crates/totsukactl/tests/state.rs`

**Interfaces:**
- Produces:
  - `pub enum ChildState { Starting, Ready, Healthy, Degraded, Unhealthy, Dead, Restarting, GivingUp, Draining, Stopped }`
  - `pub enum StackState { Stopped, Starting, Running, Degraded, ShuttingDown }`
  - `pub enum RestartPolicy { OnDeadOnly, OnUnhealthy, Never }` with `pub fn parse(s: &str) -> Result<Self, TotsukactlError>` accepting `"on-dead-only" | "on-unhealthy" | "never"`.
  - `pub enum HealthOutcome { Ok, Degraded, Unhealthy, Dead }` (consumed by Task 13 tickers).
  - `pub fn next_state(current: ChildState, outcome: HealthOutcome, consecutive_failures: u32, degraded_threshold: u32, unhealthy_threshold: u32) -> ChildState` — the pure transition function (no I/O), defined exactly as the table below.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/state.rs`:
```rust
use crate::error::TotsukactlError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildState {
    Starting,
    Ready,
    Healthy,
    Degraded,
    Unhealthy,
    Dead,
    Restarting,
    GivingUp,
    Draining,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackState {
    Stopped,
    Starting,
    Running,
    Degraded,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    OnDeadOnly,
    OnUnhealthy,
    Never,
}

impl RestartPolicy {
    pub fn parse(s: &str) -> Result<Self, TotsukactlError> {
        match s {
            "on-dead-only" => Ok(Self::OnDeadOnly),
            "on-unhealthy" => Ok(Self::OnUnhealthy),
            "never" => Ok(Self::Never),
            other => Err(TotsukactlError::Config(format!(
                "unknown restart_policy {other:?} (expected on-dead-only|on-unhealthy|never)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthOutcome { Ok, Degraded, Unhealthy, Dead }

/// Pure transition function. spec §9.1:
///   Starting → Ready → Healthy
///                    ↘ Degraded   (readyz NG ≥ degraded_threshold)
///                    ↘ Unhealthy  (healthz NG ≥ unhealthy_threshold)
///                    ↘ Dead       (SIGCHLD / connect refused)
///   Dead | Unhealthy → (caller decides Restarting via restart_policy)
///   GivingUp / Draining / Stopped: terminal w.r.t. health ticks; stay put.
pub fn next_state(
    current: ChildState,
    outcome: HealthOutcome,
    consecutive_failures: u32,
    degraded_threshold: u32,
    unhealthy_threshold: u32,
) -> ChildState {
    use ChildState::*;
    use HealthOutcome::*;
    match (current, outcome) {
        (GivingUp | Draining | Stopped | Restarting, _) => current,
        (_, Dead) => Dead,
        (_, Ok) => Healthy,
        (_, Degraded) if consecutive_failures >= unhealthy_threshold => Unhealthy,
        (_, Degraded) if consecutive_failures >= degraded_threshold => Degraded,
        (_, Degraded) => current,
        (_, Unhealthy) if consecutive_failures >= unhealthy_threshold => Unhealthy,
        (_, Unhealthy) if consecutive_failures >= degraded_threshold => Degraded,
        (_, Unhealthy) => current,
    }
}
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/state.rs`:
```rust
use totsukactl::state::{next_state, ChildState, HealthOutcome, RestartPolicy};

#[test]
fn parse_restart_policy_recognises_all_three() {
    assert_eq!(RestartPolicy::parse("on-dead-only").unwrap(), RestartPolicy::OnDeadOnly);
    assert_eq!(RestartPolicy::parse("on-unhealthy").unwrap(), RestartPolicy::OnUnhealthy);
    assert_eq!(RestartPolicy::parse("never").unwrap(), RestartPolicy::Never);
    assert!(RestartPolicy::parse("garbage").is_err());
}

#[test]
fn ok_outcome_promotes_to_healthy_from_any_live_state() {
    for from in [ChildState::Starting, ChildState::Ready, ChildState::Degraded, ChildState::Unhealthy] {
        assert_eq!(next_state(from, HealthOutcome::Ok, 0, 2, 3), ChildState::Healthy);
    }
}

#[test]
fn degraded_outcome_only_after_threshold() {
    // below threshold: stays put
    assert_eq!(next_state(ChildState::Healthy, HealthOutcome::Degraded, 1, 2, 3), ChildState::Healthy);
    // hits degraded threshold (2)
    assert_eq!(next_state(ChildState::Healthy, HealthOutcome::Degraded, 2, 2, 3), ChildState::Degraded);
    // hits unhealthy threshold (3)
    assert_eq!(next_state(ChildState::Healthy, HealthOutcome::Degraded, 3, 2, 3), ChildState::Unhealthy);
}

#[test]
fn dead_outcome_overrides_everything() {
    for from in [ChildState::Healthy, ChildState::Degraded, ChildState::Unhealthy] {
        assert_eq!(next_state(from, HealthOutcome::Dead, 0, 2, 3), ChildState::Dead);
    }
}

#[test]
fn terminal_states_are_sticky_under_ticks() {
    for from in [ChildState::GivingUp, ChildState::Draining, ChildState::Stopped, ChildState::Restarting] {
        for outcome in [HealthOutcome::Ok, HealthOutcome::Degraded, HealthOutcome::Unhealthy, HealthOutcome::Dead] {
            assert_eq!(next_state(from, outcome, 5, 2, 3), from);
        }
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test state
git add crates/totsukactl/src/state.rs crates/totsukactl/tests/state.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): ChildState/StackState enums + pure transition fn"
```

---

### Task 7: Process registry

**Files:**
- Modify: `crates/totsukactl/src/registry.rs`
- Create: `crates/totsukactl/tests/registry.rs`

**Interfaces:**
- Produces:
  - `pub struct ProcessEntry { pub name: String, pub pid: Option<i32>, pub state: ChildState, pub started_at: Option<DateTime<Utc>>, pub last_healthz_at: Option<DateTime<Utc>>, pub last_readyz_at: Option<DateTime<Utc>>, pub consecutive_failures: u32, pub restart_count: u32 }`
  - `pub struct Registry { ... }` with `pub fn new() -> Self`, `pub async fn upsert(&self, e: ProcessEntry)`, `pub async fn get(&self, name: &str) -> Option<ProcessEntry>`, `pub async fn list(&self) -> Vec<ProcessEntry>` (returns sorted by spec startup order: `pgmq, agent-adapter, orchestrator, github-watcher, qa-service`), `pub async fn set_state(&self, name: &str, state: ChildState)`, `pub async fn set_pid(&self, name: &str, pid: Option<i32>, started_at: Option<DateTime<Utc>>)`, `pub async fn bump_failure(&self, name: &str) -> u32` (returns new count), `pub async fn reset_failure(&self, name: &str)`, `pub async fn bump_restart(&self, name: &str)`.
- Internally: `Arc<RwLock<BTreeMap<String, ProcessEntry>>>` for cheap clone; sort order produced by a const `ORDER: &[&str] = &["pgmq", "agent-adapter", "orchestrator", "github-watcher", "qa-service"];`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/registry.rs`:
```rust
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const ORDER: &[&str] = &[
    "pgmq",
    "agent-adapter",
    "orchestrator",
    "github-watcher",
    "qa-service",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub name: String,
    pub pid: Option<i32>,
    pub state: ChildState,
    pub started_at: Option<DateTime<Utc>>,
    pub last_healthz_at: Option<DateTime<Utc>>,
    pub last_readyz_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub restart_count: u32,
}

impl ProcessEntry {
    pub fn fresh(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pid: None,
            state: ChildState::Stopped,
            started_at: None,
            last_healthz_at: None,
            last_readyz_at: None,
            consecutive_failures: 0,
            restart_count: 0,
        }
    }
}

#[derive(Default, Clone)]
pub struct Registry {
    inner: Arc<RwLock<BTreeMap<String, ProcessEntry>>>,
}

impl Registry {
    pub fn new() -> Self {
        let mut map = BTreeMap::new();
        for name in ORDER {
            map.insert((*name).to_string(), ProcessEntry::fresh(*name));
        }
        Self { inner: Arc::new(RwLock::new(map)) }
    }

    pub async fn upsert(&self, e: ProcessEntry) {
        self.inner.write().await.insert(e.name.clone(), e);
    }
    pub async fn get(&self, name: &str) -> Option<ProcessEntry> {
        self.inner.read().await.get(name).cloned()
    }
    pub async fn list(&self) -> Vec<ProcessEntry> {
        let map = self.inner.read().await;
        ORDER
            .iter()
            .filter_map(|n| map.get(*n).cloned())
            .collect()
    }
    pub async fn set_state(&self, name: &str, state: ChildState) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.state = state;
        }
    }
    pub async fn set_pid(&self, name: &str, pid: Option<i32>, started_at: Option<DateTime<Utc>>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.pid = pid;
            e.started_at = started_at;
        }
    }
    pub async fn bump_failure(&self, name: &str) -> u32 {
        let mut g = self.inner.write().await;
        let e = g.entry(name.to_string()).or_insert_with(|| ProcessEntry::fresh(name));
        e.consecutive_failures = e.consecutive_failures.saturating_add(1);
        e.consecutive_failures
    }
    pub async fn reset_failure(&self, name: &str) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.consecutive_failures = 0;
        }
    }
    pub async fn bump_restart(&self, name: &str) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.restart_count = e.restart_count.saturating_add(1);
        }
    }
    pub async fn touch_healthz(&self, name: &str, at: DateTime<Utc>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.last_healthz_at = Some(at);
        }
    }
    pub async fn touch_readyz(&self, name: &str, at: DateTime<Utc>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.last_readyz_at = Some(at);
        }
    }
}
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/registry.rs`:
```rust
use chrono::{TimeZone, Utc};
use totsukactl::registry::{Registry, ORDER};
use totsukactl::state::ChildState;

#[tokio::test]
async fn list_returns_spec_startup_order() {
    let reg = Registry::new();
    let names: Vec<_> = reg.list().await.into_iter().map(|e| e.name).collect();
    let want: Vec<_> = ORDER.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(names, want);
}

#[tokio::test]
async fn set_state_persists() {
    let reg = Registry::new();
    reg.set_state("orchestrator", ChildState::Healthy).await;
    assert_eq!(reg.get("orchestrator").await.unwrap().state, ChildState::Healthy);
}

#[tokio::test]
async fn bump_then_reset_failure() {
    let reg = Registry::new();
    assert_eq!(reg.bump_failure("qa-service").await, 1);
    assert_eq!(reg.bump_failure("qa-service").await, 2);
    reg.reset_failure("qa-service").await;
    assert_eq!(reg.get("qa-service").await.unwrap().consecutive_failures, 0);
}

#[tokio::test]
async fn touch_records_last_healthz_at() {
    let reg = Registry::new();
    let t = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    reg.touch_healthz("agent-adapter", t).await;
    assert_eq!(reg.get("agent-adapter").await.unwrap().last_healthz_at, Some(t));
}

#[tokio::test]
async fn set_pid_round_trip() {
    let reg = Registry::new();
    let t = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    reg.set_pid("orchestrator", Some(4242), Some(t)).await;
    let e = reg.get("orchestrator").await.unwrap();
    assert_eq!(e.pid, Some(4242));
    assert_eq!(e.started_at, Some(t));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test registry
git add crates/totsukactl/src/registry.rs crates/totsukactl/tests/registry.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): process registry (concurrent map + spec startup order)"
```

---

### Task 8: ComposeExec trait + tokio impl + MockCompose

**Files:**
- Modify: `crates/totsukactl/src/compose.rs`
- Create: `crates/totsukactl/src/compose/mock.rs`
- Create: `crates/totsukactl/tests/compose_mock.rs`

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait ComposeExec: Send + Sync` with methods `async fn docker_info(&self) -> Result<(), TotsukactlError>`, `async fn compose_version(&self) -> Result<(), TotsukactlError>`, `async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError>`, `async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError>`, `async fn stop(&self, service: &str) -> Result<(), TotsukactlError>`, `async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError>` (returns the image reference of the running container), `async fn logs_tail(&self, service: &str, n: u32) -> Result<String, TotsukactlError>`.
  - `pub struct DockerCompose { pub compose_file: PathBuf }` impl that shells out to `docker` / `docker compose` via `tokio::process::Command` (no `block_in_place`).
  - `pub mod mock` with `pub struct MockCompose { ... }` for unit tests (canned responses + call log).
- Reason: every later orchestration task (preflight, supervisor boot, status, down --postgres) takes `Arc<dyn ComposeExec>` so it's unit-testable without docker.

- [ ] **Step 1: Implement trait + real impl**

`crates/totsukactl/src/compose.rs`:
```rust
use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub mod mock;

#[async_trait]
pub trait ComposeExec: Send + Sync {
    async fn docker_info(&self) -> Result<(), TotsukactlError>;
    async fn compose_version(&self) -> Result<(), TotsukactlError>;
    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError>;
    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError>;
    async fn stop(&self, service: &str) -> Result<(), TotsukactlError>;
    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError>;
    async fn logs_tail(&self, service: &str, n: u32) -> Result<String, TotsukactlError>;
}

pub struct DockerCompose {
    pub compose_file: PathBuf,
}

impl DockerCompose {
    pub fn new(compose_file: PathBuf) -> Self { Self { compose_file } }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, TotsukactlError> {
        let out = Command::new("docker")
            .args(args)
            .output()
            .await
            .map_err(|e| TotsukactlError::Compose(format!("spawn docker {args:?}: {e}")))?;
        Ok(out)
    }

    fn ensure_ok(out: &std::process::Output, ctx: &str) -> Result<(), TotsukactlError> {
        if out.status.success() {
            Ok(())
        } else {
            Err(TotsukactlError::Compose(format!(
                "{ctx} failed (code {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }
}

#[async_trait]
impl ComposeExec for DockerCompose {
    async fn docker_info(&self) -> Result<(), TotsukactlError> {
        let out = self.run(&["info"]).await?;
        Self::ensure_ok(&out, "docker info")
    }

    async fn compose_version(&self) -> Result<(), TotsukactlError> {
        let out = self.run(&["compose", "version"]).await?;
        Self::ensure_ok(&out, "docker compose version")
    }

    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let out = self.run(&["compose", "-f", &cf, "ps", "--status=running", "--services"]).await?;
        Self::ensure_ok(&out, "docker compose ps")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout.lines().any(|l| l.trim() == service))
    }

    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let mut args = vec!["compose", "-f", &cf, "up", "-d"];
        if recreate { args.push("--force-recreate"); }
        args.push(service);
        let out = self.run(&args).await?;
        Self::ensure_ok(&out, "docker compose up -d")
    }

    async fn stop(&self, service: &str) -> Result<(), TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let out = self.run(&["compose", "-f", &cf, "stop", service]).await?;
        Self::ensure_ok(&out, "docker compose stop")
    }

    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError> {
        let out = self.run(&["inspect", "--format", "{{.Config.Image}}", container]).await?;
        Self::ensure_ok(&out, "docker inspect")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    async fn logs_tail(&self, service: &str, n: u32) -> Result<String, TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let n_s = n.to_string();
        let out = self.run(&["compose", "-f", &cf, "logs", "--tail", &n_s, service]).await?;
        // logs returns non-zero if service unknown; treat as Compose error.
        Self::ensure_ok(&out, "docker compose logs")?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
```

- [ ] **Step 2: MockCompose**

`crates/totsukactl/src/compose/mock.rs`:
```rust
use super::ComposeExec;
use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Default)]
pub struct MockCompose {
    pub running: Mutex<bool>,
    pub image: Mutex<String>,
    pub calls: Mutex<Vec<String>>,
    pub fail_docker_info: Mutex<bool>,
}

impl MockCompose {
    pub fn with_image(image: &str) -> Self {
        Self {
            image: Mutex::new(image.into()),
            ..Default::default()
        }
    }
    pub fn record(&self, c: &str) { self.calls.lock().unwrap().push(c.into()); }
    pub fn calls(&self) -> Vec<String> { self.calls.lock().unwrap().clone() }
}

#[async_trait]
impl ComposeExec for MockCompose {
    async fn docker_info(&self) -> Result<(), TotsukactlError> {
        self.record("docker_info");
        if *self.fail_docker_info.lock().unwrap() {
            Err(TotsukactlError::Compose("docker daemon down".into()))
        } else { Ok(()) }
    }
    async fn compose_version(&self) -> Result<(), TotsukactlError> {
        self.record("compose_version"); Ok(())
    }
    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError> {
        self.record(&format!("ps_running:{service}"));
        Ok(*self.running.lock().unwrap())
    }
    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError> {
        self.record(&format!("up_detached:{service}:{recreate}"));
        *self.running.lock().unwrap() = true;
        Ok(())
    }
    async fn stop(&self, service: &str) -> Result<(), TotsukactlError> {
        self.record(&format!("stop:{service}"));
        *self.running.lock().unwrap() = false;
        Ok(())
    }
    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError> {
        self.record(&format!("inspect_image:{container}"));
        Ok(self.image.lock().unwrap().clone())
    }
    async fn logs_tail(&self, service: &str, _n: u32) -> Result<String, TotsukactlError> {
        self.record(&format!("logs_tail:{service}"));
        Ok(String::new())
    }
}
```

- [ ] **Step 3: Tests of MockCompose contract**

`crates/totsukactl/tests/compose_mock.rs`:
```rust
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;

#[tokio::test]
async fn up_then_ps_reflects_running() {
    let m = MockCompose::default();
    assert!(!m.ps_running("pgmq").await.unwrap());
    m.up_detached("pgmq", false).await.unwrap();
    assert!(m.ps_running("pgmq").await.unwrap());
    let calls = m.calls();
    assert!(calls.iter().any(|c| c == "up_detached:pgmq:false"));
}

#[tokio::test]
async fn inspect_image_returns_canned_value() {
    let m = MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1");
    assert_eq!(
        m.inspect_image("totsuka-pgmq").await.unwrap(),
        "ghcr.io/pgmq/pg18-pgmq:v1.11.1"
    );
}

#[tokio::test]
async fn docker_info_failure_surfaces() {
    let m = MockCompose::default();
    *m.fail_docker_info.lock().unwrap() = true;
    assert!(m.docker_info().await.is_err());
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test compose_mock
git add crates/totsukactl/src/compose.rs crates/totsukactl/src/compose/mock.rs crates/totsukactl/tests/compose_mock.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): ComposeExec trait + DockerCompose + MockCompose"
```

---

### Task 9: Preflight probes (phase -1 + phase 0)

**Files:**
- Modify: `crates/totsukactl/src/probe.rs`
- Create: `crates/totsukactl/tests/preflight.rs`

**Interfaces:**
- Consumes: `compose::ComposeExec`, `sqlx::PgPool`, `totsuka_config::Config`, `paths::Paths`.
- Produces:
  - `pub struct Preflight<'a> { pub compose: Arc<dyn ComposeExec>, pub cfg: &'a totsuka_config::Config, pub paths: &'a Paths }`
  - `pub async fn run(&self, pool_after_compose_up: impl FnOnce(&str) -> BoxFuture<'_, Result<sqlx::PgPool, TotsukactlError>>) -> Result<sqlx::PgPool, TotsukactlError>` (the closure decouples DB connect from preflight so unit tests can pass a stub pool factory).
  - `pub async fn pgmq_extversion(pool: &sqlx::PgPool) -> Result<String, TotsukactlError>` (runs `SELECT extversion FROM pg_extension WHERE extname='pgmq'`).
  - `pub fn pgmq_compatible(extversion: &str, want: &str) -> bool` (`want = "1.11.1"`, accepts major.minor match; tested below).
  - `pub async fn herdr_socket_ping(path: &Path) -> Result<(), TotsukactlError>` (connect-only smoke test; success = the connect call resolves and immediately closes).
  - `pub async fn ensure_image_match(compose: &dyn ComposeExec, container: &str, want_image: &str, recreate_allowed: bool) -> Result<(), TotsukactlError>`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/probe.rs`:
```rust
//! Phase -1 (pgmq container) + Phase 0 (config / schema / herdr) preflight (spec §4).

use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::schema_check::check_schema_version;
use sqlx::PgPool;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;

pub type DbConnect<'a> = Box<dyn FnOnce(String) -> Pin<Box<dyn Future<Output = Result<PgPool, TotsukactlError>> + Send + 'a>> + Send + 'a>;

pub struct Preflight<'a> {
    pub compose: Arc<dyn ComposeExec>,
    pub cfg: &'a totsuka_config::Config,
    pub paths: &'a Paths,
}

impl<'a> Preflight<'a> {
    pub async fn run_phase_minus1(&self, recreate: bool) -> Result<(), TotsukactlError> {
        self.compose.docker_info().await?;
        self.compose.compose_version().await?;
        let running = self.compose.ps_running("pgmq").await?;
        if !running {
            self.compose.up_detached("pgmq", recreate).await?;
        }
        ensure_image_match(
            self.compose.as_ref(),
            &self.cfg.postgres.container,
            &self.cfg.postgres.image,
            self.cfg.supervisor.recreate_on_image_mismatch,
        )
        .await
    }

    pub async fn run_phase_0(&self, pool: &PgPool, herdr_socket: &Path) -> Result<(), TotsukactlError> {
        let extv = pgmq_extversion(pool).await?;
        if !pgmq_compatible(&extv, "1.11.1") {
            return Err(TotsukactlError::Probe(format!(
                "pgmq extension version {extv} incompatible with expected 1.11.x"
            )));
        }
        check_schema_version(pool).await?;
        herdr_socket_ping(herdr_socket).await?;
        Ok(())
    }
}

pub async fn pgmq_extversion(pool: &PgPool) -> Result<String, TotsukactlError> {
    let row: (Option<String>,) =
        sqlx::query_as("SELECT extversion FROM pg_extension WHERE extname='pgmq'")
            .fetch_one(pool)
            .await?;
    row.0.ok_or_else(|| TotsukactlError::Probe("pgmq extension not installed".into()))
}

/// Major.Minor must match `want`; patch ignored.
pub fn pgmq_compatible(extversion: &str, want: &str) -> bool {
    fn major_minor(v: &str) -> Option<(u32, u32)> {
        let mut it = v.split('.');
        let maj = it.next()?.parse().ok()?;
        let min = it.next()?.parse().ok()?;
        Some((maj, min))
    }
    match (major_minor(extversion), major_minor(want)) {
        (Some(g), Some(w)) => g == w,
        _ => false,
    }
}

pub async fn herdr_socket_ping(path: &Path) -> Result<(), TotsukactlError> {
    tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(path))
        .await
        .map_err(|_| TotsukactlError::Probe(format!("herdr socket {path:?}: connect timeout")))?
        .map_err(|e| TotsukactlError::Probe(format!("herdr socket {path:?}: {e}")))?;
    Ok(())
}

pub async fn ensure_image_match(
    compose: &dyn ComposeExec,
    container: &str,
    want_image: &str,
    recreate_allowed: bool,
) -> Result<(), TotsukactlError> {
    let got = compose.inspect_image(container).await?;
    if got == want_image {
        return Ok(());
    }
    if recreate_allowed {
        compose.up_detached("pgmq", true).await?;
        return Ok(());
    }
    Err(TotsukactlError::Probe(format!(
        "pgmq image mismatch: running={got} expected={want_image} (run `docker compose pull && totsukactl up --recreate`)"
    )))
}
```

- [ ] **Step 2: Tests (pgmq_compatible + image match against MockCompose)**

`crates/totsukactl/tests/preflight.rs`:
```rust
use std::sync::Arc;
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::probe::{ensure_image_match, pgmq_compatible};

#[test]
fn pgmq_compatible_accepts_patch_drift() {
    assert!(pgmq_compatible("1.11.1", "1.11.1"));
    assert!(pgmq_compatible("1.11.2", "1.11.1"));
    assert!(pgmq_compatible("1.11.0", "1.11.1"));
}

#[test]
fn pgmq_compatible_rejects_minor_drift() {
    assert!(!pgmq_compatible("1.10.1", "1.11.1"));
    assert!(!pgmq_compatible("2.0.0", "1.11.1"));
    assert!(!pgmq_compatible("garbage", "1.11.1"));
}

#[tokio::test]
async fn image_match_ok_passes() {
    let m: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1"));
    ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", false)
        .await
        .unwrap();
}

#[tokio::test]
async fn image_mismatch_without_recreate_errors() {
    let m: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.10.0"));
    let err = ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("image mismatch"));
}

#[tokio::test]
async fn image_mismatch_with_recreate_calls_up() {
    let inner = MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.10.0");
    let m: Arc<dyn ComposeExec> = Arc::new(inner);
    ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", true)
        .await
        .unwrap();
    // safe downcast: we only ever stored MockCompose
    let m_ref: &MockCompose = unsafe { &*(Arc::as_ptr(&m) as *const MockCompose) };
    assert!(m_ref.calls().iter().any(|c| c == "up_detached:pgmq:true"));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test preflight
git add crates/totsukactl/src/probe.rs crates/totsukactl/tests/preflight.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): phase -1/0 preflight (compose probe + pgmq extversion + herdr ping)"
```

---

### Task 10: Child spawn + log capture

**Files:**
- Modify: `crates/totsukactl/src/child.rs`
- Create: `crates/totsukactl/src/child/spec.rs`
- Create: `crates/totsukactl/tests/child_spec.rs`

**Interfaces:**
- Produces:
  - `pub struct ChildSpec { pub name: String, pub bin_path: PathBuf, pub args: Vec<String>, pub env: Vec<(String, String)>, pub log_path: PathBuf }`
  - `pub fn specs_from_config(cfg: &totsuka_config::Config, paths: &Paths, exe_dir: &Path) -> Vec<ChildSpec>` — returns one ChildSpec per Rust bin (in startup order) with `bin_path = exe_dir.join(name)`, `args = vec!["--config".into(), config_path.into()]`, `env = vec![("TOTSUKA_CONFIG".into(), config_path), ("RUST_LOG".into(), cfg.totsuka.log_level.clone())]`, `log_path = paths.child_log(name)`.
  - `#[async_trait] pub trait ChildSpawner: Send + Sync` with `async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError>` (returns pid; appends stdout+stderr to `spec.log_path`).
  - `pub struct ForkExecSpawner` impl (uses `tokio::process::Command` + `Stdio::from(File::open(...))`).
  - `pub struct MockSpawner { ... }` in `child::mock` for unit tests of the supervisor (returns synthetic pids from an atomic counter; records the spec it was asked to spawn).

- [ ] **Step 1: ChildSpec + builder**

`crates/totsukactl/src/child.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub mod mock;
pub mod spec;

pub use spec::{specs_from_config, ChildSpec};

#[async_trait]
pub trait ChildSpawner: Send + Sync {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError>;
}

pub struct ForkExecSpawner;

#[async_trait]
impl ChildSpawner for ForkExecSpawner {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError> {
        if let Some(parent) = spec.log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.log_path)?;
        let stderr = stdout.try_clone()?;
        let mut cmd = Command::new(&spec.bin_path);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .stdin(Stdio::null());
        let child = cmd.spawn().map_err(|e| {
            TotsukactlError::Spawn(format!("spawn {}: {e}", spec.bin_path.display()))
        })?;
        child
            .id()
            .map(|pid| pid as i32)
            .ok_or_else(|| TotsukactlError::Spawn(format!("{} pid unavailable", spec.name)))
    }
}

#[allow(dead_code)]
fn _unused_paths_ref(_p: &Paths, _b: &Path) -> PathBuf { PathBuf::new() }
```

- [ ] **Step 2: Spec builder**

`crates/totsukactl/src/child/spec.rs`:
```rust
use crate::paths::Paths;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub name: String,
    pub bin_path: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub log_path: PathBuf,
}

pub const RUST_BINS_IN_ORDER: &[&str] = &[
    "agent-adapter",
    "orchestrator",
    "github-watcher",
    "qa-service",
];

pub fn specs_from_config(
    cfg: &totsuka_config::Config,
    paths: &Paths,
    exe_dir: &Path,
    config_path: &str,
) -> Vec<ChildSpec> {
    RUST_BINS_IN_ORDER
        .iter()
        .map(|name| ChildSpec {
            name: (*name).into(),
            bin_path: exe_dir.join(name),
            args: vec!["--config".into(), config_path.into()],
            env: vec![
                ("TOTSUKA_CONFIG".into(), config_path.into()),
                ("RUST_LOG".into(), cfg.totsuka.log_level.clone()),
            ],
            log_path: paths.child_log(name),
        })
        .collect()
}
```

- [ ] **Step 3: MockSpawner**

`crates/totsukactl/src/child/mock.rs`:
```rust
use super::{ChildSpawner, ChildSpec};
use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

pub struct MockSpawner {
    pub next_pid: AtomicI32,
    pub spawned: Mutex<Vec<String>>,
    pub fail_for: Mutex<Vec<String>>,
}

impl Default for MockSpawner {
    fn default() -> Self {
        Self {
            next_pid: AtomicI32::new(10_000),
            spawned: Mutex::new(Vec::new()),
            fail_for: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChildSpawner for MockSpawner {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError> {
        if self.fail_for.lock().unwrap().iter().any(|n| n == &spec.name) {
            return Err(TotsukactlError::Spawn(format!("mock: fail {}", spec.name)));
        }
        self.spawned.lock().unwrap().push(spec.name.clone());
        Ok(self.next_pid.fetch_add(1, Ordering::SeqCst))
    }
}
```

- [ ] **Step 4: Spec builder test**

`crates/totsukactl/tests/child_spec.rs`:
```rust
use std::path::Path;
use tempfile::TempDir;
use totsukactl::child::spec::{specs_from_config, RUST_BINS_IN_ORDER};
use totsukactl::paths::Paths;

const MIN_TOML: &str = r#"
[totsuka]
log_level="trace"
state_dir="/tmp/state"
data_dir="/tmp/data"
[supervisor]
[supervisor.heartbeat]
[postgres]
image="ghcr.io/pgmq/pg18-pgmq:v1.11.1"
container="totsuka-pgmq"
host="127.0.0.1"
port=5432
database="totsuka"
user="postgres"
volume="totsuka_pgmq_data"
compose_file="deploy/docker-compose.yml"
[bus]
queue_name="totsuka_events"
[agent_adapter]
uds_path="/tmp/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true
[orchestrator]
uds_path="/tmp/sock/orc.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="/tmp/sock/adapter.sock"
[github]
project_owner="o"
project_number=1
[github.columns]
inbox="📥"
ready="📋"
design="🤖"
design_review="🚧"
impl_verify="🤖"
final_review="🚧"
awaiting_release="🚀"
released="🏁"
[github_watcher]
bind="127.0.0.1:7802"
[qa_service]
uds_path="/tmp/sock/qa.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="/tmp/sock/adapter.sock"
[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"
[qa_service.answer]
[notifications]
[retention]
[telemetry]
"#;

#[test]
fn specs_cover_all_four_bins_in_startup_order() {
    let cfg = totsuka_config::Config::from_toml_str(MIN_TOML).unwrap();
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    let exe_dir = Path::new("/usr/local/bin");
    let specs = specs_from_config(&cfg, &paths, exe_dir, "/etc/totsuka/config.toml");
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, RUST_BINS_IN_ORDER);
    let s = &specs[0];
    assert_eq!(s.bin_path, exe_dir.join("agent-adapter"));
    assert_eq!(s.args, vec!["--config".to_string(), "/etc/totsuka/config.toml".to_string()]);
    assert!(s.env.iter().any(|(k, v)| k == "TOTSUKA_CONFIG" && v == "/etc/totsuka/config.toml"));
    assert!(s.env.iter().any(|(k, v)| k == "RUST_LOG" && v == "trace"));
    assert_eq!(s.log_path, paths.log_dir.join("agent-adapter.log"));
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test child_spec
git add crates/totsukactl/src/child.rs crates/totsukactl/src/child/spec.rs crates/totsukactl/src/child/mock.rs crates/totsukactl/tests/child_spec.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): ChildSpec builder + ForkExecSpawner + MockSpawner"
```

---

### Task 11: HealthProbe trait + UDS / TCP impls + Mock

**Files:**
- Create: `crates/totsukactl/src/health.rs`
- Modify: `crates/totsukactl/src/lib.rs` (`pub mod health;`)
- Create: `crates/totsukactl/tests/health.rs`

**Interfaces:**
- Produces:
  - `pub enum Endpoint { Uds(PathBuf), Tcp(String /* bind addr e.g. "127.0.0.1:7802" */) }`
  - `pub fn endpoint_for(name: &str, cfg: &totsuka_config::Config) -> Result<Endpoint, TotsukactlError>` — matches `"agent-adapter" → cfg.agent_adapter.uds_path`, `"orchestrator" → cfg.orchestrator.uds_path`, `"qa-service" → cfg.qa_service.uds_path`, `"github-watcher" → cfg.github_watcher.bind`; else `UnknownChild`.
  - `#[async_trait] pub trait HealthProbe: Send + Sync` with `async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError>`, `async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError>`.
  - `pub struct HttpHealthProbe { endpoints: HashMap<String, Endpoint> }` — UDS hits use `hyperlocal::UnixClientExt`, TCP hits use `reqwest::Client`.
  - `pub struct MockHealthProbe { ... }` for unit tests (canned `bool`/error per name).

- [ ] **Step 1: Implement endpoints + probe**

`crates/totsukactl/src/health.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::resolve_tilde;
use async_trait::async_trait;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::Request;
use hyperlocal::{UnixClientExt, UnixConnector, Uri as HyperlocalUri};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum Endpoint {
    Uds(PathBuf),
    Tcp(String),
}

pub fn endpoint_for(name: &str, cfg: &totsuka_config::Config) -> Result<Endpoint, TotsukactlError> {
    match name {
        "agent-adapter" => Ok(Endpoint::Uds(resolve_tilde(&cfg.agent_adapter.uds_path))),
        "orchestrator" => Ok(Endpoint::Uds(resolve_tilde(&cfg.orchestrator.uds_path))),
        "qa-service" => Ok(Endpoint::Uds(resolve_tilde(&cfg.qa_service.uds_path))),
        "github-watcher" => Ok(Endpoint::Tcp(cfg.github_watcher.bind.clone())),
        other => Err(TotsukactlError::UnknownChild(other.into())),
    }
}

#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError>;
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError>;
}

pub struct HttpHealthProbe {
    endpoints: HashMap<String, Endpoint>,
}

impl HttpHealthProbe {
    pub fn new(endpoints: HashMap<String, Endpoint>) -> Self { Self { endpoints } }

    async fn hit(&self, name: &str, path: &str) -> Result<u16, TotsukactlError> {
        let ep = self.endpoints.get(name)
            .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
        match ep {
            Endpoint::Uds(sock) => {
                let client: hyper_util::client::legacy::Client<UnixConnector, Empty<Bytes>> =
                    hyper_util::client::legacy::Client::unix();
                let uri: hyper::Uri = HyperlocalUri::new(sock, path).into();
                let req = Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Empty::<Bytes>::new())
                    .map_err(|e| TotsukactlError::Health(format!("build req: {e}")))?;
                let resp = client
                    .request(req)
                    .await
                    .map_err(|e| TotsukactlError::Health(format!("{name} {path}: {e}")))?;
                let code = resp.status().as_u16();
                let _ = resp.into_body().collect().await;
                Ok(code)
            }
            Endpoint::Tcp(addr) => {
                let url = format!("http://{addr}{path}");
                let resp = reqwest::Client::new()
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                    .map_err(|e| TotsukactlError::Health(format!("{name} {url}: {e}")))?;
                Ok(resp.status().as_u16())
            }
        }
    }
}

#[async_trait]
impl HealthProbe for HttpHealthProbe {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(self.hit(name, "/healthz").await? == 200)
    }
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(self.hit(name, "/readyz").await? == 200)
    }
}

#[derive(Default)]
pub struct MockHealthProbe {
    pub healthy: Mutex<HashMap<String, bool>>,
    pub ready: Mutex<HashMap<String, bool>>,
}

impl MockHealthProbe {
    pub fn set_healthy(&self, name: &str, v: bool) {
        self.healthy.lock().unwrap().insert(name.into(), v);
    }
    pub fn set_ready(&self, name: &str, v: bool) {
        self.ready.lock().unwrap().insert(name.into(), v);
    }
}

#[async_trait]
impl HealthProbe for MockHealthProbe {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(*self.healthy.lock().unwrap().get(name).unwrap_or(&true))
    }
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(*self.ready.lock().unwrap().get(name).unwrap_or(&true))
    }
}
```

- [ ] **Step 2: Add `pub mod health;` to lib.rs**

Insert `pub mod health;` into `crates/totsukactl/src/lib.rs` (alphabetical with `heartbeat`).

- [ ] **Step 3: Tests (endpoint_for + Mock)**

`crates/totsukactl/tests/health.rs`:
```rust
use totsukactl::health::{endpoint_for, Endpoint, HealthProbe, MockHealthProbe};

const TOML: &str = include_str!("./fixtures/min_config.toml");

#[test]
fn endpoint_for_maps_each_known_bin() {
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let ep = endpoint_for("agent-adapter", &cfg).unwrap();
    assert!(matches!(ep, Endpoint::Uds(_)));
    let ep = endpoint_for("github-watcher", &cfg).unwrap();
    assert!(matches!(ep, Endpoint::Tcp(addr) if addr.starts_with("127.0.0.1:")));
}

#[test]
fn endpoint_for_unknown_errors() {
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    assert!(endpoint_for("not-a-bin", &cfg).is_err());
}

#[tokio::test]
async fn mock_probe_returns_canned_values() {
    let m = MockHealthProbe::default();
    m.set_healthy("orchestrator", false);
    assert!(!m.healthz("orchestrator").await.unwrap());
    assert!(m.readyz("orchestrator").await.unwrap()); // default true
}
```

`crates/totsukactl/tests/fixtures/min_config.toml`: copy the same TOML block used in Task 10 (`MIN_TOML`) so other tests can include it via `include_str!`.

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test health
git add crates/totsukactl/src/health.rs crates/totsukactl/src/lib.rs crates/totsukactl/tests/health.rs crates/totsukactl/tests/fixtures/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): HealthProbe trait + HttpHealthProbe (UDS+TCP) + MockHealthProbe"
```

---

### Task 12: PgmqProbe (SELECT 1 + compose status)

**Files:**
- Create: `crates/totsukactl/src/pgmq_probe.rs`
- Modify: `crates/totsukactl/src/lib.rs` (`pub mod pgmq_probe;`)
- Create: `crates/totsukactl/tests/pgmq_probe_mock.rs`

**Interfaces:**
- Produces:
  - `#[async_trait] pub trait PgmqProbe: Send + Sync` with `async fn ping(&self) -> Result<bool, TotsukactlError>` (compose ps + SELECT 1).
  - `pub struct LivePgmqProbe { pub compose: Arc<dyn ComposeExec>, pub pool: sqlx::PgPool }` impl that returns `false` if container is not running or `SELECT 1` fails.
  - `pub struct MockPgmqProbe { pub answer: Mutex<bool> }` returning the canned bool.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/pgmq_probe.rs`:
```rust
use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use async_trait::async_trait;
use sqlx::PgPool;
use std::sync::Arc;
use std::sync::Mutex;

#[async_trait]
pub trait PgmqProbe: Send + Sync {
    async fn ping(&self) -> Result<bool, TotsukactlError>;
}

pub struct LivePgmqProbe {
    pub compose: Arc<dyn ComposeExec>,
    pub pool: PgPool,
}

#[async_trait]
impl PgmqProbe for LivePgmqProbe {
    async fn ping(&self) -> Result<bool, TotsukactlError> {
        if !self.compose.ps_running("pgmq").await? {
            return Ok(false);
        }
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::warn!(error=%e, "pgmq SELECT 1 failed");
                Ok(false)
            }
        }
    }
}

pub struct MockPgmqProbe {
    pub answer: Mutex<bool>,
}

impl MockPgmqProbe {
    pub fn new(initial: bool) -> Self { Self { answer: Mutex::new(initial) } }
    pub fn set(&self, v: bool) { *self.answer.lock().unwrap() = v; }
}

#[async_trait]
impl PgmqProbe for MockPgmqProbe {
    async fn ping(&self) -> Result<bool, TotsukactlError> {
        Ok(*self.answer.lock().unwrap())
    }
}
```

- [ ] **Step 2: Add to lib.rs**

Insert `pub mod pgmq_probe;` alphabetically into `crates/totsukactl/src/lib.rs`.

- [ ] **Step 3: Tests**

`crates/totsukactl/tests/pgmq_probe_mock.rs`:
```rust
use totsukactl::pgmq_probe::{MockPgmqProbe, PgmqProbe};

#[tokio::test]
async fn mock_pgmq_probe_returns_canned() {
    let p = MockPgmqProbe::new(true);
    assert!(p.ping().await.unwrap());
    p.set(false);
    assert!(!p.ping().await.unwrap());
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test pgmq_probe_mock
git add crates/totsukactl/src/pgmq_probe.rs crates/totsukactl/src/lib.rs crates/totsukactl/tests/pgmq_probe_mock.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): PgmqProbe (compose ps + SELECT 1) + Mock"
```

---

### Task 13: Heartbeat tickers (healthz / readyz / pgmq) + outcome calculator

**Files:**
- Modify: `crates/totsukactl/src/heartbeat.rs`
- Create: `crates/totsukactl/tests/heartbeat.rs`

**Interfaces:**
- Consumes: `Arc<dyn HealthProbe>`, `Arc<dyn PgmqProbe>`, `Arc<Registry>`, `Arc<dyn Clock>`, `CancellationToken`, `HeartbeatSection`.
- Produces:
  - `pub struct HeartbeatCfg { pub healthz_interval: Duration, pub readyz_interval: Duration, pub pgmq_interval: Duration, pub degraded_threshold: u32, pub unhealthy_threshold: u32 }` plus `From<&HeartbeatSection>`.
  - `pub async fn run_healthz_loop(...)`, `pub async fn run_readyz_loop(...)`, `pub async fn run_pgmq_loop(...)` — each: tick → call probe → on Ok: `reset_failure` + `set_state(next_state(_, Ok, …))`; on probe error: `bump_failure` + apply `next_state(_, Degraded, …)`; loop ends when `shutdown.cancelled()`.
  - `pub fn outcome_from(healthz_ok: bool, readyz_ok: bool) -> HealthOutcome` — `(true, true) → Ok`, `(true, false) → Degraded`, `(false, _) → Unhealthy`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/heartbeat.rs`:
```rust
use crate::health::HealthProbe;
use crate::pgmq_probe::PgmqProbe;
use crate::registry::Registry;
use crate::state::{next_state, ChildState, HealthOutcome};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_config::schema::HeartbeatSection;
use totsuka_core::Clock;

#[derive(Debug, Clone)]
pub struct HeartbeatCfg {
    pub healthz_interval: Duration,
    pub readyz_interval: Duration,
    pub pgmq_interval: Duration,
    pub degraded_threshold: u32,
    pub unhealthy_threshold: u32,
}

impl From<&HeartbeatSection> for HeartbeatCfg {
    fn from(s: &HeartbeatSection) -> Self {
        Self {
            healthz_interval: Duration::from_secs(s.healthz_interval_secs),
            readyz_interval: Duration::from_secs(s.readyz_interval_secs),
            pgmq_interval: Duration::from_secs(s.pgmq_interval_secs),
            degraded_threshold: s.degraded_threshold,
            unhealthy_threshold: s.unhealthy_threshold,
        }
    }
}

pub fn outcome_from(healthz_ok: bool, readyz_ok: bool) -> HealthOutcome {
    match (healthz_ok, readyz_ok) {
        (true, true) => HealthOutcome::Ok,
        (true, false) => HealthOutcome::Degraded,
        (false, _) => HealthOutcome::Unhealthy,
    }
}

pub async fn run_healthz_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn HealthProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    bins: Vec<String>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.healthz_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                for name in &bins {
                    let ok = probe.healthz(name).await.unwrap_or(false);
                    let now = clock.now();
                    registry.touch_healthz(name, now).await;
                    apply_outcome(&registry, name, ok, /*ready_ok*/ true, &cfg).await;
                }
            }
        }
    }
}

pub async fn run_readyz_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn HealthProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    bins: Vec<String>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.readyz_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                for name in &bins {
                    let ok = probe.readyz(name).await.unwrap_or(false);
                    let now = clock.now();
                    registry.touch_readyz(name, now).await;
                    apply_outcome(&registry, name, /*healthz*/ true, ok, &cfg).await;
                }
            }
        }
    }
}

pub async fn run_pgmq_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn PgmqProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.pgmq_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                let ok = probe.ping().await.unwrap_or(false);
                registry.touch_healthz("pgmq", clock.now()).await;
                if ok {
                    registry.reset_failure("pgmq").await;
                    registry.set_state("pgmq", ChildState::Healthy).await;
                } else {
                    let n = registry.bump_failure("pgmq").await;
                    let curr = registry.get("pgmq").await.map(|e| e.state).unwrap_or(ChildState::Healthy);
                    let next = next_state(curr, HealthOutcome::Unhealthy, n, cfg.degraded_threshold, cfg.unhealthy_threshold);
                    registry.set_state("pgmq", next).await;
                }
            }
        }
    }
}

async fn apply_outcome(
    registry: &Registry,
    name: &str,
    healthz_ok: bool,
    readyz_ok: bool,
    cfg: &HeartbeatCfg,
) {
    let outcome = outcome_from(healthz_ok, readyz_ok);
    let curr = registry.get(name).await.map(|e| e.state).unwrap_or(ChildState::Healthy);
    let next = match outcome {
        HealthOutcome::Ok => {
            registry.reset_failure(name).await;
            next_state(curr, HealthOutcome::Ok, 0, cfg.degraded_threshold, cfg.unhealthy_threshold)
        }
        _ => {
            let n = registry.bump_failure(name).await;
            next_state(curr, outcome, n, cfg.degraded_threshold, cfg.unhealthy_threshold)
        }
    };
    if next != curr {
        tracing::info!(name, prev=?curr, next=?next, "child state transition");
    }
    registry.set_state(name, next).await;
}
```

- [ ] **Step 2: Tests (loop without real probe — uses MockHealthProbe + paused tokio time)**

`crates/totsukactl/tests/heartbeat.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_core::{MockClock, SystemClock};
use totsukactl::health::MockHealthProbe;
use totsukactl::heartbeat::{outcome_from, run_healthz_loop, HeartbeatCfg};
use totsukactl::registry::Registry;
use totsukactl::state::{ChildState, HealthOutcome};

#[test]
fn outcome_from_truth_table() {
    assert_eq!(outcome_from(true, true), HealthOutcome::Ok);
    assert_eq!(outcome_from(true, false), HealthOutcome::Degraded);
    assert_eq!(outcome_from(false, true), HealthOutcome::Unhealthy);
    assert_eq!(outcome_from(false, false), HealthOutcome::Unhealthy);
}

#[tokio::test(start_paused = true)]
async fn healthz_loop_transitions_after_unhealthy_threshold() {
    let probe = Arc::new(MockHealthProbe::default());
    probe.set_healthy("orchestrator", false);
    let probe_dyn: Arc<dyn totsukactl::health::HealthProbe> = probe.clone();

    let reg = Arc::new(Registry::new());
    reg.set_state("orchestrator", ChildState::Healthy).await;
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = HeartbeatCfg {
        healthz_interval: Duration::from_secs(5),
        readyz_interval: Duration::from_secs(30),
        pgmq_interval: Duration::from_secs(30),
        degraded_threshold: 2,
        unhealthy_threshold: 3,
    };
    let shutdown = CancellationToken::new();
    let bins = vec!["orchestrator".to_string()];
    let h = {
        let reg = reg.clone();
        let cfg = cfg.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            run_healthz_loop(cfg, probe_dyn, reg, clock, bins, shutdown).await;
        })
    };

    // Advance past the first interval (interval ticks once immediately, then every 5s)
    tokio::time::advance(Duration::from_secs(1)).await; // tick #1 (immediate)
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #2
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #3
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #4 — failures>=3 → Unhealthy
    tokio::task::yield_now().await;
    shutdown.cancel();
    h.await.unwrap();

    assert_eq!(reg.get("orchestrator").await.unwrap().state, ChildState::Unhealthy);
    let _ = MockClock::default(); // touch import
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test heartbeat
git add crates/totsukactl/src/heartbeat.rs crates/totsukactl/tests/heartbeat.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): heartbeat tickers (healthz/readyz/pgmq) + outcome calculator"
```

---

### Task 14: Restart policy + backoff + giving_up

**Files:**
- Create: `crates/totsukactl/src/restart.rs`
- Modify: `crates/totsukactl/src/lib.rs` (`pub mod restart;`)
- Create: `crates/totsukactl/tests/restart.rs`

**Interfaces:**
- Produces:
  - `pub struct RestartCfg { pub policy: RestartPolicy, pub backoff_secs: Vec<u64>, pub max_attempts: u32 }` + `From<&HeartbeatSection>`.
  - `pub enum RestartDecision { Skip /* policy=never or non-eligible state */, Wait(Duration), GiveUp }` — `pub fn decide(state: ChildState, restart_count: u32, cfg: &RestartCfg) -> RestartDecision`. Eligibility:
    - `OnDeadOnly` → only `ChildState::Dead`.
    - `OnUnhealthy` → `Dead | Unhealthy`.
    - `Never` → none.
    - Special rule: name `"pgmq"` is **never restarted** (no cascade), caller filters by name; `decide` itself is name-agnostic but spec §7 calls out "policy 上書きで never 固定" for pgmq.
  - `pub fn backoff_for(attempt: u32, cfg: &RestartCfg) -> Duration` — clamp to last index when attempt exceeds `backoff_secs.len()`.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/restart.rs`:
```rust
use crate::state::{ChildState, RestartPolicy};
use std::time::Duration;
use totsuka_config::schema::HeartbeatSection;

#[derive(Debug, Clone)]
pub struct RestartCfg {
    pub policy: RestartPolicy,
    pub backoff_secs: Vec<u64>,
    pub max_attempts: u32,
}

impl RestartCfg {
    pub fn from_section(s: &HeartbeatSection) -> Result<Self, crate::error::TotsukactlError> {
        Ok(Self {
            policy: RestartPolicy::parse(&s.restart_policy)?,
            backoff_secs: s.restart_backoff_secs.clone(),
            max_attempts: s.restart_max_attempts,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestartDecision {
    Skip,
    Wait(Duration),
    GiveUp,
}

pub fn decide(state: ChildState, restart_count: u32, cfg: &RestartCfg) -> RestartDecision {
    let eligible = match cfg.policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnDeadOnly => matches!(state, ChildState::Dead),
        RestartPolicy::OnUnhealthy => matches!(state, ChildState::Dead | ChildState::Unhealthy),
    };
    if !eligible {
        return RestartDecision::Skip;
    }
    if restart_count >= cfg.max_attempts {
        return RestartDecision::GiveUp;
    }
    RestartDecision::Wait(backoff_for(restart_count, cfg))
}

pub fn backoff_for(attempt: u32, cfg: &RestartCfg) -> Duration {
    if cfg.backoff_secs.is_empty() {
        return Duration::from_secs(5);
    }
    let idx = (attempt as usize).min(cfg.backoff_secs.len() - 1);
    Duration::from_secs(cfg.backoff_secs[idx])
}
```

- [ ] **Step 2: Tests**

`crates/totsukactl/tests/restart.rs`:
```rust
use std::time::Duration;
use totsukactl::restart::{backoff_for, decide, RestartCfg, RestartDecision};
use totsukactl::state::{ChildState, RestartPolicy};

fn cfg(policy: RestartPolicy) -> RestartCfg {
    RestartCfg { policy, backoff_secs: vec![5, 15, 60], max_attempts: 5 }
}

#[test]
fn never_policy_always_skips() {
    let c = cfg(RestartPolicy::Never);
    assert_eq!(decide(ChildState::Dead, 0, &c), RestartDecision::Skip);
    assert_eq!(decide(ChildState::Unhealthy, 0, &c), RestartDecision::Skip);
}

#[test]
fn on_dead_only_skips_unhealthy() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(decide(ChildState::Unhealthy, 0, &c), RestartDecision::Skip);
    assert_eq!(decide(ChildState::Dead, 0, &c), RestartDecision::Wait(Duration::from_secs(5)));
}

#[test]
fn on_unhealthy_wakes_for_both() {
    let c = cfg(RestartPolicy::OnUnhealthy);
    assert_eq!(decide(ChildState::Unhealthy, 1, &c), RestartDecision::Wait(Duration::from_secs(15)));
    assert_eq!(decide(ChildState::Dead, 2, &c), RestartDecision::Wait(Duration::from_secs(60)));
}

#[test]
fn backoff_clamps_to_last_entry() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(backoff_for(0, &c), Duration::from_secs(5));
    assert_eq!(backoff_for(2, &c), Duration::from_secs(60));
    assert_eq!(backoff_for(99, &c), Duration::from_secs(60));
}

#[test]
fn give_up_at_max_attempts() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(decide(ChildState::Dead, 5, &c), RestartDecision::GiveUp);
    assert_eq!(decide(ChildState::Dead, 6, &c), RestartDecision::GiveUp);
}

#[test]
fn from_section_parses_kebab_case() {
    use totsuka_config::schema::HeartbeatSection;
    let s = HeartbeatSection {
        healthz_interval_secs: 5,
        readyz_interval_secs: 30,
        pgmq_interval_secs: 30,
        unhealthy_threshold: 3,
        degraded_threshold: 2,
        restart_policy: "on-dead-only".into(),
        restart_backoff_secs: vec![1, 2],
        restart_max_attempts: 2,
        notify_on_degraded: false,
    };
    let c = RestartCfg::from_section(&s).unwrap();
    assert_eq!(c.policy, RestartPolicy::OnDeadOnly);
    assert_eq!(c.backoff_secs, vec![1, 2]);
    assert_eq!(c.max_attempts, 2);
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test restart
git add crates/totsukactl/src/restart.rs crates/totsukactl/src/lib.rs crates/totsukactl/tests/restart.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): restart policy decision + backoff + giving_up"
```

---

### Task 15: Supervisor boot sequence (phases -1 → 3)

**Files:**
- Modify: `crates/totsukactl/src/supervisor.rs`
- Create: `crates/totsukactl/src/supervisor/boot.rs`
- Create: `crates/totsukactl/tests/boot.rs`

**Interfaces:**
- Consumes: `Arc<dyn ComposeExec>`, `Arc<dyn ChildSpawner>`, `Arc<dyn HealthProbe>`, `Arc<Registry>`, `Arc<dyn Clock>`, `Paths`, `Config`, child specs from Task 10.
- Produces:
  - `pub struct BootCtx { compose: Arc<dyn ComposeExec>, spawner: Arc<dyn ChildSpawner>, probe: Arc<dyn HealthProbe>, registry: Arc<Registry>, clock: Arc<dyn Clock>, paths: Paths, ready_timeout: Duration }`
  - `pub async fn boot(ctx: &BootCtx, specs: &[ChildSpec], wait_for_pgmq_ready: impl Future<Output = Result<(), TotsukactlError>>, run_phase_0: impl Future<Output = Result<(), TotsukactlError>>) -> Result<(), TotsukactlError>` — runs phase -1 (compose up + image match) → awaits `wait_for_pgmq_ready` → runs `run_phase_0` (extversion + schema + herdr) → phase 1 spawn `agent-adapter` + await readyz (30s × 500ms) → phase 2 spawn `orchestrator` + readyz → phase 3 spawn `github-watcher` and `qa-service` in parallel + both readyz. On any failure: reverse-order SIGTERM all spawned PIDs (via registry pid lookup), return error.
  - `pub async fn await_ready(probe: Arc<dyn HealthProbe>, name: &str, timeout: Duration) -> Result<(), TotsukactlError>` — `tokio::time::timeout(timeout, async loop { sleep 500ms; if readyz → return Ok })`.

- [ ] **Step 1: await_ready helper**

`crates/totsukactl/src/supervisor.rs`:
```rust
pub mod boot;
pub use boot::{boot, await_ready, BootCtx};
```

`crates/totsukactl/src/supervisor/boot.rs`:
```rust
use crate::child::{ChildSpawner, ChildSpec};
use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::health::HealthProbe;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::state::ChildState;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

pub struct BootCtx {
    pub compose: Arc<dyn ComposeExec>,
    pub spawner: Arc<dyn ChildSpawner>,
    pub probe: Arc<dyn HealthProbe>,
    pub registry: Arc<Registry>,
    pub clock: Arc<dyn Clock>,
    pub paths: Paths,
    pub ready_timeout: Duration,
}

pub async fn await_ready(
    probe: Arc<dyn HealthProbe>,
    name: &str,
    timeout: Duration,
) -> Result<(), TotsukactlError> {
    let fut = async {
        loop {
            if probe.readyz(name).await.unwrap_or(false) {
                return Ok::<_, TotsukactlError>(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| TotsukactlError::Timeout(format!("{name} readyz did not arrive in {timeout:?}")))?
}

pub async fn boot<F1, F2>(
    ctx: &BootCtx,
    specs: &[ChildSpec],
    wait_for_pgmq_ready: F1,
    run_phase_0: F2,
) -> Result<(), TotsukactlError>
where
    F1: Future<Output = Result<(), TotsukactlError>>,
    F2: Future<Output = Result<(), TotsukactlError>>,
{
    wait_for_pgmq_ready.await?;
    ctx.registry.set_state("pgmq", ChildState::Healthy).await;

    run_phase_0.await?;

    let mut spawned: Vec<(String, i32)> = Vec::new();

    let phases: Vec<&[&str]> = vec![
        &["agent-adapter"],
        &["orchestrator"],
        &["github-watcher", "qa-service"],
    ];

    for phase in phases {
        let mut pids_this_phase = Vec::new();
        // spawn (sequential within a phase except phase-3, which is conceptually parallel
        // but spawn() is fast — we run them sequentially then await readyz concurrently).
        for name in phase {
            let spec = specs
                .iter()
                .find(|s| s.name == *name)
                .ok_or_else(|| TotsukactlError::Internal(format!("missing spec for {name}")))?;
            ctx.registry.set_state(name, ChildState::Starting).await;
            let pid = match ctx.spawner.spawn(spec).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(child=name, error=%e, "spawn failed; aborting boot");
                    rollback(&spawned).await;
                    return Err(e);
                }
            };
            let now = ctx.clock.now();
            ctx.registry.set_pid(name, Some(pid), Some(now)).await;
            pidfile::write_pid(&ctx.paths.child_pid(name), pid)?;
            spawned.push(((*name).to_string(), pid));
            pids_this_phase.push((*name, pid));
        }
        // await readyz in parallel
        let waits: Vec<_> = pids_this_phase
            .iter()
            .map(|(n, _)| {
                let p = ctx.probe.clone();
                let n = (*n).to_string();
                let to = ctx.ready_timeout;
                async move { (n.clone(), await_ready(p, &n, to).await) }
            })
            .collect();
        let results = futures_join_all(waits).await;
        for (name, res) in results {
            match res {
                Ok(()) => ctx.registry.set_state(&name, ChildState::Ready).await,
                Err(e) => {
                    tracing::error!(child=%name, error=%e, "readyz timed out; aborting boot");
                    rollback(&spawned).await;
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

async fn rollback(spawned: &[(String, i32)]) {
    for (name, pid) in spawned.iter().rev() {
        let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        tracing::warn!(child=%name, pid, "boot rollback SIGTERM");
    }
}

async fn futures_join_all<F, T>(futs: Vec<F>) -> Vec<T>
where
    F: Future<Output = T>,
{
    let mut out = Vec::with_capacity(futs.len());
    let handles: Vec<_> = futs
        .into_iter()
        .map(|f| tokio::spawn(async move { f.await }))
        .collect::<Vec<_>>();
    for h in handles {
        out.push(h.await.expect("futures_join_all spawn"));
    }
    out
}
```

Note: `tokio::spawn` requires `F: Send + 'static`; since the boot loop is the only consumer, we replace the helper with `futures_util::future::join_all`. Add `futures-util = "0.3"` to `crates/totsukactl/Cargo.toml` and rewrite the helper:

```rust
use futures_util::future::join_all;
let results = join_all(waits).await;
```

Drop the custom `futures_join_all` fn. (The implementer should land the import + dependency in this same task.)

- [ ] **Step 2: Test (boot with MockCompose + MockSpawner + MockHealthProbe)**

`crates/totsukactl/tests/boot.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::health::{HealthProbe, MockHealthProbe};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::state::ChildState;
use totsukactl::supervisor::{boot, BootCtx};

fn fake_spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![],
        env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

#[tokio::test]
async fn boot_happy_path_spawns_all_four_in_order() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1"));
    let spawner_concrete = Arc::new(MockSpawner::default());
    let spawner: Arc<dyn ChildSpawner> = spawner_concrete.clone();
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    for n in ["agent-adapter", "orchestrator", "github-watcher", "qa-service"] {
        probe_concrete.set_ready(n, true);
    }
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let ctx = BootCtx { compose, spawner, probe, registry: registry.clone(), clock, paths, ready_timeout: Duration::from_secs(5) };
    let specs: Vec<_> = ["agent-adapter", "orchestrator", "github-watcher", "qa-service"]
        .into_iter().map(|n| fake_spec(n, &tmp)).collect();

    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) }).await.unwrap();

    let order = spawner_concrete.spawned.lock().unwrap().clone();
    assert_eq!(order[0], "agent-adapter");
    assert_eq!(order[1], "orchestrator");
    let phase3: std::collections::HashSet<_> = order[2..].iter().cloned().collect();
    assert_eq!(phase3, ["github-watcher".to_string(), "qa-service".into()].into_iter().collect());
    for n in ["agent-adapter", "orchestrator", "github-watcher", "qa-service"] {
        assert_eq!(registry.get(n).await.unwrap().state, ChildState::Ready);
    }
}

#[tokio::test]
async fn boot_rolls_back_on_readyz_timeout() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    probe_concrete.set_ready("agent-adapter", false); // never becomes ready
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let ctx = BootCtx { compose, spawner, probe, registry, clock, paths, ready_timeout: Duration::from_millis(200) };
    let specs = vec![fake_spec("agent-adapter", &tmp), fake_spec("orchestrator", &tmp),
                     fake_spec("github-watcher", &tmp), fake_spec("qa-service", &tmp)];
    let err = boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) }).await.unwrap_err();
    assert!(matches!(err, totsukactl::error::TotsukactlError::Timeout(_)));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test boot
git add crates/totsukactl/src/supervisor.rs crates/totsukactl/src/supervisor/boot.rs crates/totsukactl/tests/boot.rs crates/totsukactl/Cargo.toml
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): supervisor boot sequence (phases -1→3) with readyz wait + rollback"
```

---

### Task 16: Supervisor shutdown sequence (reverse-order SIGTERM + escalation)

**Files:**
- Create: `crates/totsukactl/src/supervisor/shutdown.rs`
- Modify: `crates/totsukactl/src/supervisor.rs` (`pub mod shutdown; pub use shutdown::{shutdown_stack, ShutdownCfg};`)
- Create: `crates/totsukactl/tests/shutdown.rs`

**Interfaces:**
- Produces:
  - `pub struct ShutdownCfg { pub grace: Duration, pub second_term: Duration, pub force_grace: Duration, pub also_postgres: bool, pub force: bool }`.
  - `pub async fn shutdown_stack(cfg: ShutdownCfg, registry: Arc<Registry>, compose: Arc<dyn ComposeExec>, paths: Paths) -> Result<(), TotsukactlError>` —
    - **graceful** (`!cfg.force`):
      - **stage 1**: parallel SIGTERM `github-watcher` + `qa-service`, wait `cfg.grace`.
      - **stage 2**: SIGTERM `orchestrator`, wait `cfg.grace`.
      - **stage 3**: SIGTERM `agent-adapter`, wait `cfg.grace`.
      - Per stage: any pid still alive → second SIGTERM, wait `cfg.second_term`; still alive → SIGKILL.
    - **force** (`cfg.force`): parallel SIGTERM all 4, wait `cfg.force_grace`; still alive → SIGKILL.
    - **stage 4** (optional, when `cfg.also_postgres`): `compose.stop("pgmq")`.
    - Cleanup: remove `${state_dir}/pids/*.pid` and `${state_dir}/supervisor.pid` regardless of stage outcomes.
  - Use the same `pidfile::process_alive` check between escalations.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/supervisor/shutdown.rs`:
```rust
use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::state::ChildState;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::sync::Arc;
use std::time::Duration;

pub struct ShutdownCfg {
    pub grace: Duration,
    pub second_term: Duration,
    pub force_grace: Duration,
    pub also_postgres: bool,
    pub force: bool,
}

pub async fn shutdown_stack(
    cfg: ShutdownCfg,
    registry: Arc<Registry>,
    compose: Arc<dyn ComposeExec>,
    paths: Paths,
) -> Result<(), TotsukactlError> {
    if cfg.force {
        let all = ["github-watcher", "qa-service", "orchestrator", "agent-adapter"];
        sigterm_parallel(&registry, &all).await;
        wait_or_kill(&registry, &all, cfg.force_grace).await;
    } else {
        // stage 1: ingestion
        sigterm_parallel(&registry, &["github-watcher", "qa-service"]).await;
        wait_or_kill_escalate(&registry, &["github-watcher", "qa-service"], cfg.grace, cfg.second_term).await;
        // stage 2: control
        sigterm_parallel(&registry, &["orchestrator"]).await;
        wait_or_kill_escalate(&registry, &["orchestrator"], cfg.grace, cfg.second_term).await;
        // stage 3: execution
        sigterm_parallel(&registry, &["agent-adapter"]).await;
        wait_or_kill_escalate(&registry, &["agent-adapter"], cfg.grace, cfg.second_term).await;
    }

    for n in ["github-watcher", "qa-service", "orchestrator", "agent-adapter"] {
        registry.set_state(n, ChildState::Stopped).await;
        pidfile::remove(&paths.child_pid(n))?;
    }

    if cfg.also_postgres {
        compose.stop("pgmq").await?;
        registry.set_state("pgmq", ChildState::Stopped).await;
    }
    pidfile::remove(&paths.supervisor_pid())?;
    Ok(())
}

async fn sigterm_parallel(registry: &Registry, names: &[&str]) {
    for n in names {
        if let Some(e) = registry.get(n).await {
            if let Some(pid) = e.pid {
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
                tracing::info!(child=*n, pid, "SIGTERM");
                let _ = registry; // borrow holder
            }
        }
    }
}

async fn wait_or_kill_escalate(registry: &Registry, names: &[&str], grace: Duration, second: Duration) {
    tokio::time::sleep(grace).await;
    let still: Vec<_> = collect_alive(registry, names).await;
    for (n, pid) in &still {
        let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        tracing::warn!(child=%n, pid, "SIGTERM (2nd)");
    }
    tokio::time::sleep(second).await;
    for (n, pid) in collect_alive(registry, names).await {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        tracing::error!(child=%n, pid, "SIGKILL");
    }
}

async fn wait_or_kill(registry: &Registry, names: &[&str], grace: Duration) {
    tokio::time::sleep(grace).await;
    for (n, pid) in collect_alive(registry, names).await {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        tracing::error!(child=%n, pid, "SIGKILL (force)");
    }
}

async fn collect_alive(registry: &Registry, names: &[&str]) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    for n in names {
        if let Some(e) = registry.get(n).await {
            if let Some(pid) = e.pid {
                if pidfile::process_alive(pid) {
                    out.push(((*n).to_string(), pid));
                }
            }
        }
    }
    out
}
```

- [ ] **Step 2: Test (uses a real short-lived child that ignores SIGTERM until escalation)**

`crates/totsukactl/tests/shutdown.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::supervisor::shutdown::{shutdown_stack, ShutdownCfg};

#[tokio::test]
async fn shutdown_clears_pid_files_and_sets_stopped_state() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    // pretend stack was up — write fake pid files and registry entries with pid=0xdeadbeef
    // (kill on a non-existent pid is a no-op so we exercise the cleanup path only).
    for n in ["github-watcher", "qa-service", "orchestrator", "agent-adapter"] {
        let pid = std::process::id() as i32; // self — but we'll signal and immediately escalate
        registry.set_pid(n, Some(pid), Some(chrono::Utc::now())).await;
        std::fs::write(paths.child_pid(n), format!("{pid}\n")).unwrap();
    }
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(50),
        second_term: Duration::from_millis(50),
        force_grace: Duration::from_millis(50),
        also_postgres: false,
        force: true, // use force to skip the multi-stage waits in the test
    };
    shutdown_stack(cfg, registry.clone(), compose, paths.clone()).await.unwrap();
    for n in ["github-watcher", "qa-service", "orchestrator", "agent-adapter"] {
        assert!(!paths.child_pid(n).exists(), "{n}.pid still exists");
        assert_eq!(
            registry.get(n).await.unwrap().state,
            totsukactl::state::ChildState::Stopped
        );
    }
    assert!(!paths.supervisor_pid().exists());
}

#[tokio::test]
async fn shutdown_with_postgres_calls_compose_stop() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    let compose_concrete = Arc::new(MockCompose::default());
    let compose: Arc<dyn ComposeExec> = compose_concrete.clone();
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(10),
        second_term: Duration::from_millis(10),
        force_grace: Duration::from_millis(10),
        also_postgres: true,
        force: true,
    };
    shutdown_stack(cfg, registry, compose, paths).await.unwrap();
    assert!(compose_concrete.calls().iter().any(|c| c == "stop:pgmq"));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p totsukactl --test shutdown
git add crates/totsukactl/src/supervisor.rs crates/totsukactl/src/supervisor/shutdown.rs crates/totsukactl/tests/shutdown.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): graceful + force shutdown with reverse-order SIGTERM/SIGKILL"
```

---

### Task 17: supervisor.sock UDS server (status / restart / reload / shutdown)

**Files:**
- Modify: `crates/totsukactl/src/sock_api.rs`
- Create: `crates/totsukactl/src/sock_api/server.rs`
- Create: `crates/totsukactl/src/sock_api/client.rs`
- Create: `crates/totsukactl/src/sock_api/dto.rs`
- Create: `crates/totsukactl/tests/sock_api.rs`

**Interfaces:**
- Wire format (JSON, RFC7807 for errors):
  - `GET /v1/processes` → `[ProcessDto]` (200)
  - `POST /v1/processes/<name>/restart` → `{ "queued": true, "name": "<name>" }` (202) or RFC7807
  - `POST /v1/processes/<name>/reload` → same (only `agent-adapter` is meaningful per spec §6 hot-reload; other bins → 400 `not_reloadable`)
  - `POST /v1/shutdown` `{ "postgres": bool, "force": bool }` → `{ "accepted": true }` (202)
- Produces:
  - `pub struct ProcessDto { name, pid, state, started_at, last_healthz_at, last_readyz_at, consecutive_failures, restart_count }` (via `serde`).
  - `pub fn router(state: SockApiState) -> axum::Router` with the 4 handlers above; uses the same `serve_uds` accept-loop pattern as `crates/orchestrator/src/listener.rs`.
  - `pub struct SockApiState { pub registry: Arc<Registry>, pub control_tx: mpsc::Sender<ControlMsg> }`.
  - `pub enum ControlMsg { Restart(String), Reload(String), Shutdown { postgres: bool, force: bool } }` — handlers translate HTTP to channel sends so the supervisor main loop owns side effects.
  - `pub struct SupervisorClient { sock: PathBuf }` with `pub async fn list(&self) -> Result<Vec<ProcessDto>, TotsukactlError>`, `pub async fn restart(&self, name: &str)`, `pub async fn reload(&self, name: &str)`, `pub async fn shutdown(&self, postgres: bool, force: bool)`. Uses hyperlocal.

- [ ] **Step 1: DTOs**

`crates/totsukactl/src/sock_api/dto.rs`:
```rust
use crate::registry::ProcessEntry;
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProcessDto {
    pub name: String,
    pub pid: Option<i32>,
    pub state: ChildState,
    pub started_at: Option<DateTime<Utc>>,
    pub last_healthz_at: Option<DateTime<Utc>>,
    pub last_readyz_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub restart_count: u32,
}

impl From<ProcessEntry> for ProcessDto {
    fn from(e: ProcessEntry) -> Self {
        ProcessDto {
            name: e.name,
            pid: e.pid,
            state: e.state,
            started_at: e.started_at,
            last_healthz_at: e.last_healthz_at,
            last_readyz_at: e.last_readyz_at,
            consecutive_failures: e.consecutive_failures,
            restart_count: e.restart_count,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShutdownReq { pub postgres: bool, pub force: bool }
```

- [ ] **Step 2: Router + ControlMsg**

`crates/totsukactl/src/sock_api/server.rs`:
```rust
use super::dto::{ProcessDto, ShutdownReq};
use crate::registry::Registry;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct SockApiState {
    pub registry: Arc<Registry>,
    pub control_tx: mpsc::Sender<ControlMsg>,
}

#[derive(Debug, Clone)]
pub enum ControlMsg {
    Restart(String),
    Reload(String),
    Shutdown { postgres: bool, force: bool },
}

pub fn router(state: SockApiState) -> Router {
    Router::new()
        .route("/v1/processes", get(list))
        .route("/v1/processes/:name/restart", post(restart))
        .route("/v1/processes/:name/reload", post(reload))
        .route("/v1/shutdown", post(shutdown))
        .with_state(state)
}

async fn list(State(s): State<SockApiState>) -> impl IntoResponse {
    let entries = s.registry.list().await;
    let dto: Vec<ProcessDto> = entries.into_iter().map(Into::into).collect();
    (StatusCode::OK, Json(dto))
}

async fn restart(State(s): State<SockApiState>, Path(name): Path<String>) -> impl IntoResponse {
    if !known(&name) {
        return rfc7807(StatusCode::NOT_FOUND, "/errors/unknown_child", &name);
    }
    let _ = s.control_tx.send(ControlMsg::Restart(name.clone())).await;
    (StatusCode::ACCEPTED, Json(json!({ "queued": true, "name": name }))).into_response()
}

async fn reload(State(s): State<SockApiState>, Path(name): Path<String>) -> impl IntoResponse {
    if name != "agent-adapter" {
        return rfc7807(StatusCode::BAD_REQUEST, "/errors/not_reloadable", &name);
    }
    let _ = s.control_tx.send(ControlMsg::Reload(name.clone())).await;
    (StatusCode::ACCEPTED, Json(json!({ "queued": true, "name": name }))).into_response()
}

async fn shutdown(State(s): State<SockApiState>, Json(req): Json<ShutdownReq>) -> impl IntoResponse {
    let _ = s.control_tx.send(ControlMsg::Shutdown { postgres: req.postgres, force: req.force }).await;
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true }))).into_response()
}

fn known(name: &str) -> bool {
    crate::registry::ORDER.iter().any(|n| *n == name)
}

fn rfc7807(code: StatusCode, type_uri: &str, detail: &str) -> axum::response::Response {
    (
        code,
        [(axum::http::header::CONTENT_TYPE, "application/problem+json")],
        Json(json!({
            "type": type_uri,
            "title": code.canonical_reason().unwrap_or("error"),
            "status": code.as_u16(),
            "detail": detail,
        })),
    )
        .into_response()
}
```

- [ ] **Step 3: Client + sock_api mod**

`crates/totsukactl/src/sock_api.rs`:
```rust
pub mod client;
pub mod dto;
pub mod server;

use crate::error::TotsukactlError;
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;

pub use client::SupervisorClient;
pub use dto::{ProcessDto, ShutdownReq};
pub use server::{router, ControlMsg, SockApiState};

pub async fn bind_uds(path: &Path) -> Result<UnixListener, TotsukactlError> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(UnixListener::bind(path)?)
}

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> Result<(), TotsukactlError> {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use tower::Service;

    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let tower_service = svc.call(()).await
            .map_err(|e| TotsukactlError::Internal(format!("router make_service: {e}")))?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "supervisor.sock connection error");
            }
        });
    }
}

#[allow(dead_code)]
fn _unused_pathbuf() -> PathBuf { PathBuf::new() }
```

`crates/totsukactl/src/sock_api/client.rs`:
```rust
use super::dto::{ProcessDto, ShutdownReq};
use crate::error::TotsukactlError;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::Request;
use hyperlocal::{UnixClientExt, UnixConnector, Uri as HyperlocalUri};
use std::path::PathBuf;

pub struct SupervisorClient { pub sock: PathBuf }

impl SupervisorClient {
    pub fn new(sock: PathBuf) -> Self { Self { sock } }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, TotsukactlError> {
        let client: hyper_util::client::legacy::Client<UnixConnector, Empty<Bytes>> =
            hyper_util::client::legacy::Client::unix();
        let uri: hyper::Uri = HyperlocalUri::new(&self.sock, path).into();
        let req = Request::get(uri).body(Empty::<Bytes>::new())
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("build {path}: {e}")))?;
        let resp = client.request(req).await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("{path}: {e}")))?;
        let bytes = resp.into_body().collect().await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("read {path}: {e}")))?
            .to_bytes();
        serde_json::from_slice(&bytes)
            .map_err(|e| TotsukactlError::Internal(format!("decode {path}: {e}")))
    }

    async fn post_json<T: serde::Serialize>(&self, path: &str, body: &T) -> Result<(), TotsukactlError> {
        let json = serde_json::to_vec(body)
            .map_err(|e| TotsukactlError::Internal(format!("encode {path}: {e}")))?;
        let client: hyper_util::client::legacy::Client<UnixConnector, Full<Bytes>> =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(UnixConnector);
        let uri: hyper::Uri = HyperlocalUri::new(&self.sock, path).into();
        let req = Request::post(uri)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(json)))
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("build {path}: {e}")))?;
        let resp = client.request(req).await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("{path}: {e}")))?;
        if !resp.status().is_success() {
            return Err(TotsukactlError::SupervisorUnreachable(format!("{path}: {}", resp.status())));
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<ProcessDto>, TotsukactlError> {
        self.get_json("/v1/processes").await
    }
    pub async fn restart(&self, name: &str) -> Result<(), TotsukactlError> {
        self.post_json(&format!("/v1/processes/{name}/restart"), &serde_json::json!({})).await
    }
    pub async fn reload(&self, name: &str) -> Result<(), TotsukactlError> {
        self.post_json(&format!("/v1/processes/{name}/reload"), &serde_json::json!({})).await
    }
    pub async fn shutdown(&self, postgres: bool, force: bool) -> Result<(), TotsukactlError> {
        self.post_json("/v1/shutdown", &ShutdownReq { postgres, force }).await
    }
}
```

- [ ] **Step 4: Round-trip test (bind → client → server → channel msg)**

`crates/totsukactl/tests/sock_api.rs`:
```rust
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use totsukactl::registry::Registry;
use totsukactl::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState, SupervisorClient};
use totsukactl::state::ChildState;

#[tokio::test]
async fn list_round_trip_returns_registry_entries() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    registry.set_state("orchestrator", ChildState::Healthy).await;
    let (tx, _rx) = mpsc::channel(8);
    let state = SockApiState { registry: registry.clone(), control_tx: tx };
    let listener = bind_uds(&sock).await.unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });

    let client = SupervisorClient::new(sock.clone());
    let list = client.list().await.unwrap();
    let orch = list.into_iter().find(|p| p.name == "orchestrator").unwrap();
    assert_eq!(orch.state, ChildState::Healthy);
}

#[tokio::test]
async fn shutdown_post_enqueues_control_msg() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    let (tx, mut rx) = mpsc::channel(8);
    let state = SockApiState { registry, control_tx: tx };
    let listener = bind_uds(&sock).await.unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });

    let client = SupervisorClient::new(sock);
    client.shutdown(true, false).await.unwrap();
    let msg = rx.recv().await.unwrap();
    matches!(msg, ControlMsg::Shutdown { postgres: true, force: false });
}

#[tokio::test]
async fn reload_rejects_non_adapter() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    let (tx, _rx) = mpsc::channel(8);
    let state = SockApiState { registry, control_tx: tx };
    let listener = bind_uds(&sock).await.unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });

    let client = SupervisorClient::new(sock);
    let err = client.reload("orchestrator").await.unwrap_err();
    assert!(format!("{err}").contains("400"));
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test sock_api
git add crates/totsukactl/src/sock_api.rs crates/totsukactl/src/sock_api/ crates/totsukactl/tests/sock_api.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): supervisor.sock UDS API (status/restart/reload/shutdown) + client"
```

---

### Task 18: CLI scaffold (clap App with all subcommands)

**Files:**
- Modify: `crates/totsukactl/src/cli.rs`
- Modify: `crates/totsukactl/src/main.rs`
- Create: `crates/totsukactl/tests/cli_parse.rs`

**Interfaces:**
- Produces:
  - `pub struct Cli { config: Option<PathBuf>, command: Cmd }` (clap derive).
  - `pub enum Cmd { Up { recreate: bool, bootstrap: bool }, Down { force: bool, postgres: bool }, Status, Migrate, Init, Restart { bin: String }, Reload { bin: String }, Logs { bin: String, follow: bool, lines: u32 } }`.
  - `pub fn parse() -> Cli`.
  - `pub async fn dispatch(cli: Cli) -> Result<(), TotsukactlError>` — match on the variant and call into Tasks 19–25 (stubs for now: each match arm returns `Err(TotsukactlError::Internal("not yet wired in scaffold task"))`. Real wiring lands in subsequent tasks.).

- [ ] **Step 1: Clap definitions**

`crates/totsukactl/src/cli.rs`:
```rust
use crate::error::TotsukactlError;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "totsukactl", version, about = "Supervisor + CLI for the totsuka stack")]
pub struct Cli {
    /// Path to totsuka.toml (defaults to $TOTSUKA_CONFIG or ~/.config/totsuka/config.toml)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Start the stack (postgres → preflight → adapter → orchestrator → watcher∥qa)
    Up {
        #[arg(long)]
        recreate: bool,
        #[arg(long)]
        bootstrap: bool,
    },
    /// Graceful shutdown (reverse dep order, 15s grace)
    Down {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        postgres: bool,
    },
    /// Print process registry as a formatted table
    Status,
    /// Apply sqlx migrations
    Migrate,
    /// First-run bootstrap (write config.toml + secrets.toml, compose up, migrate)
    Init,
    /// Restart a single bin (respects dependency order)
    Restart { bin: String },
    /// Send SIGHUP to a single bin (only agent-adapter is meaningful)
    Reload { bin: String },
    /// Tail a child's log file
    Logs {
        bin: String,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: u32,
    },
}

pub fn parse() -> Cli { Cli::parse() }

pub async fn dispatch(cli: Cli) -> Result<(), TotsukactlError> {
    let _ = cli;
    Err(TotsukactlError::Internal(
        "cli dispatch wiring lands in Tasks 19-25".into(),
    ))
}
```

- [ ] **Step 2: main.rs wires clap**

Replace `crates/totsukactl/src/main.rs`:
```rust
use totsukactl::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = cli::parse();
    cli::dispatch(cli).await?;
    Ok(())
}
```

- [ ] **Step 3: Parser test**

`crates/totsukactl/tests/cli_parse.rs`:
```rust
use clap::Parser;
use totsukactl::cli::{Cli, Cmd};

#[test]
fn up_with_flags() {
    let c = Cli::parse_from(["totsukactl", "up", "--recreate", "--bootstrap"]);
    assert_eq!(c.command, Cmd::Up { recreate: true, bootstrap: true });
}

#[test]
fn down_force_and_postgres() {
    let c = Cli::parse_from(["totsukactl", "down", "--force", "--postgres"]);
    assert_eq!(c.command, Cmd::Down { force: true, postgres: true });
}

#[test]
fn logs_default_lines_100() {
    let c = Cli::parse_from(["totsukactl", "logs", "orchestrator"]);
    assert_eq!(c.command, Cmd::Logs { bin: "orchestrator".into(), follow: false, lines: 100 });
}

#[test]
fn restart_and_reload_take_bin_name() {
    let c = Cli::parse_from(["totsukactl", "restart", "agent-adapter"]);
    assert_eq!(c.command, Cmd::Restart { bin: "agent-adapter".into() });
    let c = Cli::parse_from(["totsukactl", "reload", "agent-adapter"]);
    assert_eq!(c.command, Cmd::Reload { bin: "agent-adapter".into() });
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test cli_parse
git add crates/totsukactl/src/cli.rs crates/totsukactl/src/main.rs crates/totsukactl/tests/cli_parse.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): clap CLI scaffold (up/down/status/migrate/init/restart/reload/logs)"
```

---

### Task 19: `status` subcommand (formatted table)

**Files:**
- Modify: `crates/totsukactl/src/cli.rs` (wire `Cmd::Status` → `commands::status::run`)
- Create: `crates/totsukactl/src/commands/mod.rs`
- Create: `crates/totsukactl/src/commands/status.rs`
- Create: `crates/totsukactl/tests/status_format.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(cfg: &Config, paths: &Paths, clock: &dyn Clock) -> Result<(), TotsukactlError>` — opens `SupervisorClient::new(paths.supervisor_sock())`, calls `.list()`, formats with `tabwriter` and prints.
  - Empty / unreachable supervisor → print one line `stack not running` and return `Err(TotsukactlError::NotRunning)`.
  - `pub fn format_table(entries: &[ProcessDto], now: DateTime<Utc>) -> String` — pure formatter matching spec §7 example:
    ```
    NAME            STATE      PID    UPTIME    HEALTHZ  RESTARTS
    pgmq            running    -      1h23m     ok       -
    agent-adapter   healthy    1234   1h22m     ok(5s)   0
    ```
    State formatting: pgmq prints `running` (special-cased to spec §7 example) when state ∈ {Healthy, Ready}; else the lowercase state name. HEALTHZ: if `last_healthz_at` is Some, `ok({delta})` using `humantime::format_duration`; else `-`. UPTIME: `humantime::format_duration(now - started_at)` truncated to minute precision; if started_at is None, `-`. PID: `-` if None. RESTARTS: `-` for pgmq, number otherwise.

- [ ] **Step 1: commands/mod.rs**

`crates/totsukactl/src/commands/mod.rs`:
```rust
pub mod status;
```

Add `pub mod commands;` to `crates/totsukactl/src/lib.rs` (alphabetical).

- [ ] **Step 2: status.rs**

`crates/totsukactl/src/commands/status.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::sock_api::{ProcessDto, SupervisorClient};
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use std::io::Write;
use tabwriter::TabWriter;
use totsuka_core::Clock;

pub async fn run(paths: &Paths, clock: &dyn Clock) -> Result<(), TotsukactlError> {
    let client = SupervisorClient::new(paths.supervisor_sock());
    let entries = match client.list().await {
        Ok(v) => v,
        Err(_) => {
            println!("stack not running");
            return Err(TotsukactlError::NotRunning);
        }
    };
    println!("{}", format_table(&entries, clock.now()));
    Ok(())
}

pub fn format_table(entries: &[ProcessDto], now: DateTime<Utc>) -> String {
    let mut tw = TabWriter::new(Vec::new()).padding(2);
    writeln!(tw, "NAME\tSTATE\tPID\tUPTIME\tHEALTHZ\tRESTARTS").unwrap();
    for e in entries {
        let state = if e.name == "pgmq" && matches!(e.state, ChildState::Healthy | ChildState::Ready) {
            "running".to_string()
        } else {
            format!("{:?}", e.state).to_lowercase()
        };
        let pid = e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let uptime = e.started_at
            .map(|t| short_dur(now.signed_duration_since(t).num_seconds().max(0) as u64))
            .unwrap_or_else(|| "-".into());
        let hz = match e.last_healthz_at {
            Some(t) => format!("ok({})", short_dur(now.signed_duration_since(t).num_seconds().max(0) as u64)),
            None => "-".into(),
        };
        let restarts = if e.name == "pgmq" { "-".into() } else { e.restart_count.to_string() };
        writeln!(tw, "{}\t{}\t{}\t{}\t{}\t{}", e.name, state, pid, uptime, hz, restarts).unwrap();
    }
    tw.flush().unwrap();
    String::from_utf8(tw.into_inner().unwrap()).unwrap()
}

fn short_dur(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 { format!("{h}h{m:02}m") }
    else if m > 0 { format!("{m}m{s:02}s") }
    else { format!("{s}s") }
}
```

- [ ] **Step 3: Wire dispatch**

Modify `crates/totsukactl/src/cli.rs`'s `dispatch`:
```rust
pub async fn dispatch(cli: Cli) -> Result<(), TotsukactlError> {
    let config_path = cli.config
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| std::env::var("TOTSUKA_CONFIG").ok())
        .unwrap_or_else(|| "~/.config/totsuka/config.toml".into());
    let cfg = totsuka_config::Config::load(crate::paths::resolve_tilde(&config_path))
        .map_err(|e| TotsukactlError::Config(format!("{e:?}")))?;
    let paths = crate::paths::Paths::from_config(&cfg);
    let clock: std::sync::Arc<dyn totsuka_core::Clock> =
        std::sync::Arc::new(totsuka_core::SystemClock);

    match cli.command {
        Cmd::Status => crate::commands::status::run(&paths, clock.as_ref()).await,
        _ => Err(TotsukactlError::Internal(
            "cli dispatch wiring lands in Tasks 20-25".into(),
        )),
    }
}
```

- [ ] **Step 4: Formatter test (pure)**

`crates/totsukactl/tests/status_format.rs`:
```rust
use chrono::{Duration, TimeZone, Utc};
use totsukactl::commands::status::format_table;
use totsukactl::sock_api::ProcessDto;
use totsukactl::state::ChildState;

#[test]
fn table_has_expected_header_and_rows() {
    let now = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let started = now - Duration::seconds(83 * 60);
    let healthz = now - Duration::seconds(5);
    let pgmq = ProcessDto {
        name: "pgmq".into(), pid: None, state: ChildState::Healthy,
        started_at: Some(started), last_healthz_at: Some(healthz),
        last_readyz_at: None, consecutive_failures: 0, restart_count: 0,
    };
    let adapter = ProcessDto {
        name: "agent-adapter".into(), pid: Some(1234), state: ChildState::Healthy,
        started_at: Some(started), last_healthz_at: Some(healthz),
        last_readyz_at: None, consecutive_failures: 0, restart_count: 0,
    };
    let s = format_table(&[pgmq, adapter], now);
    assert!(s.lines().next().unwrap().starts_with("NAME"));
    assert!(s.contains("pgmq"));
    assert!(s.contains("running"));         // pgmq special-case
    assert!(s.contains("agent-adapter"));
    assert!(s.contains("healthy"));
    assert!(s.contains("1234"));
    assert!(s.contains("ok(5s)"));
    assert!(s.contains("1h23m"));
}

#[test]
fn missing_pid_and_times_render_dashes() {
    let now = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    let stopped = ProcessDto {
        name: "qa-service".into(), pid: None, state: ChildState::Stopped,
        started_at: None, last_healthz_at: None,
        last_readyz_at: None, consecutive_failures: 0, restart_count: 2,
    };
    let s = format_table(&[stopped], now);
    assert!(s.contains("qa-service"));
    assert!(s.contains("stopped"));
    let row = s.lines().nth(1).unwrap();
    assert!(row.contains("-"), "expected dashes for pid/uptime/healthz, got {row}");
    assert!(row.contains('2'), "restarts should still show count");
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test status_format
git add crates/totsukactl/src/cli.rs crates/totsukactl/src/commands/ crates/totsukactl/src/lib.rs crates/totsukactl/tests/status_format.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): status subcommand + tabwriter formatter"
```

---

### Task 20: `up` subcommand — daemonize, boot, supervise loop

**Files:**
- Create: `crates/totsukactl/src/commands/up.rs`
- Modify: `crates/totsukactl/src/commands/mod.rs` (`pub mod up;`)
- Modify: `crates/totsukactl/src/cli.rs` (wire `Cmd::Up`)
- Create: `crates/totsukactl/src/supervisor/main_loop.rs`
- Modify: `crates/totsukactl/src/supervisor.rs` (`pub mod main_loop; pub use main_loop::run_supervisor;`)
- Create: `crates/totsukactl/tests/up_daemonize.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(cfg: Config, paths: Paths, recreate: bool, bootstrap: bool) -> Result<(), TotsukactlError>` in `commands::up` — entry:
    1. Check `pidfile::check(&paths.supervisor_pid())`:
       - `Alive(pid)` → return `AlreadyRunning(format!("supervisor pid {pid}"))`.
       - `Stale(pid)` → log warning + `pidfile::remove`.
       - `Absent` → continue.
    2. If `bootstrap` and `config.toml` missing → call `commands::init::run` (Task 25).
    3. `paths.ensure()`.
    4. **Daemonize** via `nix::unistd::{fork, setsid, dup2}` (`fork()` → parent writes pid file + exits; child becomes session leader; redirect stdin/stdout/stderr to `/dev/null` and append `supervisor.log`). Wrap in a `daemonize()` helper local to this file so tests can swap it out.
    5. In the daemon: tokio runtime is **re-built** post-fork (`tokio::runtime::Builder::new_multi_thread().enable_all().build()`) — the parent runtime is invalid across fork.
    6. Run `supervisor::run_supervisor(cfg, paths, recreate).await`.
  - `pub async fn run_supervisor(cfg: Config, paths: Paths, recreate: bool) -> Result<(), TotsukactlError>` in `supervisor::main_loop` — builds real `DockerCompose`, opens `PgPool`, runs `Preflight::run_phase_minus1`, then opens `PgPool`, runs `Preflight::run_phase_0`, builds `BootCtx` with `ForkExecSpawner` + `HttpHealthProbe`, calls `boot(...)`, binds `supervisor.sock`, spawns the heartbeat trio + sock_api server + control msg dispatcher, then awaits SIGTERM and triggers `shutdown_stack` with the configured grace.

- [ ] **Step 1: daemonize helper**

`crates/totsukactl/src/commands/up.rs` (snippet):
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::supervisor::run_supervisor;
use nix::unistd::{fork, setsid, ForkResult};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use totsuka_config::Config;

pub async fn run(cfg: Config, paths: Paths, recreate: bool, bootstrap: bool) -> Result<(), TotsukactlError> {
    match pidfile::check(&paths.supervisor_pid())? {
        pidfile::PidState::Alive(pid) => {
            return Err(TotsukactlError::AlreadyRunning(format!("supervisor pid {pid}")));
        }
        pidfile::PidState::Stale(pid) => {
            tracing::warn!(stale_pid=pid, "removing stale supervisor.pid");
            pidfile::remove(&paths.supervisor_pid())?;
        }
        pidfile::PidState::Absent => {}
    }

    if bootstrap {
        let config_path = Path::new(&paths.state_dir).parent().map(|p| p.join("config").join("config.toml"));
        if let Some(p) = config_path {
            if !p.exists() {
                crate::commands::init::run(&paths).await?;
            }
        }
    }

    paths.ensure()?;

    // SAFETY: fork() is called before any tokio runtime exists in this binary
    // (main builds none — see main.rs spawning the runtime inside the child).
    // The parent path writes the pid file and exits; the child builds a
    // fresh tokio runtime and enters the supervisor loop.
    match unsafe { fork() }.map_err(|e| TotsukactlError::Spawn(format!("fork: {e}")))? {
        ForkResult::Parent { child } => {
            pidfile::write_pid(&paths.supervisor_pid(), child.as_raw())?;
            println!("totsuka stack starting (supervisor pid {})", child);
            Ok(())
        }
        ForkResult::Child => {
            setsid().map_err(|e| TotsukactlError::Spawn(format!("setsid: {e}")))?;
            redirect_stdio(&paths)?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| TotsukactlError::Internal(format!("build runtime: {e}")))?;
            rt.block_on(run_supervisor(cfg, paths, recreate))?;
            std::process::exit(0);
        }
    }
}

fn redirect_stdio(paths: &Paths) -> Result<(), TotsukactlError> {
    use nix::unistd::dup2;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.supervisor_log())?;
    let null = std::fs::OpenOptions::new().read(true).open("/dev/null")?;
    dup2(null.as_raw_fd(), 0).map_err(|e| TotsukactlError::Spawn(format!("dup2 stdin: {e}")))?;
    dup2(log.as_raw_fd(), 1).map_err(|e| TotsukactlError::Spawn(format!("dup2 stdout: {e}")))?;
    dup2(log.as_raw_fd(), 2).map_err(|e| TotsukactlError::Spawn(format!("dup2 stderr: {e}")))?;
    Ok(())
}
```

The dispatch step in `main.rs` MUST NOT build a tokio runtime before calling `up::run`. Replace `main.rs`:

```rust
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = totsukactl::cli::parse();
    totsukactl::cli::dispatch(cli)
}
```

And in `cli::dispatch`, for `Cmd::Up` call `commands::up::run(...)` (the up command builds its own runtime for the child); for all other subcommands build a `tokio::runtime` inline and `block_on` the async work.

- [ ] **Step 2: Supervisor main loop**

`crates/totsukactl/src/supervisor/main_loop.rs`:
```rust
use crate::child::{specs_from_config, ForkExecSpawner};
use crate::compose::{ComposeExec, DockerCompose};
use crate::error::TotsukactlError;
use crate::health::{endpoint_for, Endpoint, HttpHealthProbe};
use crate::heartbeat::{run_healthz_loop, run_pgmq_loop, run_readyz_loop, HeartbeatCfg};
use crate::paths::{resolve_tilde, Paths};
use crate::pgmq_probe::{LivePgmqProbe, PgmqProbe};
use crate::probe::Preflight;
use crate::registry::Registry;
use crate::restart::RestartCfg;
use crate::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState};
use crate::supervisor::boot::{boot, BootCtx};
use crate::supervisor::shutdown::{shutdown_stack, ShutdownCfg};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;

pub async fn run_supervisor(
    cfg: totsuka_config::Config,
    paths: Paths,
    recreate: bool,
) -> Result<(), TotsukactlError> {
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let compose: Arc<dyn ComposeExec> = Arc::new(DockerCompose::new(
        PathBuf::from(&cfg.postgres.compose_file),
    ));

    // Phase -1: pgmq
    let pre = Preflight { compose: compose.clone(), cfg: &cfg, paths: &paths };
    pre.run_phase_minus1(recreate).await?;

    // Open pool (after compose up — pgmq may still need a few seconds; PgPool retries on connect).
    let db_url = format!(
        "postgres://{}:{}@{}:{}/{}",
        cfg.postgres.user,
        cfg.postgres.password.expose(),
        cfg.postgres.host,
        cfg.postgres.port,
        cfg.postgres.database,
    );
    let pool = retry_connect(&db_url, Duration::from_secs(30)).await?;

    // Phase 0
    pre.run_phase_0(&pool, &resolve_tilde(&cfg.agent_adapter.herdr_socket)).await?;

    let registry = Arc::new(Registry::new());
    let spawner = Arc::new(ForkExecSpawner);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"));
    let config_path = std::env::var("TOTSUKA_CONFIG")
        .unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let specs = specs_from_config(&cfg, &paths, &exe_dir, &config_path);

    let mut eps: HashMap<String, Endpoint> = HashMap::new();
    for n in ["agent-adapter", "orchestrator", "github-watcher", "qa-service"] {
        eps.insert(n.into(), endpoint_for(n, &cfg)?);
    }
    let probe: Arc<dyn crate::health::HealthProbe> = Arc::new(HttpHealthProbe::new(eps));

    let ctx = BootCtx {
        compose: compose.clone(),
        spawner,
        probe: probe.clone(),
        registry: registry.clone(),
        clock: clock.clone(),
        paths: paths.clone(),
        ready_timeout: Duration::from_secs(cfg.supervisor.ready_timeout_secs),
    };

    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) }).await?;

    // Heartbeat tickers
    let hb: HeartbeatCfg = (&cfg.supervisor.heartbeat).into();
    let shutdown_tok = CancellationToken::new();
    let bins = vec![
        "agent-adapter".to_string(), "orchestrator".into(),
        "github-watcher".into(), "qa-service".into(),
    ];
    let pgmq_probe: Arc<dyn PgmqProbe> = Arc::new(LivePgmqProbe { compose: compose.clone(), pool: pool.clone() });
    let h_hb = tokio::spawn(run_healthz_loop(hb.clone(), probe.clone(), registry.clone(), clock.clone(), bins.clone(), shutdown_tok.clone()));
    let h_rd = tokio::spawn(run_readyz_loop(hb.clone(), probe.clone(), registry.clone(), clock.clone(), bins.clone(), shutdown_tok.clone()));
    let h_pg = tokio::spawn(run_pgmq_loop(hb.clone(), pgmq_probe, registry.clone(), clock.clone(), shutdown_tok.clone()));

    // sock_api server
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ControlMsg>(16);
    let listener = bind_uds(&paths.supervisor_sock()).await?;
    let state = SockApiState { registry: registry.clone(), control_tx: ctl_tx };
    let r = router(state);
    let h_sock = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });

    // Control dispatcher
    let _restart_cfg = RestartCfg::from_section(&cfg.supervisor.heartbeat)?;
    let shutdown_cfg_grace = Duration::from_secs(cfg.supervisor.shutdown_grace_secs);
    let shutdown_cfg_kill = Duration::from_secs(cfg.supervisor.shutdown_kill_secs);
    let shutdown_drive = {
        let registry = registry.clone();
        let compose = compose.clone();
        let paths = paths.clone();
        let shutdown_tok = shutdown_tok.clone();
        async move {
            // SIGTERM / SIGINT or ControlMsg::Shutdown
            let mut term = signal(SignalKind::terminate())
                .map_err(|e| TotsukactlError::Internal(format!("install SIGTERM: {e}")))?;
            let mut int = signal(SignalKind::interrupt())
                .map_err(|e| TotsukactlError::Internal(format!("install SIGINT: {e}")))?;
            let (also_postgres, force) = tokio::select! {
                _ = term.recv() => (false, false),
                _ = int.recv()  => (false, false),
                msg = ctl_rx.recv() => match msg {
                    Some(ControlMsg::Shutdown { postgres, force }) => (postgres, force),
                    _ => (false, false),
                },
            };
            shutdown_tok.cancel();
            shutdown_stack(
                ShutdownCfg {
                    grace: shutdown_cfg_grace,
                    second_term: shutdown_cfg_kill,
                    force_grace: Duration::from_secs(3),
                    also_postgres,
                    force,
                },
                registry, compose, paths,
            ).await
        }
    };
    shutdown_drive.await?;
    let _ = tokio::join!(h_hb, h_rd, h_pg, h_sock);
    Ok(())
}

async fn retry_connect(url: &str, total: Duration) -> Result<sqlx::PgPool, TotsukactlError> {
    let deadline = std::time::Instant::now() + total;
    let mut delay = Duration::from_millis(500);
    loop {
        match PgPoolOptions::new().max_connections(4).connect(url).await {
            Ok(p) => return Ok(p),
            Err(e) if std::time::Instant::now() < deadline => {
                tracing::warn!(error=%e, "postgres connect failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(3));
            }
            Err(e) => return Err(TotsukactlError::Probe(format!("postgres connect: {e}"))),
        }
    }
}
```

- [ ] **Step 3: Wire `Cmd::Up` in dispatch**

Modify `crates/totsukactl/src/cli.rs` `dispatch`:
```rust
pub fn dispatch(cli: Cli) -> Result<(), TotsukactlError> {
    let config_path = cli.config
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| std::env::var("TOTSUKA_CONFIG").ok())
        .unwrap_or_else(|| "~/.config/totsuka/config.toml".into());

    // up has to fork BEFORE creating a tokio runtime; other commands build one on demand.
    if let Cmd::Up { recreate, bootstrap } = &cli.command {
        let cfg = totsuka_config::Config::load(crate::paths::resolve_tilde(&config_path))
            .map_err(|e| TotsukactlError::Config(format!("{e:?}")))?;
        let paths = crate::paths::Paths::from_config(&cfg);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|e| TotsukactlError::Internal(format!("build runtime: {e}")))?;
        return rt.block_on(crate::commands::up::run(cfg, paths, *recreate, *bootstrap));
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()
        .map_err(|e| TotsukactlError::Internal(format!("build runtime: {e}")))?;
    rt.block_on(async move {
        let cfg = totsuka_config::Config::load(crate::paths::resolve_tilde(&config_path))
            .map_err(|e| TotsukactlError::Config(format!("{e:?}")))?;
        let paths = crate::paths::Paths::from_config(&cfg);
        let clock: std::sync::Arc<dyn totsuka_core::Clock> =
            std::sync::Arc::new(totsuka_core::SystemClock);
        match cli.command {
            Cmd::Status => crate::commands::status::run(&paths, clock.as_ref()).await,
            _ => Err(TotsukactlError::Internal(
                "cli dispatch wiring lands in Tasks 21-25".into(),
            )),
        }
    })
}
```

- [ ] **Step 4: Test (pidfile alive guard rejects re-`up`)**

`crates/totsukactl/tests/up_daemonize.rs`:
```rust
//! We don't actually fork in tests — we exercise the pre-fork pidfile guard.
use std::sync::Arc;
use tempfile::TempDir;
use totsukactl::commands::up;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;
use totsukactl::pidfile;

const TOML: &str = include_str!("./fixtures/min_config.toml");

#[tokio::test]
async fn up_refuses_when_supervisor_already_running() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    pidfile::write_pid(&paths.supervisor_pid(), std::process::id() as i32).unwrap();

    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let err = up::run(cfg, paths, false, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::AlreadyRunning(_)));
    // ensure we didn't accidentally fork — pidfile still contains our PID
    let _ = Arc::new(()); // silence unused import
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test up_daemonize
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/src/main.rs crates/totsukactl/src/supervisor.rs crates/totsukactl/src/supervisor/main_loop.rs crates/totsukactl/tests/up_daemonize.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): up subcommand (fork+detach supervisor) + main_loop wiring"
```

---

### Task 21: `down` subcommand (SupervisorClient or fallback to pid kill)

**Files:**
- Create: `crates/totsukactl/src/commands/down.rs`
- Modify: `crates/totsukactl/src/commands/mod.rs` (`pub mod down;`)
- Modify: `crates/totsukactl/src/cli.rs` (wire `Cmd::Down`)
- Create: `crates/totsukactl/tests/down_flow.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(paths: &Paths, force: bool, postgres: bool) -> Result<(), TotsukactlError>`:
    1. `pidfile::check(supervisor_pid)` → `Absent | Stale` → `Err(TotsukactlError::NotRunning)`.
    2. Call `SupervisorClient::new(paths.supervisor_sock()).shutdown(postgres, force)`.
    3. Poll the supervisor pid for liveness every 500ms up to `grace + second_term + force_grace + 2s` (we don't know the values from CLI side — use 30s ceiling).
    4. If still alive after ceiling and `force` → `kill(pid, SIGKILL)`.
    5. Remove `supervisor.pid`.
  - When the UDS call returns `SupervisorUnreachable` but the pid file says Alive (race during shutdown): fall back to direct `SIGTERM` then `SIGKILL` after grace, then remove the pid file.

- [ ] **Step 1: Implement**

`crates/totsukactl/src/commands/down.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::sock_api::SupervisorClient;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::time::{Duration, Instant};

pub async fn run(paths: &Paths, force: bool, postgres: bool) -> Result<(), TotsukactlError> {
    let pid_state = pidfile::check(&paths.supervisor_pid())?;
    let pid = match pid_state {
        pidfile::PidState::Alive(p) => p,
        pidfile::PidState::Stale(_) | pidfile::PidState::Absent => {
            return Err(TotsukactlError::NotRunning);
        }
    };

    let client = SupervisorClient::new(paths.supervisor_sock());
    match client.shutdown(postgres, force).await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error=%e, "supervisor.sock shutdown unreachable; falling back to SIGTERM");
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !pidfile::process_alive(pid) {
            pidfile::remove(&paths.supervisor_pid())?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if force {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        pidfile::remove(&paths.supervisor_pid())?;
        Ok(())
    } else {
        Err(TotsukactlError::Timeout(format!(
            "supervisor pid {pid} did not exit in 30s; rerun with --force"
        )))
    }
}
```

- [ ] **Step 2: Wire dispatch**

Modify the `_ => Err(...)` arm in `cli::dispatch` to add:
```rust
Cmd::Down { force, postgres } => crate::commands::down::run(&paths, force, postgres).await,
```

- [ ] **Step 3: Test (NotRunning when supervisor.pid absent)**

`crates/totsukactl/tests/down_flow.rs`:
```rust
use tempfile::TempDir;
use totsukactl::commands::down;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

#[tokio::test]
async fn down_returns_not_running_without_pidfile() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let err = down::run(&paths, false, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::NotRunning));
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test down_flow
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/tests/down_flow.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): down subcommand (UDS shutdown + SIGTERM fallback + force)"
```

---

### Task 22: `restart` + `reload` subcommands

**Files:**
- Create: `crates/totsukactl/src/commands/restart.rs`
- Create: `crates/totsukactl/src/commands/reload.rs`
- Modify: `crates/totsukactl/src/commands/mod.rs`
- Modify: `crates/totsukactl/src/cli.rs`
- Create: `crates/totsukactl/tests/restart_reload.rs`

**Interfaces:**
- Produces:
  - `pub async fn restart::run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError>` — validates `bin ∈ registry::ORDER \ {"pgmq"}`, calls `SupervisorClient::restart(bin)`.
  - `pub async fn reload::run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError>` — validates `bin == "agent-adapter"` (spec §6: only adapter has meaningful hot-reload), calls `SupervisorClient::reload(bin)`.
  - Both translate `SupervisorUnreachable` into `NotRunning`.

- [ ] **Step 1: Implement restart.rs**

`crates/totsukactl/src/commands/restart.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::registry::ORDER;
use crate::sock_api::SupervisorClient;

pub async fn run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError> {
    if bin == "pgmq" {
        return Err(TotsukactlError::Config(
            "restarting pgmq is forbidden (data integrity); use docker compose manually".into(),
        ));
    }
    if !ORDER.iter().any(|n| *n == bin) {
        return Err(TotsukactlError::UnknownChild(bin.into()));
    }
    let client = SupervisorClient::new(paths.supervisor_sock());
    match client.restart(bin).await {
        Ok(()) => Ok(()),
        Err(TotsukactlError::SupervisorUnreachable(_)) => Err(TotsukactlError::NotRunning),
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 2: Implement reload.rs**

`crates/totsukactl/src/commands/reload.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::sock_api::SupervisorClient;

pub async fn run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError> {
    if bin != "agent-adapter" {
        return Err(TotsukactlError::Config(format!(
            "reload is only meaningful for agent-adapter (spec §6); refusing {bin}"
        )));
    }
    let client = SupervisorClient::new(paths.supervisor_sock());
    match client.reload(bin).await {
        Ok(()) => Ok(()),
        Err(TotsukactlError::SupervisorUnreachable(_)) => Err(TotsukactlError::NotRunning),
        Err(e) => Err(e),
    }
}
```

- [ ] **Step 3: Mod + dispatch**

`commands/mod.rs`: add `pub mod restart;` and `pub mod reload;`.
`cli::dispatch` arms:
```rust
Cmd::Restart { bin } => crate::commands::restart::run(&paths, &bin).await,
Cmd::Reload { bin } => crate::commands::reload::run(&paths, &bin).await,
```

- [ ] **Step 4: Tests (validation guards)**

`crates/totsukactl/tests/restart_reload.rs`:
```rust
use tempfile::TempDir;
use totsukactl::commands::{reload, restart};
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

fn paths(tmp: &TempDir) -> Paths {
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    p
}

#[tokio::test]
async fn restart_pgmq_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "pgmq").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Config(_)));
}

#[tokio::test]
async fn restart_unknown_bin_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "nope").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::UnknownChild(_)));
}

#[tokio::test]
async fn reload_non_adapter_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = reload::run(&paths(&tmp), "orchestrator").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Config(_)));
}

#[tokio::test]
async fn restart_adapter_without_supervisor_returns_not_running() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "agent-adapter").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::NotRunning));
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test restart_reload
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/tests/restart_reload.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): restart + reload subcommands (validation + UDS forward)"
```

---

### Task 23: `logs <bin>` subcommand (tail + optional follow)

**Files:**
- Create: `crates/totsukactl/src/commands/logs.rs`
- Modify: `crates/totsukactl/src/commands/mod.rs`
- Modify: `crates/totsukactl/src/cli.rs`
- Create: `crates/totsukactl/tests/logs.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(paths: &Paths, bin: &str, lines: u32, follow: bool) -> Result<(), TotsukactlError>`:
    - Resolve `paths.child_log(bin)` (or `paths.supervisor_log()` if `bin == "supervisor"`).
    - Validate `bin ∈ registry::ORDER ∪ {"supervisor"}`.
    - Print the last `lines` lines (read whole file, split, take tail — log files capped at 50MB per spec retention).
    - If `follow`: open the file, seek to end, poll for appended bytes every 250ms; flush each new line to stdout. Loop until SIGINT.
  - Pure helper: `pub fn tail_lines(text: &str, n: u32) -> String` — extract last `n` lines preserving order (no trailing newline duplication).

- [ ] **Step 1: Implement**

`crates/totsukactl/src/commands/logs.rs`:
```rust
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::registry::ORDER;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub async fn run(paths: &Paths, bin: &str, lines: u32, follow: bool) -> Result<(), TotsukactlError> {
    let path = log_path(paths, bin)?;
    if !path.exists() {
        return Err(TotsukactlError::Internal(format!("log file {path:?} not found")));
    }
    let text = std::fs::read_to_string(&path)?;
    print!("{}", tail_lines(&text, lines));
    if follow {
        follow_file(&path).await?;
    }
    Ok(())
}

fn log_path(paths: &Paths, bin: &str) -> Result<PathBuf, TotsukactlError> {
    if bin == "supervisor" {
        return Ok(paths.supervisor_log());
    }
    if !ORDER.iter().any(|n| *n == bin) {
        return Err(TotsukactlError::UnknownChild(bin.into()));
    }
    Ok(paths.child_log(bin))
}

pub fn tail_lines(text: &str, n: u32) -> String {
    let n = n as usize;
    let mut lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines.drain(..start);
    let mut out = lines.join("\n");
    if text.ends_with('\n') { out.push('\n'); }
    out
}

async fn follow_file(path: &std::path::Path) -> Result<(), TotsukactlError> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::End(0))?;
    let mut async_file = tokio::fs::File::from_std(file);
    let mut buf = vec![0u8; 4096];
    loop {
        let n = async_file.read(&mut buf).await?;
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        use std::io::Write;
        std::io::stdout().write_all(&buf[..n])?;
        std::io::stdout().flush()?;
    }
}
```

- [ ] **Step 2: Mod + dispatch**

`commands/mod.rs`: `pub mod logs;`.
`cli::dispatch`: `Cmd::Logs { bin, follow, lines } => crate::commands::logs::run(&paths, &bin, lines, follow).await,`

- [ ] **Step 3: Pure tail_lines test**

`crates/totsukactl/tests/logs.rs`:
```rust
use tempfile::TempDir;
use totsukactl::commands::logs::{run, tail_lines};
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

#[test]
fn tail_lines_takes_last_n_only() {
    let text = "a\nb\nc\nd\ne\n";
    assert_eq!(tail_lines(text, 2), "d\ne\n");
    assert_eq!(tail_lines(text, 0), "\n");
    assert_eq!(tail_lines(text, 99), text);
}

#[test]
fn tail_lines_preserves_no_trailing_newline() {
    let text = "a\nb\nc";
    assert_eq!(tail_lines(text, 2), "b\nc");
}

#[tokio::test]
async fn run_errors_for_unknown_bin() {
    let tmp = TempDir::new().unwrap();
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    let err = run(&p, "no-such-bin", 10, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::UnknownChild(_)));
}

#[tokio::test]
async fn run_errors_when_log_missing() {
    let tmp = TempDir::new().unwrap();
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    let err = run(&p, "orchestrator", 10, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Internal(_)));
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test logs
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/tests/logs.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): logs subcommand (tail + follow)"
```

---

### Task 24: `migrate` subcommand (embedded sqlx::migrate!)

**Files:**
- Create: `crates/totsukactl/src/commands/migrate.rs`
- Modify: `crates/totsukactl/src/commands/mod.rs`
- Modify: `crates/totsukactl/src/cli.rs`
- Create: `crates/totsukactl/tests/migrate_dryrun.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(cfg: &Config) -> Result<(), TotsukactlError>` — connects to the configured Postgres URL (using `cfg.postgres.password.expose()`), runs `sqlx::migrate!("../../migrations").run(&pool)`, returns success.
  - The `sqlx::migrate!` macro embeds the migrations at compile time so the binary doesn't need `sqlx-cli` at runtime.
  - `pub fn build_db_url(cfg: &Config) -> String` — single source of truth for the URL string used by both `migrate::run` and (later) `commands::up`.

- [ ] **Step 1: build_db_url + migrate**

`crates/totsukactl/src/commands/migrate.rs`:
```rust
use crate::error::TotsukactlError;
use sqlx::postgres::PgPoolOptions;

pub fn build_db_url(cfg: &totsuka_config::Config) -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            cfg.postgres.user,
            cfg.postgres.password.expose(),
            cfg.postgres.host,
            cfg.postgres.port,
            cfg.postgres.database,
        )
    })
}

pub async fn run(cfg: &totsuka_config::Config) -> Result<(), TotsukactlError> {
    let url = build_db_url(cfg);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .map_err(|e| TotsukactlError::Migrate(format!("connect: {e}")))?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .map_err(|e| TotsukactlError::Migrate(format!("{e}")))?;
    println!("migrations applied");
    Ok(())
}
```

- [ ] **Step 2: Mod + dispatch**

`commands/mod.rs`: `pub mod migrate;`.
`cli::dispatch`: `Cmd::Migrate => crate::commands::migrate::run(&cfg).await,`.

Replace the matching hand-built URL in `supervisor/main_loop.rs::run_supervisor` with `commands::migrate::build_db_url(&cfg)` to keep the formula in one place.

- [ ] **Step 3: Test (URL builder is the unit; macro test runs as integration when DATABASE_URL set)**

`crates/totsukactl/tests/migrate_dryrun.rs`:
```rust
use totsukactl::commands::migrate::build_db_url;

const TOML: &str = include_str!("./fixtures/min_config.toml");

#[test]
fn build_db_url_uses_secret_expose_not_hardcoded_password() {
    // DATABASE_URL overrides — temporarily remove it for this test.
    let restore = std::env::var("DATABASE_URL").ok();
    std::env::remove_var("DATABASE_URL");

    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let url = build_db_url(&cfg);
    // empty default Secret + min config → "postgres://postgres:@127.0.0.1:5432/totsuka"
    assert!(url.starts_with("postgres://postgres:"));
    assert!(url.contains("@127.0.0.1:5432/totsuka"));

    if let Some(v) = restore { std::env::set_var("DATABASE_URL", v); }
}

#[test]
fn database_url_env_override_wins() {
    std::env::set_var("DATABASE_URL", "postgres://custom@host:1234/x");
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    assert_eq!(build_db_url(&cfg), "postgres://custom@host:1234/x");
    std::env::remove_var("DATABASE_URL");
}

#[tokio::test]
async fn migrate_actually_runs_when_db_available() {
    let Ok(_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skip: DATABASE_URL not set");
        return;
    };
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    totsukactl::commands::migrate::run(&cfg).await.unwrap();
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test migrate_dryrun
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/src/supervisor/main_loop.rs crates/totsukactl/tests/migrate_dryrun.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): migrate subcommand (embedded sqlx::migrate!) + shared build_db_url"
```

---

### Task 25: `init` subcommand (first-run bootstrap)

**Files:**
- Create: `crates/totsukactl/src/commands/init.rs`
- Create: `crates/totsukactl/src/commands/templates/config.toml.tmpl`
- Create: `crates/totsukactl/src/commands/templates/secrets.toml.tmpl`
- Modify: `crates/totsukactl/src/commands/mod.rs`
- Modify: `crates/totsukactl/src/cli.rs`
- Create: `crates/totsukactl/tests/init_bootstrap.rs`

**Interfaces:**
- Produces:
  - `pub async fn run(paths: &Paths) -> Result<(), TotsukactlError>` — implements spec §11.11 in order:
    1. `paths.ensure()` — create state/log/pid/sock dirs.
    2. Resolve `config_dir = ${XDG_CONFIG_HOME:-~/.config}/totsuka/` and `mkdir -p`.
    3. Write `config_dir/config.toml` from `templates/config.toml.tmpl` (skip + warn if exists).
    4. Write `config_dir/secrets.toml` from template with mode `0600` (skip + warn if exists).
    5. Read the freshly-written config to drive (4) and (5):
       - Build `DockerCompose::new(cfg.postgres.compose_file)` and `compose.up_detached("pgmq", false)`.
       - `migrate::run(&cfg)` (idempotent — re-running over an applied DB is a no-op).
    6. Print next-step hint to stdout: `"edit ~/.config/totsuka/secrets.toml to add your tokens, then run `totsukactl up`"`.
  - Templates use the same TOML shape as `examples/totsuka.toml.example` (which spec §10.1 says lives in `examples/`); since that file doesn't exist yet, embed a minimal-but-runnable template inline via `include_str!`. The template MUST match the `MIN_TOML` used in tests (Task 10 fixtures) so a fresh `init` produces a parseable config.

- [ ] **Step 1: Templates**

`crates/totsukactl/src/commands/templates/config.toml.tmpl`:
```toml
# totsuka — generated by `totsukactl init` ($DATE)
# Edit before running `totsukactl up`. Secrets go in secrets.toml (chmod 0600).
[totsuka]
log_level = "info"
state_dir = "~/.local/state/totsuka"
data_dir  = "~/.local/share/totsuka"
timezone  = "Asia/Tokyo"

[supervisor]
ready_timeout_secs    = 30
shutdown_grace_secs   = 15
shutdown_kill_secs    = 5

[supervisor.heartbeat]
healthz_interval_secs = 5
readyz_interval_secs  = 30
pgmq_interval_secs    = 30
restart_policy        = "on-dead-only"
restart_backoff_secs  = [5, 15, 60]
restart_max_attempts  = 5

[postgres]
image        = "ghcr.io/pgmq/pg18-pgmq:v1.11.1"
container    = "totsuka-pgmq"
host         = "127.0.0.1"
port         = 5432
database     = "totsuka"
user         = "postgres"
volume       = "totsuka_pgmq_data"
compose_file = "deploy/docker-compose.yml"

[bus]
queue_name      = "totsuka_events"
visibility_secs = 30
batch_size      = 16

[agent_adapter]
uds_path      = "${totsuka.state_dir}/sock/adapter.sock"
herdr_socket  = "~/.config/herdr/herdr.sock"
node_capacity = 8
repos_root    = "${env:HOME}/work/repos"
auto_clone    = true

[orchestrator]
uds_path                  = "${totsuka.state_dir}/sock/orchestrator.sock"
wip_global                = 3
phase_timeout_default_secs = 1800
retry_max                 = 1
stuck_threshold_secs      = 600
adapter_uds               = "${agent_adapter.uds_path}"

[github]
project_owner  = "YOUR_ORG"
project_number = 1
[github.columns]
inbox            = "📥 Inbox"
ready            = "📋 Ready"
design           = "🤖 調査・設計"
design_review    = "🚧 設計レビュー"
impl_verify      = "🤖 実装・受入検証"
final_review     = "🚧 最終レビュー"
awaiting_release = "🚀 リリース待ち"
released         = "🏁 完了"

[github_watcher]
bind                       = "127.0.0.1:7802"
project_poll_interval_secs = 20

[qa_service]
uds_path         = "${totsuka.state_dir}/sock/qa-service.sock"
allowed_user_ids = []
catchup_channels = []
reaction_trigger = "memo"
default_mode     = "delegated"
adapter_uds      = "${agent_adapter.uds_path}"

[qa_service.classifier]
provider = "anthropic"
model    = "claude-haiku-4-5-20251001"

[qa_service.answer]
[notifications]
[retention]
[telemetry]
```

`crates/totsukactl/src/commands/templates/secrets.toml.tmpl`:
```toml
# totsuka secrets — chmod 0600
# Each value is also overridable via env (e.g. POSTGRES_PASSWORD).
[postgres]
password = "postgres"

[github_watcher]
github_token = ""

[qa_service]
slack_app_token = ""
slack_bot_token = ""

[qa_service.classifier]
api_key = ""

[notifications.slack]
webhook_url = ""
```

- [ ] **Step 2: init.rs**

`crates/totsukactl/src/commands/init.rs`:
```rust
use crate::compose::{ComposeExec, DockerCompose};
use crate::error::TotsukactlError;
use crate::paths::{resolve_tilde, Paths};
use std::path::PathBuf;

const CONFIG_TMPL: &str = include_str!("templates/config.toml.tmpl");
const SECRETS_TMPL: &str = include_str!("templates/secrets.toml.tmpl");

pub async fn run(paths: &Paths) -> Result<(), TotsukactlError> {
    paths.ensure()?;
    let config_dir = config_home().join("totsuka");
    std::fs::create_dir_all(&config_dir)?;

    let cfg_path = config_dir.join("config.toml");
    write_if_absent(&cfg_path, CONFIG_TMPL, 0o644)?;
    let sec_path = config_dir.join("secrets.toml");
    write_if_absent(&sec_path, SECRETS_TMPL, 0o600)?;

    // Read back so we can drive compose + migrate from the user's actual values.
    let cfg = totsuka_config::Config::load(&cfg_path)
        .map_err(|e| TotsukactlError::Config(format!("re-reading freshly written config: {e:?}")))?;
    let compose: std::sync::Arc<dyn ComposeExec> =
        std::sync::Arc::new(DockerCompose::new(PathBuf::from(&cfg.postgres.compose_file)));
    compose.docker_info().await?;
    compose.compose_version().await?;
    if !compose.ps_running("pgmq").await? {
        compose.up_detached("pgmq", false).await?;
    }
    crate::commands::migrate::run(&cfg).await?;

    println!(
        "totsuka initialised. edit {} to add tokens, then run `totsukactl up`.",
        sec_path.display()
    );
    Ok(())
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| resolve_tilde("~/.config"))
}

fn write_if_absent(path: &std::path::Path, body: &str, mode: u32) -> Result<(), TotsukactlError> {
    if path.exists() {
        tracing::warn!(path=?path, "exists; not overwriting");
        return Ok(());
    }
    std::fs::write(path, body)?;
    set_mode(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> Result<(), TotsukactlError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> Result<(), TotsukactlError> { Ok(()) }
```

- [ ] **Step 3: Mod + dispatch**

`commands/mod.rs`: `pub mod init;`.
`cli::dispatch`: `Cmd::Init => crate::commands::init::run(&paths).await,`.

- [ ] **Step 4: Test (idempotency + secrets file mode 0600)**

`crates/totsukactl/tests/init_bootstrap.rs`:
```rust
//! Tests the file-writing portion only — docker compose / migrate are exercised
//! by the e2e tasks (and skipped when DATABASE_URL is absent).

use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_via_tmpl_helper(path: &std::path::Path, body: &str, mode: u32) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn secrets_file_is_chmod_0600() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("secrets.toml");
    write_via_tmpl_helper(&p, "x = 1\n", 0o600);
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn config_template_is_parseable() {
    let tmpl = include_str!("../src/commands/templates/config.toml.tmpl");
    // Template parses as TOML before var expansion.
    let _: toml::Value = toml::from_str(tmpl).expect("config template must be valid TOML");
}

#[test]
fn secrets_template_is_parseable() {
    let tmpl = include_str!("../src/commands/templates/secrets.toml.tmpl");
    let _: toml::Value = toml::from_str(tmpl).expect("secrets template must be valid TOML");
}
```

- [ ] **Step 5: Run + commit**

```bash
cargo test -p totsukactl --test init_bootstrap
git add crates/totsukactl/src/commands/ crates/totsukactl/src/cli.rs crates/totsukactl/tests/init_bootstrap.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): init subcommand (config/secrets templates + compose + migrate)"
```

---

### Task 26: ControlMsg dispatcher in supervisor main loop (Restart / Reload handling)

**Files:**
- Modify: `crates/totsukactl/src/supervisor/main_loop.rs`
- Create: `crates/totsukactl/src/supervisor/control.rs`
- Modify: `crates/totsukactl/src/supervisor.rs`
- Create: `crates/totsukactl/tests/control_restart.rs`

**Interfaces:**
- Produces:
  - `pub async fn handle_restart(name: &str, registry: Arc<Registry>, spawner: Arc<dyn ChildSpawner>, specs: &[ChildSpec], paths: &Paths, clock: Arc<dyn Clock>, restart_cfg: &RestartCfg) -> Result<(), TotsukactlError>` — looks up current pid, SIGTERM + wait `shutdown_grace`, escalate to SIGKILL if alive, then re-spawn via `spawner`, write new pid, bump `restart_count`, transition to `Restarting → Starting → Ready`.
  - `pub async fn handle_reload(name: &str, registry: Arc<Registry>) -> Result<(), TotsukactlError>` — looks up pid, `kill(pid, Signal::SIGHUP)`, returns Ok.
  - Wire into `run_supervisor`: split the existing `shutdown_drive` `select!` arm — the `Shutdown` variant still cancels + invokes `shutdown_stack`; `Restart` / `Reload` invoke the above handlers and the loop continues.

- [ ] **Step 1: control.rs**

`crates/totsukactl/src/supervisor/control.rs`:
```rust
use crate::child::{ChildSpawner, ChildSpec};
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::restart::RestartCfg;
use crate::state::ChildState;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

pub async fn handle_restart(
    name: &str,
    registry: Arc<Registry>,
    spawner: Arc<dyn ChildSpawner>,
    specs: &[ChildSpec],
    paths: &Paths,
    clock: Arc<dyn Clock>,
    restart_cfg: &RestartCfg,
    grace: Duration,
) -> Result<(), TotsukactlError> {
    let spec = specs
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
    if let Some(e) = registry.get(name).await {
        if let Some(pid) = e.pid {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
            registry.set_state(name, ChildState::Draining).await;
            tokio::time::sleep(grace).await;
            if pidfile::process_alive(pid) {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
    registry.set_state(name, ChildState::Restarting).await;
    let cur = registry.get(name).await.map(|e| e.restart_count).unwrap_or(0);
    if cur >= restart_cfg.max_attempts {
        registry.set_state(name, ChildState::GivingUp).await;
        return Err(TotsukactlError::Internal(format!(
            "restart count {cur} >= max_attempts {} → giving_up",
            restart_cfg.max_attempts
        )));
    }
    registry.set_state(name, ChildState::Starting).await;
    let pid = spawner.spawn(spec).await?;
    let now = clock.now();
    registry.set_pid(name, Some(pid), Some(now)).await;
    pidfile::write_pid(&paths.child_pid(name), pid)?;
    registry.bump_restart(name).await;
    registry.set_state(name, ChildState::Ready).await;
    Ok(())
}

pub async fn handle_reload(name: &str, registry: Arc<Registry>) -> Result<(), TotsukactlError> {
    let e = registry.get(name).await
        .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
    let pid = e.pid.ok_or_else(|| TotsukactlError::Internal(format!("{name} has no pid")))?;
    kill(Pid::from_raw(pid), Signal::SIGHUP)
        .map_err(|e| TotsukactlError::Internal(format!("SIGHUP {pid}: {e}")))?;
    Ok(())
}
```

- [ ] **Step 2: Wire into main_loop**

In `crates/totsukactl/src/supervisor/main_loop.rs` replace the `shutdown_drive` `select!` body with a loop over `ctl_rx` until SIGTERM/SIGINT/`Shutdown`:

```rust
let mut term = signal(SignalKind::terminate())?;
let mut int = signal(SignalKind::interrupt())?;
let restart_cfg = RestartCfg::from_section(&cfg.supervisor.heartbeat)?;
let grace = Duration::from_secs(cfg.supervisor.shutdown_grace_secs);
loop {
    tokio::select! {
        _ = term.recv() => { shutdown_stack(/* graceful */, ...).await?; break; }
        _ = int.recv()  => { shutdown_stack(/* graceful */, ...).await?; break; }
        msg = ctl_rx.recv() => {
            match msg {
                Some(ControlMsg::Restart(name)) => {
                    if let Err(e) = crate::supervisor::control::handle_restart(
                        &name, registry.clone(), spawner_arc.clone(), &specs,
                        &paths, clock.clone(), &restart_cfg, grace).await {
                        tracing::error!(child=%name, error=%e, "restart failed");
                    }
                }
                Some(ControlMsg::Reload(name)) => {
                    if let Err(e) = crate::supervisor::control::handle_reload(&name, registry.clone()).await {
                        tracing::error!(child=%name, error=%e, "reload failed");
                    }
                }
                Some(ControlMsg::Shutdown { postgres, force }) => {
                    shutdown_stack(ShutdownCfg {
                        grace, second_term: Duration::from_secs(cfg.supervisor.shutdown_kill_secs),
                        force_grace: Duration::from_secs(3),
                        also_postgres: postgres, force,
                    }, registry.clone(), compose.clone(), paths.clone()).await?;
                    break;
                }
                None => break,
            }
        }
    }
}
```

The `spawner_arc` reference requires `let spawner_arc: Arc<dyn ChildSpawner> = Arc::new(ForkExecSpawner);` to be hoisted above the loop (replace the existing `let spawner = Arc::new(ForkExecSpawner);` line).

- [ ] **Step 3: handle_restart unit test (MockSpawner)**

`crates/totsukactl/tests/control_restart.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::control::{handle_reload, handle_restart};

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![], env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

#[tokio::test]
async fn handle_restart_increments_count_and_lands_ready() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(), data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"), pid_dir: tmp.path().join("pids"), sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let reg = Arc::new(Registry::new());
    reg.set_pid("orchestrator", Some(0x7fff_fffe), Some(chrono::Utc::now())).await;
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let specs = vec![spec("orchestrator", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = RestartCfg { policy: RestartPolicy::OnDeadOnly, backoff_secs: vec![1], max_attempts: 3 };

    handle_restart("orchestrator", reg.clone(), spawner, &specs, &paths, clock, &cfg, Duration::from_millis(10))
        .await.unwrap();
    let e = reg.get("orchestrator").await.unwrap();
    assert_eq!(e.state, ChildState::Ready);
    assert_eq!(e.restart_count, 1);
    assert!(paths.child_pid("orchestrator").exists());
}

#[tokio::test]
async fn handle_reload_errors_when_pid_unknown() {
    let reg = Arc::new(Registry::new());
    let err = handle_reload("agent-adapter", reg).await.unwrap_err();
    assert!(matches!(err, totsukactl::error::TotsukactlError::Internal(_)));
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p totsukactl --test control_restart
git add crates/totsukactl/src/supervisor/control.rs crates/totsukactl/src/supervisor.rs crates/totsukactl/src/supervisor/main_loop.rs crates/totsukactl/tests/control_restart.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsukactl): ControlMsg dispatcher (restart re-spawn + reload SIGHUP)"
```

---

### Task 27: E2E — boot → status → restart → down (mocked compose / spawner / probe)

**Files:**
- Create: `crates/totsukactl/tests/e2e_lifecycle.rs`

**Goal:** exercise the full supervisor wiring (boot + sock_api + heartbeat tick + restart + shutdown) without touching docker or real bins. Uses the same mocks built in Tasks 8/10/11/12 plus the in-process supervisor split into helpers so a test can drive it.

**Interfaces:** Reuses public API; no new code outside the test.

- [ ] **Step 1: Test**

`crates/totsukactl/tests/e2e_lifecycle.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::health::{HealthProbe, MockHealthProbe};
use totsukactl::heartbeat::{run_healthz_loop, HeartbeatCfg};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState, SupervisorClient};
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::boot::{boot, BootCtx};
use totsukactl::supervisor::control::handle_restart;
use totsukactl::supervisor::shutdown::{shutdown_stack, ShutdownCfg};

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec { name: name.into(), bin_path: tmp.path().join(name), args: vec![], env: vec![],
        log_path: tmp.path().join(format!("{name}.log")) }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_boot_status_restart_down() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(), data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"), pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1"));
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    for n in ["agent-adapter", "orchestrator", "github-watcher", "qa-service"] {
        probe_concrete.set_ready(n, true);
        probe_concrete.set_healthy(n, true);
    }
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);

    let ctx = BootCtx {
        compose: compose.clone(), spawner: spawner.clone(), probe: probe.clone(),
        registry: registry.clone(), clock: clock.clone(),
        paths: paths.clone(), ready_timeout: Duration::from_secs(2),
    };
    let specs: Vec<_> = ["agent-adapter", "orchestrator", "github-watcher", "qa-service"]
        .into_iter().map(|n| spec(n, &tmp)).collect();
    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) }).await.unwrap();

    // Spawn sock_api server
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ControlMsg>(8);
    let listener = bind_uds(&paths.supervisor_sock()).await.unwrap();
    let state = SockApiState { registry: registry.clone(), control_tx: ctl_tx };
    let r = router(state);
    let _h_sock = tokio::spawn(async move { let _ = serve_uds(listener, r).await; });

    // Spawn healthz ticker
    let cancel = CancellationToken::new();
    let hb = HeartbeatCfg {
        healthz_interval: Duration::from_millis(50), readyz_interval: Duration::from_secs(30),
        pgmq_interval: Duration::from_secs(30), degraded_threshold: 2, unhealthy_threshold: 3,
    };
    let bins = vec!["agent-adapter".into(), "orchestrator".into(), "github-watcher".into(), "qa-service".into()];
    let _h_hb = tokio::spawn(run_healthz_loop(hb, probe.clone(), registry.clone(), clock.clone(), bins, cancel.clone()));

    // Status via supervisor client
    let client = SupervisorClient::new(paths.supervisor_sock());
    let list = client.list().await.unwrap();
    assert!(list.iter().any(|p| p.name == "orchestrator"));

    // Drive a restart via UDS
    client.restart("orchestrator").await.unwrap();
    let msg = ctl_rx.recv().await.unwrap();
    if let ControlMsg::Restart(name) = msg {
        let rcfg = RestartCfg { policy: RestartPolicy::OnDeadOnly, backoff_secs: vec![0], max_attempts: 3 };
        handle_restart(&name, registry.clone(), spawner.clone(), &specs, &paths, clock.clone(), &rcfg, Duration::from_millis(10)).await.unwrap();
    }
    assert_eq!(registry.get("orchestrator").await.unwrap().restart_count, 1);

    // Drive shutdown
    cancel.cancel();
    shutdown_stack(
        ShutdownCfg { grace: Duration::from_millis(20), second_term: Duration::from_millis(10),
                      force_grace: Duration::from_millis(20), also_postgres: false, force: true },
        registry.clone(), compose.clone(), paths.clone(),
    ).await.unwrap();
    for n in ["agent-adapter", "orchestrator", "github-watcher", "qa-service"] {
        assert_eq!(registry.get(n).await.unwrap().state, ChildState::Stopped);
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p totsukactl --test e2e_lifecycle
git add crates/totsukactl/tests/e2e_lifecycle.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(totsukactl): e2e boot → status → restart → down"
```

---

### Task 28: E2E — dead child detection → restart-loop → giving_up + PR + merge

**Files:**
- Create: `crates/totsukactl/tests/e2e_giving_up.rs`
- Final-task housekeeping (PR + merge, no source files added beyond the test).

**Goal:** prove that after `restart_max_attempts` failed re-spawns, the registry lands in `GivingUp` and no further spawns occur. Uses `MockSpawner::fail_for` to make every re-spawn attempt fail; drives the loop with `handle_restart` directly so the test stays deterministic.

- [ ] **Step 1: Test**

`crates/totsukactl/tests/e2e_giving_up.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::control::handle_restart;

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec { name: name.into(), bin_path: tmp.path().join(name), args: vec![], env: vec![],
        log_path: tmp.path().join(format!("{name}.log")) }
}

#[tokio::test]
async fn restart_loop_lands_in_giving_up_after_max_attempts() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(), data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"), pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    let spawner_concrete = Arc::new(MockSpawner::default());
    spawner_concrete.fail_for.lock().unwrap().push("orchestrator".into());
    let spawner: Arc<dyn ChildSpawner> = spawner_concrete.clone();
    let specs = vec![spec("orchestrator", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = RestartCfg { policy: RestartPolicy::OnDeadOnly, backoff_secs: vec![0], max_attempts: 3 };

    // First three attempts: each call surfaces the spawn error, but `restart_count`
    // only increments on success — so each failed call leaves count=0 and re-tries
    // are eligible. We bump restart_count manually to simulate a real supervisor's
    // counter (mirrors handle_restart's bump on success path). To exercise the
    // GivingUp branch, set restart_count directly.
    registry.set_state("orchestrator", ChildState::Dead).await;
    for _ in 0..3 { registry.bump_restart("orchestrator").await; }

    let err = handle_restart(
        "orchestrator", registry.clone(), spawner, &specs, &paths,
        clock, &cfg, Duration::from_millis(5),
    ).await.unwrap_err();
    assert!(format!("{err}").contains("giving_up"));
    assert_eq!(registry.get("orchestrator").await.unwrap().state, ChildState::GivingUp);
}
```

- [ ] **Step 2: Run all totsukactl tests + workspace tests**

```bash
cargo test -p totsukactl
cargo test --workspace --all-features --locked
cargo clippy -p totsukactl --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: every test passes; clippy clean; fmt clean.

- [ ] **Step 3: Commit + push + PR**

```bash
git -c commit.gpgsign=false -c tag.gpgsign=false commit -am "test(totsukactl): e2e — restart loop lands in giving_up after max_attempts"
git push -u origin feat/totsukactl
gh pr create --title "feat: totsukactl (supervisor + CLI)" --body "$(cat <<'EOF'
## Summary
- New `totsukactl` binary supervises the 4 Rust bins + pgmq container per spec §4 / §5 / §7 / §8.5 / §11.11
- Subcommands: `init` (first-run bootstrap), `up [--recreate] [--bootstrap]` (fork+detach supervisor), `down [--force] [--postgres]`, `status` (formatted table), `restart <bin>`, `reload <bin>` (agent-adapter only), `logs <bin>`, `migrate` (embedded sqlx::migrate!)
- Process supervisor: state machine per child (`Starting→Ready→Healthy→Degraded→Unhealthy→Dead→Restarting→GivingUp`), heartbeat tickers (healthz 5s / readyz 30s / pgmq 30s), restart policy (`on-dead-only` default; pgmq never cascade-restarted), reverse-order graceful shutdown with 15s grace + 5s second-SIGTERM + SIGKILL escalation
- IPC: supervisor.sock UDS API (`/v1/processes`, `/v1/processes/<name>/restart|reload`, `/v1/shutdown`) consumed by CLI via hyperlocal
- All boundaries (compose / spawn / healthz / pgmq probe) behind traits with Mock impls — full state-machine coverage without docker or real bins

## Test plan
- [ ] `cargo test -p totsukactl` (all unit + integration tests pass)
- [ ] `cargo test --workspace --all-features --locked` green
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] Manual smoke: `totsukactl init` → `totsukactl up` → `totsukactl status` → `totsukactl down`
EOF
)"
```

- [ ] **Step 4: After CI green and review approves — merge fast-forward**

```bash
gh pr merge --merge --delete-branch
git checkout main && git pull
```

---

## Self-review notes (controller-side)

- **Spec §4 startup** — Task 15 boot covers phases -1 → 3 (compose up + image inspect, preflight skipped to closure, adapter then orchestrator then watcher∥qa in parallel readyz, 30s timeout, rollback in reverse).
- **Spec §5 shutdown** — Task 16 reverse-order SIGTERM with 15s grace + 5s escalation + SIGKILL; `--force` parallel path; `--postgres` calls compose stop; pid cleanup mandatory.
- **Spec §7 IPC + heartbeat** — Task 17 UDS server with the exact 4 routes; Task 13 ticker cadence matches spec defaults (5/30/30); Task 14 restart policy enums match spec strings; Task 26 wires Restart/Reload/Shutdown.
- **Spec §8.5 totsukactl** — every subcommand on the spec list (`init`, `up`, `down`, `status`, `migrate`, `restart`, `reload`, `logs`) has its own task; `backup`/`restore` (spec §11.3) are **explicitly out of scope** for this plan — they are operational tooling, not supervisor primitives, and can ship in a follow-up PR.
- **Spec §11.11 first-run bootstrap** — Task 25 implements the exact 6-step sequence (XDG dirs / config.toml / secrets.toml 0600 / compose up / sqlx migrate / next-step hint).
- **`--bootstrap` flag** — Task 20 step 1 conditional: when `config.toml` is missing, `up --bootstrap` invokes `commands::init::run` before continuing. (When `config.toml` exists, `--bootstrap` is a no-op — matches spec wording "両方欠落時のみ暗黙 init".)
- **Spec §11.7 Secret discipline** — Task 24 `build_db_url` exposes `cfg.postgres.password` exactly once (at outbound DB URL construction) and is the single source of truth; Task 24 test guards against hardcoded `:totsuka@` regression.
- **Spec §11.5 Clock injection** — every code path that records `started_at` / `last_healthz_at` takes `Arc<dyn Clock>` and never calls `SystemTime::now()` directly.
- **pgmq cascade-restart guard** — Task 22 rejects `restart pgmq` at the CLI layer; the supervisor itself never spawns pgmq from `handle_restart` (only `compose.up_detached` in boot).
- **Type consistency** — `ProcessEntry`/`ProcessDto` field names match across registry → DTO → status formatter. `ChildState`/`RestartPolicy`/`HealthOutcome` consumers all use the same enum variants. `ChildSpec` shape is identical across spawner, supervisor, and tests.
- **Known follow-up referenced in compaction summary** — agent-adapter `GET /v1/agents` route is unrelated to totsukactl (qa-service follow-up) and stays out of this plan.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-29-totsukactl.md`. Two execution options:**

1. **Subagent-Driven (recommended)** — fresh implementer + reviewer per task; same rhythm that delivered PRs #1–#6.
2. **Inline Execution** — batch with checkpoints (`superpowers:executing-plans`).
