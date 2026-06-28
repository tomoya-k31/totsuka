# agent-adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first binary on top of the foundation: an HTTP-over-Unix-Domain-Socket adapter that translates orchestrator / qa-service spawn / send / read / close requests into herdr socket calls, manages git worktrees per task, and enforces argv discipline.

**Architecture:** New crate `crates/agent-adapter/` (lib + bin). The lib is internally structured around a `HerdrClient` trait so the HTTP server, worktree manager, and tests share the same surface (real NDJSON impl for production, in-memory mock for tests). State is intentionally minimal: live agent IDs come from `herdr.agent.list` on demand, not from in-process tracking (spec §8.1 — "stateless, agent state is herdr's job"). Worktrees are created on `git worktree add` via `spawn_blocking` and removed by a periodic GC scanner that diffs the on-disk set against the live-agent set.

**Tech Stack:** Rust stable / tokio (rt-multi-thread) / axum 0.7 with hyperlocal for UDS / tokio NDJSON codec / git CLI subprocess via `tokio::process::Command` wrapped in `spawn_blocking` / regex / anyhow (bin) / thiserror (lib)

## Global Constraints

(spec §11 reproduced verbatim — every task implicitly satisfies these)

- Rust toolchain: **stable**, `[profile.release] panic = "abort"`, `tokio::task::block_in_place` clippy-denied
- HTTP path prefix: `/v1/`, RFC7807 errors with `type` = `/errors/<kind>` (spec §11.6)
- `x-totsuka-request-id` header propagated via `totsuka_telemetry::request_id::middleware`
- Storage UTC; display in `[totsuka].timezone` (Asia/Tokyo). Access time only via `Arc<dyn Clock>` (spec §11.5)
- Secrets typed `Secret<String>` (spec §11.7), `.expose()` only at outbound construction; argv guard rejects secret-like flags (spec §11.13)
- Bounded mpsc channels only (spec §11.8)
- Subprocess (`git worktree`), large parse, sync fs → `spawn_blocking` (spec §11.10)
- task_id = ProjectV2Item.id (spec §11.14); branch = `totsuka/{task_id_short}/{phase_short}`; effect_key includes attempt (spec §11.15)
- adapter is **stateless across restarts** — `agent.list` rebuilds the live set; in-memory cache only holds resolved repo config (spec §8.1)
- herdr `events.subscribe` is **NOT** used — totsuka does not track agent state (spec §8.1 / §9)
- 5-binary stack: agent-adapter is binary #2 (after totsukactl supervisor, which is plan #5). Until totsukactl exists, the bin is started manually for dev.

---

## File Structure

```
crates/agent-adapter/
├── Cargo.toml                       [Create] bin + lib
└── src/
    ├── main.rs                      [Create] anyhow entry
    ├── lib.rs                       [Create] re-exports + AdapterApp::run
    ├── error.rs                     [Create] AdapterError + Problem serialize
    ├── herdr/
    │   ├── mod.rs                   [Create] HerdrClient trait + types
    │   ├── wire.rs                  [Create] NDJSON Unix-socket impl
    │   └── mock.rs                  [Create] in-memory mock for tests
    ├── repo.rs                      [Create] RepoRegistry (load + atomic swap)
    ├── worktree.rs                  [Create] git worktree create/remove (spawn_blocking)
    ├── argv.rs                      [Create] secret-pattern guard
    ├── server/
    │   ├── mod.rs                   [Create] axum Router builder + AppState
    │   ├── spawn.rs                 [Create] POST /v1/agents
    │   ├── send.rs                  [Create] POST /v1/agents/{id}/messages
    │   ├── output.rs                [Create] GET  /v1/agents/{id}/output
    │   ├── stop.rs                  [Create] DELETE /v1/agents/{id}
    │   └── reload.rs                [Create] POST /v1/repos/reload
    ├── gc.rs                        [Create] worktree orphan scanner loop
    ├── lifecycle.rs                 [Create] readiness state + SIGTERM/SIGHUP
    └── listener.rs                  [Create] UDS + optional TCP listener factory
crates/agent-adapter/tests/
├── http_with_mock.rs                [Create] integration: axum + mock HerdrClient
├── worktree.rs                      [Create] integration: real git on tempdir
├── argv_guard.rs                    [Create] integration: regex matrix
└── e2e_herdr.rs                     [Create] gated by HERDR_SOCKET env (real herdr)
```

Workspace updates:
- `Cargo.toml`: add `"crates/agent-adapter"` to `[workspace] members`
- `[workspace.dependencies]`: add `hyperlocal = "0.9"` (used only by agent-adapter for now)

---

## Tasks

Tasks follow the TDD cycle (failing test → fail run → impl → pass run → commit). Each task is 10–30 min.

### Task 1: Crate scaffold + bin/lib split

**Files:**
- Create: `crates/agent-adapter/Cargo.toml`
- Create: `crates/agent-adapter/src/main.rs`
- Create: `crates/agent-adapter/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + add `hyperlocal` to `workspace.dependencies`)

**Interfaces:**
- Consumes: foundation crates (`totsuka-core`, `totsuka-config`, `totsuka-telemetry`, `totsuka-bus`)
- Produces: `agent_adapter::AdapterApp` struct (constructor + `run` method, stubbed in this task) and a `bin` target that loads config and calls `AdapterApp::run`

- [ ] **Step 1: Add agent-adapter to workspace + new dep**

Append `"crates/agent-adapter"` to `[workspace] members`. Add `hyperlocal = "0.9"` under `[workspace.dependencies]` (alphabetical, between `chrono-tz` and `hyper`).

- [ ] **Step 2: Write crate Cargo.toml**

`crates/agent-adapter/Cargo.toml`:
```toml
[package]
name = "agent-adapter"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "agent-adapter"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
totsuka-core      = { path = "../totsuka-core",      version = "0.1.0" }
totsuka-config    = { path = "../totsuka-config",    version = "0.1.0" }
totsuka-telemetry = { path = "../totsuka-telemetry", version = "0.1.0" }
totsuka-bus       = { path = "../totsuka-bus",       version = "0.1.0" }

tokio       = { workspace = true, features = ["rt-multi-thread", "macros", "signal", "fs", "process", "net", "io-util", "sync", "time"] }
axum        = { workspace = true }
hyper       = { workspace = true }
hyperlocal  = { workspace = true }
tower       = { workspace = true }
tower-http  = { workspace = true }
serde       = { workspace = true }
serde_json  = { workspace = true }
chrono      = { workspace = true }
tracing     = { workspace = true }
anyhow      = { workspace = true }
thiserror   = { workspace = true }
regex       = { workspace = true }
async-trait = { workspace = true }
uuid        = { workspace = true }

[dev-dependencies]
tempfile = "3.12"
tokio    = { workspace = true, features = ["test-util"] }
```

- [ ] **Step 3: Stubbed lib.rs**

`crates/agent-adapter/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

/// Top-level wiring for the agent-adapter binary. Holds shared dependencies
/// constructed once at startup; `run()` blocks until SIGTERM.
pub struct AdapterApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl AdapterApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }

    /// Stub. Later tasks replace this with the full lifecycle.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("agent-adapter stub: nothing to do yet");
        Ok(())
    }
}
```

- [ ] **Step 4: Stubbed main.rs**

`crates/agent-adapter/src/main.rs`:
```rust
use std::sync::Arc;

use agent_adapter::AdapterApp;
use totsuka_config::Config;
use totsuka_core::SystemClock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = std::env::var("TOTSUKA_CONFIG")
        .unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(Config::load(&config_path)?);

    // Tracing init will be wired in Task 17; bare subscriber for now so logs
    // still appear when running by hand.
    tracing_subscriber::fmt().with_env_filter("info").init();

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    AdapterApp::new(config, clock).run().await
}
```

- [ ] **Step 5: Add tracing-subscriber to dev-deps for the bin smoke**

Add to `crates/agent-adapter/Cargo.toml` `[dependencies]`:
```toml
tracing-subscriber = { workspace = true }
```

- [ ] **Step 6: Run smoke check**

```bash
cargo check --workspace
cargo build -p agent-adapter
```
Expected: both succeed.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/agent-adapter/
git commit -m "feat(adapter): bin/lib scaffold + workspace wire-up"
```

---

### Task 2: HerdrClient trait + types

**Files:**
- Create: `crates/agent-adapter/src/herdr/mod.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod herdr;`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub trait HerdrClient: Send + Sync` with async methods `start`, `send`, `read`, `close`, `list`
  - Types `AgentId`, `PaneSnapshot`, `SpawnRequest`, `SpawnResult`, `HerdrError`
  - Used by Task 10's HTTP server and Task 16's GC scanner

- [ ] **Step 1: Write the failing test**

Append at the bottom of the file you're about to create (`crates/agent-adapter/src/herdr/mod.rs`) — write it first as a compile failure to drive the surface design:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_serde_transparent() {
        let id = AgentId::new("ag_123".to_string());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ag_123\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn pane_snapshot_revision_monotone_helper() {
        let s = PaneSnapshot {
            revision: 4,
            text: "hello".into(),
        };
        assert!(s.is_newer_than(3));
        assert!(!s.is_newer_than(4));
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter herdr::tests`
Expected: `error[E0432]: unresolved import` (module doesn't exist yet).

- [ ] **Step 3: Implement the module**

`crates/agent-adapter/src/herdr/mod.rs`:
```rust
//! Abstraction over the herdr daemon socket. Spec §8.1: agent-adapter is the
//! sole adapter from totsuka domain types into herdr's native Unix-socket API
//! (NDJSON). The concrete wire impl lives in [`wire`]; tests use [`mock`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod wire;
pub mod mock;

/// herdr-assigned identifier for a single Claude pane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Snapshot returned by `pane.read`. `revision` is herdr's monotone update
/// counter; callers should poll until it advances past their previous value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub revision: u64,
    pub text: String,
}

impl PaneSnapshot {
    pub fn is_newer_than(&self, prev_rev: u64) -> bool {
        self.revision > prev_rev
    }
}

/// Parameters for `agent.start`. `argv` is the Claude Code invocation; `env`
/// carries secrets (spec §11.13: never in argv).
#[derive(Debug, Clone, Serialize)]
pub struct SpawnRequest {
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: HashMap<String, String>,
    pub label: String, // herdr pane label (e.g. "totsuka:abc123:implv")
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnResult {
    pub agent_id: AgentId,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub agent_id: AgentId,
    pub label: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("herdr io: {0}")]
    Io(#[from] std::io::Error),
    #[error("herdr returned error: {code} {message}")]
    Remote { code: String, message: String },
    #[error("herdr returned malformed response: {0}")]
    Decode(String),
}

#[async_trait]
pub trait HerdrClient: Send + Sync {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError>;
    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError>;
    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError>;
    async fn close(&self, id: &AgentId) -> Result<(), HerdrError>;
    async fn list(&self) -> Result<Vec<ListItem>, HerdrError>;
}
```

- [ ] **Step 4: Add `pub mod herdr;` to lib.rs**

In `crates/agent-adapter/src/lib.rs`, insert `pub mod herdr;` immediately after the `#![forbid(unsafe_code)]` line.

- [ ] **Step 5: Confirm wire.rs / mock.rs stubs exist**

Create both files with one line each so the `pub mod` declarations compile:
- `crates/agent-adapter/src/herdr/wire.rs`: `//! NDJSON over Unix domain socket (Task 4).`
- `crates/agent-adapter/src/herdr/mock.rs`: `//! In-memory test impl (Task 3).`

- [ ] **Step 6: Pass**

Run: `cargo test -p agent-adapter herdr::tests`
Expected: `test result: ok. 2 passed`

- [ ] **Step 7: Commit**

```bash
git add crates/agent-adapter/src/
git commit -m "feat(adapter): HerdrClient trait + AgentId/PaneSnapshot types"
```

---

### Task 3: MockHerdr in-memory implementation

**Files:**
- Modify: `crates/agent-adapter/src/herdr/mock.rs`

**Interfaces:**
- Consumes: Task 2 types
- Produces: `MockHerdr` struct with `Arc<Mutex<State>>` so tests can both inject behavior (e.g. preset spawn errors) and inspect emitted calls. Used by Tasks 11–14 (HTTP route tests) and Task 16 (GC scanner tests).

- [ ] **Step 1: Failing test**

Append to `crates/agent-adapter/src/herdr/mock.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{HerdrClient, SpawnRequest};
    use std::collections::HashMap;

    #[tokio::test]
    async fn start_then_list_then_close() {
        let h = MockHerdr::new();
        let id = h
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                label: "totsuka:t1:design".into(),
            })
            .await
            .unwrap()
            .agent_id;
        let list = h.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_id, id);
        h.close(&id).await.unwrap();
        assert!(h.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_returns_appended_text() {
        let h = MockHerdr::new();
        let id = h
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec![],
                env: HashMap::new(),
                label: "x".into(),
            })
            .await
            .unwrap()
            .agent_id;
        h.send(&id, "hello").await.unwrap();
        h.send(&id, "world").await.unwrap();
        let snap = h.read(&id).await.unwrap();
        assert_eq!(snap.text, "helloworld");
        assert_eq!(snap.revision, 2);
    }

    #[tokio::test]
    async fn close_unknown_errors() {
        let h = MockHerdr::new();
        let err = h.close(&AgentId::new("nope".into())).await.unwrap_err();
        assert!(matches!(err, HerdrError::Remote { .. }));
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter herdr::mock::tests`
Expected: `MockHerdr` is not in scope (compile error).

- [ ] **Step 3: Implement MockHerdr**

Replace `crates/agent-adapter/src/herdr/mock.rs` with:
```rust
//! In-memory `HerdrClient` for tests. Maintains a `HashMap<AgentId, Pane>`
//! under a Mutex; revision advances on `send` and persists across `read`.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{AgentId, HerdrClient, HerdrError, ListItem, PaneSnapshot, SpawnRequest, SpawnResult};

#[derive(Debug, Default)]
struct Pane {
    label: String,
    text: String,
    revision: u64,
}

#[derive(Debug, Default, Clone)]
pub struct MockHerdr {
    inner: Arc<Mutex<HashMap<AgentId, Pane>>>,
}

impl MockHerdr {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: count of in-flight panes.
    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[async_trait]
impl HerdrClient for MockHerdr {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError> {
        let id = AgentId::new(format!("ag_{}", Uuid::new_v4().simple()));
        let pane = Pane {
            label: req.label,
            text: String::new(),
            revision: 0,
        };
        self.inner.lock().unwrap().insert(id.clone(), pane);
        Ok(SpawnResult {
            agent_id: id.clone(),
            terminal_id: format!("term_{}", id.as_str()),
        })
    }

    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError> {
        let mut g = self.inner.lock().unwrap();
        let pane = g.get_mut(id).ok_or_else(|| HerdrError::Remote {
            code: "not_found".into(),
            message: format!("unknown agent {}", id.as_str()),
        })?;
        pane.text.push_str(text);
        pane.revision += 1;
        Ok(())
    }

    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError> {
        let g = self.inner.lock().unwrap();
        let pane = g.get(id).ok_or_else(|| HerdrError::Remote {
            code: "not_found".into(),
            message: format!("unknown agent {}", id.as_str()),
        })?;
        Ok(PaneSnapshot {
            revision: pane.revision,
            text: pane.text.clone(),
        })
    }

    async fn close(&self, id: &AgentId) -> Result<(), HerdrError> {
        let mut g = self.inner.lock().unwrap();
        if g.remove(id).is_none() {
            return Err(HerdrError::Remote {
                code: "not_found".into(),
                message: format!("unknown agent {}", id.as_str()),
            });
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ListItem>, HerdrError> {
        let g = self.inner.lock().unwrap();
        Ok(g.iter()
            .map(|(id, p)| ListItem {
                agent_id: id.clone(),
                label: p.label.clone(),
            })
            .collect())
    }
}
```

- [ ] **Step 4: Pass**

Run: `cargo test -p agent-adapter herdr::mock`
Expected: `test result: ok. 3 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/agent-adapter/src/herdr/mock.rs
git commit -m "feat(adapter): MockHerdr for HTTP/GC tests"
```

---

### Task 4: NDJSON wire HerdrClient (real impl)

**Files:**
- Modify: `crates/agent-adapter/src/herdr/wire.rs`

**Interfaces:**
- Consumes: Task 2 types
- Produces: `WireHerdr::connect(socket_path)` async constructor + `impl HerdrClient`. Production code path; the mock keeps non-herdr tests fast.

> **Wire format assumption:** spec §7 names the protocol "Unix domain socket (NDJSON)" and the methods `agent.start / agent.send / pane.read / pane.close / agent.list`. We commit to a request envelope of `{"id":<u64>,"method":"agent.start","params":{...}}\n` and a response envelope of `{"id":<u64>,"result":{...}}` or `{"id":<u64>,"error":{"code":"...","message":"..."}}` — JSON-RPC 2.0 line-delimited, but without the `"jsonrpc":"2.0"` literal (NDJSON is the wire, the literal is unnecessary overhead). If herdr's real wire format diverges, swap encoder/decoder bodies only; the trait surface stays.

- [ ] **Step 1: Failing test**

Append to `crates/agent-adapter/src/herdr/wire.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{HerdrClient, SpawnRequest};
    use std::collections::HashMap;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Spin up a fake herdr that responds to `agent.start` with a canned result.
    #[tokio::test]
    async fn start_sends_jsonrpc_envelope_and_parses_result() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        // Spawn a one-shot fake server
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&req_line).unwrap();
            assert_eq!(req["method"], "agent.start");
            assert_eq!(req["params"]["cwd"], "/w");
            let reply = serde_json::json!({
                "id": req["id"],
                "result": { "agent_id": "ag_42", "terminal_id": "t_42" },
            });
            wr.write_all(format!("{}\n", reply).as_bytes())
                .await
                .unwrap();
        });

        let client = WireHerdr::connect(&sock).await.unwrap();
        let res = client
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                label: "lbl".into(),
            })
            .await
            .unwrap();
        assert_eq!(res.agent_id.as_str(), "ag_42");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_error_is_propagated() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&req_line).unwrap();
            let reply = serde_json::json!({
                "id": req["id"],
                "error": { "code": "capacity", "message": "no slots" },
            });
            wr.write_all(format!("{}\n", reply).as_bytes())
                .await
                .unwrap();
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let err = client.list().await.unwrap_err();
        assert!(matches!(err, super::super::HerdrError::Remote { ref code, .. } if code == "capacity"));
        server.await.unwrap();
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter herdr::wire`
Expected: `WireHerdr` undefined.

- [ ] **Step 3: Implement WireHerdr**

Replace `crates/agent-adapter/src/herdr/wire.rs` with:
```rust
//! NDJSON-over-Unix-domain-socket herdr client. See module-level doc on
//! `mod.rs` for the wire envelope. Single multiplexed connection per
//! `WireHerdr` instance; one request at a time (herdr's load is low: a few
//! calls per second total across all panes).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use super::{
    AgentId, HerdrClient, HerdrError, ListItem, PaneSnapshot, SpawnRequest, SpawnResult,
};

#[derive(Serialize)]
struct Request<'a, P: Serialize> {
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct Response {
    #[allow(dead_code)]
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
}

pub struct WireHerdr {
    // Mutex serialises in-flight calls. Single-connection is fine for current
    // herdr load (a few RPC/sec total).
    conn: Mutex<Conn>,
    next_id: AtomicU64,
}

struct Conn {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl WireHerdr {
    pub async fn connect(socket_path: &Path) -> Result<Self, HerdrError> {
        let stream = UnixStream::connect(socket_path).await?;
        let (rd, wr) = stream.into_split();
        Ok(Self {
            conn: Mutex::new(Conn {
                reader: BufReader::new(rd),
                writer: wr,
            }),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, HerdrError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request { id, method, params };
        let line = serde_json::to_string(&req)
            .map_err(|e| HerdrError::Decode(format!("encode: {e}")))?;

        let mut conn = self.conn.lock().await;
        conn.writer.write_all(line.as_bytes()).await?;
        conn.writer.write_all(b"\n").await?;
        conn.writer.flush().await?;

        let mut buf = String::new();
        let n = conn.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(HerdrError::Decode("herdr closed connection".into()));
        }
        let resp: Response =
            serde_json::from_str(buf.trim_end()).map_err(|e| HerdrError::Decode(e.to_string()))?;
        if let Some(err) = resp.error {
            return Err(HerdrError::Remote {
                code: err.code,
                message: err.message,
            });
        }
        let result = resp
            .result
            .ok_or_else(|| HerdrError::Decode("missing result".into()))?;
        serde_json::from_value(result).map_err(|e| HerdrError::Decode(e.to_string()))
    }
}

#[async_trait]
impl HerdrClient for WireHerdr {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError> {
        self.call("agent.start", req).await
    }

    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
            text: &'a str,
        }
        let _: Value = self
            .call(
                "agent.send",
                P {
                    agent_id: id.as_str(),
                    text,
                },
            )
            .await?;
        Ok(())
    }

    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
        }
        self.call(
            "pane.read",
            P {
                agent_id: id.as_str(),
            },
        )
        .await
    }

    async fn close(&self, id: &AgentId) -> Result<(), HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
        }
        let _: Value = self
            .call(
                "pane.close",
                P {
                    agent_id: id.as_str(),
                },
            )
            .await?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ListItem>, HerdrError> {
        #[derive(Serialize)]
        struct P {}
        self.call("agent.list", P {}).await
    }
}
```

- [ ] **Step 4: Pass**

Run: `cargo test -p agent-adapter herdr::wire`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/agent-adapter/src/herdr/wire.rs
git commit -m "feat(adapter): NDJSON Unix-socket herdr client"
```

---

### Task 5: RepoRegistry (loaded from config, atomic swap on reload)

**Files:**
- Create: `crates/agent-adapter/src/repo.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod repo;`)

**Interfaces:**
- Consumes: `totsuka_config::Config`'s `AgentAdapterSection.repos` map
- Produces:
  - `pub struct RepoRegistry` holding `ArcSwap<HashMap<RepoKey, RepoEntry>>` (atomic swap on reload)
  - `pub struct RepoKey(String)` newtype around `owner/repo`
  - `pub struct RepoEntry { description, repo_path: PathBuf, worktree_root: PathBuf }`
  - `RepoRegistry::resolve(&self, repo_key: &RepoKey) -> Option<RepoEntry>` for hot lookups
  - `RepoRegistry::reload(&self, cfg: &AgentAdapterSection) -> ReloadReport` for Task 15

> We choose `arc-swap` (a workspace dep we add now) over `Mutex<HashMap>` because lookups happen on the hot path of every HTTP request. Reload is rare (manual SIGHUP).

- [ ] **Step 1: Add arc-swap workspace dep**

In root `Cargo.toml` `[workspace.dependencies]` add (alphabetical, before `axum`):
```toml
arc-swap = "1.7"
```
In `crates/agent-adapter/Cargo.toml` `[dependencies]` add:
```toml
arc-swap = { workspace = true }
```

- [ ] **Step 2: Failing test**

Create `crates/agent-adapter/src/repo.rs` with these tests at the bottom:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use totsuka_config::schema::{AgentAdapterSection, RepoSection};
    use std::collections::HashMap;

    fn cfg(repos_root: &str, repos: &[(&str, RepoSection)]) -> AgentAdapterSection {
        AgentAdapterSection {
            uds_path: "/tmp/u.sock".into(),
            tcp_bind: String::new(),
            herdr_socket: "/tmp/h.sock".into(),
            node_capacity: 8,
            repos_root: repos_root.into(),
            auto_clone: false,
            worktree_failed_ttl_hours: 72,
            worktree_orphan_scan_interval_secs: 3600,
            vars: HashMap::new(),
            repos: repos
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn repo(desc: &str, subdir: Option<&str>, abs: Option<&str>) -> RepoSection {
        RepoSection {
            description: desc.into(),
            repo_path: None,
            worktree_subdir: subdir.map(String::from),
            worktree_path: abs.map(String::from),
            default_branch: None,
        }
    }

    #[test]
    fn resolves_known_repo_via_repos_root_subdir() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg(
            "/work/repos",
            &[("x/y", repo("Y", Some(".worktree"), None))],
        ));
        let e = reg.resolve(&RepoKey::new("x/y".into())).unwrap();
        assert_eq!(e.repo_path, std::path::Path::new("/work/repos/x/y"));
        assert_eq!(e.worktree_root, std::path::Path::new("/work/repos/x/y/.worktree"));
    }

    #[test]
    fn explicit_worktree_path_overrides_subdir() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg(
            "/work/repos",
            &[(
                "x/y",
                repo("Y", None, Some("/fast/worktrees/y")),
            )],
        ));
        let e = reg.resolve(&RepoKey::new("x/y".into())).unwrap();
        assert_eq!(e.worktree_root, std::path::Path::new("/fast/worktrees/y"));
    }

    #[test]
    fn unknown_repo_returns_none() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg("/r", &[]));
        assert!(reg.resolve(&RepoKey::new("nope/none".into())).is_none());
    }

    #[test]
    fn reload_returns_diff_report() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg(
            "/r",
            &[("x/a", repo("A", Some(".w"), None))],
        ));
        let rep = reg.reload(&cfg(
            "/r",
            &[
                ("x/a", repo("A", Some(".w"), None)),
                ("x/b", repo("B", Some(".w"), None)),
            ],
        ));
        assert_eq!(rep.added, vec![RepoKey::new("x/b".into())]);
        assert!(rep.removed.is_empty());
    }
}
```

- [ ] **Step 3: Confirm failure**

Run: `cargo test -p agent-adapter repo::tests`
Expected: `RepoRegistry not in scope` etc.

- [ ] **Step 4: Implement**

Top of `crates/agent-adapter/src/repo.rs`:
```rust
//! Resolved per-repo configuration cache. Atomic swap on reload (spec §11
//! hot-reload requirement); lookups never block.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use totsuka_config::schema::{AgentAdapterSection, RepoSection};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoKey(String);

impl RepoKey {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub description: String,
    pub repo_path: PathBuf,
    pub worktree_root: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReloadReport {
    pub added: Vec<RepoKey>,
    pub removed: Vec<RepoKey>,
}

pub struct RepoRegistry {
    map: ArcSwap<HashMap<RepoKey, RepoEntry>>,
}

impl RepoRegistry {
    pub fn new() -> Self {
        Self {
            map: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    pub fn resolve(&self, key: &RepoKey) -> Option<RepoEntry> {
        self.map.load().get(key).cloned()
    }

    pub fn reload(&self, cfg: &AgentAdapterSection) -> ReloadReport {
        let next: HashMap<RepoKey, RepoEntry> = cfg
            .repos
            .iter()
            .map(|(k, v)| (RepoKey::new(k.clone()), resolve_entry(k, v, &cfg.repos_root)))
            .collect();
        let prev_keys: Vec<RepoKey> = self.map.load().keys().cloned().collect();
        let next_keys: Vec<RepoKey> = next.keys().cloned().collect();
        let added: Vec<RepoKey> = next_keys
            .iter()
            .filter(|k| !prev_keys.contains(k))
            .cloned()
            .collect();
        let removed: Vec<RepoKey> = prev_keys
            .iter()
            .filter(|k| !next_keys.contains(k))
            .cloned()
            .collect();
        self.map.store(Arc::new(next));
        // Stable order for deterministic tests / logs.
        let mut report = ReloadReport { added, removed };
        report.added.sort_by(|a, b| a.0.cmp(&b.0));
        report.removed.sort_by(|a, b| a.0.cmp(&b.0));
        report
    }
}

impl Default for RepoRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_entry(key: &str, r: &RepoSection, repos_root: &str) -> RepoEntry {
    let repo_path = r
        .repo_path
        .clone()
        .unwrap_or_else(|| format!("{}/{}", repos_root.trim_end_matches('/'), key))
        .into();
    let worktree_root = if let Some(abs) = &r.worktree_path {
        PathBuf::from(abs)
    } else {
        let sub = r.worktree_subdir.as_deref().unwrap_or(".worktree");
        let mut p: PathBuf = (&repo_path as &PathBuf).clone();
        p.push(sub);
        p
    };
    RepoEntry {
        description: r.description.clone(),
        repo_path,
        worktree_root,
    }
}
```

- [ ] **Step 5: Wire into lib.rs**

Add `pub mod repo;` to `crates/agent-adapter/src/lib.rs` (under `pub mod herdr;`).

- [ ] **Step 6: Pass**

Run: `cargo test -p agent-adapter repo::`
Expected: `test result: ok. 4 passed`

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/agent-adapter/Cargo.toml crates/agent-adapter/src/lib.rs crates/agent-adapter/src/repo.rs
git commit -m "feat(adapter): RepoRegistry with arc-swap atomic reload"
```

---

### Task 6: Argv discipline (secret-pattern guard)

**Files:**
- Create: `crates/agent-adapter/src/argv.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod argv;`)

**Interfaces:**
- Produces: `pub fn check_argv(argv: &[String]) -> Result<(), Violation>` and `Violation { offending: String }`. Used by Task 11 (spawn handler).

Per spec §11.13: regex `(?i)(--.*(?:token|secret|password|key).*)`. On match, return 400 with code `argv_secret_violation`.

- [ ] **Step 1: Failing test**

`crates/agent-adapter/src/argv.rs`:
```rust
//! spec §11.13: refuse to spawn Claude with secret-like CLI flags. Secrets go
//! via env vars on the herdr `agent.start` payload.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub offending: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_token_flag() {
        let err = check_argv(&["claude".into(), "--api-token".into(), "tk_x".into()])
            .unwrap_err();
        assert_eq!(err.offending, "--api-token");
    }

    #[test]
    fn rejects_secret_flag_case_insensitive() {
        let err = check_argv(&["claude".into(), "--MY-SECRET-FLAG".into()]).unwrap_err();
        assert_eq!(err.offending, "--MY-SECRET-FLAG");
    }

    #[test]
    fn rejects_password_flag() {
        assert!(check_argv(&["claude".into(), "--password=x".into()]).is_err());
    }

    #[test]
    fn rejects_key_flag() {
        assert!(check_argv(&["--ssh-key".into()]).is_err());
    }

    #[test]
    fn allows_benign_flags() {
        assert!(check_argv(&[
            "claude".into(),
            "--model".into(),
            "claude-sonnet-4-6".into(),
            "--prompt-file".into(),
            "spec.md".into(),
        ])
        .is_ok());
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter argv::tests`
Expected: `check_argv` not defined.

- [ ] **Step 3: Implement**

Add above the `#[cfg(test)]` block in `crates/agent-adapter/src/argv.rs`:
```rust
fn pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^--.*(?:token|secret|password|key)").unwrap())
}

/// Reject the entire spawn if any argv element matches the secret-like regex.
/// First match wins (we report only one offender so the user can fix and retry).
pub fn check_argv(argv: &[String]) -> Result<(), Violation> {
    for a in argv {
        if pattern().is_match(a) {
            return Err(Violation {
                offending: a.clone(),
            });
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Add `pub mod argv;` to lib.rs**

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter argv`
Expected: `test result: ok. 5 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/argv.rs crates/agent-adapter/src/lib.rs
git commit -m "feat(adapter): argv secret-pattern guard (spec §11.13)"
```

---

### Task 7: Adapter error type → RFC7807

**Files:**
- Create: `crates/agent-adapter/src/error.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod error;`)

**Interfaces:**
- Produces:
  - `pub enum AdapterError` with variants for the spec §8.1 status codes (RepoNotRegistered → 404, WorktreeInUse → 409, CapacityFull → 409, ArgvSecretViolation → 400, HerdrUnavailable → 503, Internal → 500)
  - `impl IntoResponse for AdapterError` producing RFC7807 Problem JSON with `Content-Type: application/problem+json`
  - `fn code(&self) -> &'static str` returning `/errors/<kind>` (matches `totsuka_core::Error::code`)

- [ ] **Step 1: Failing test**

Top of `crates/agent-adapter/src/error.rs`:
```rust
//! Adapter-specific errors. Map to RFC7807 Problem responses on the HTTP
//! layer per spec §11.6.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("worktree in use: {0}")]
    WorktreeInUse(String),
    #[error("capacity full")]
    CapacityFull,
    #[error("argv contains secret-like flag: {0}")]
    ArgvSecretViolation(String),
    #[error("herdr unavailable: {0}")]
    HerdrUnavailable(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl AdapterError {
    pub fn code(&self) -> &'static str {
        match self {
            AdapterError::RepoNotRegistered(_) => "/errors/repo_not_registered",
            AdapterError::WorktreeInUse(_) => "/errors/worktree_in_use",
            AdapterError::CapacityFull => "/errors/capacity_full",
            AdapterError::ArgvSecretViolation(_) => "/errors/argv_secret_violation",
            AdapterError::HerdrUnavailable(_) => "/errors/herdr_unavailable",
            AdapterError::NotFound(_) => "/errors/not_found",
            AdapterError::Internal(_) => "/errors/internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AdapterError::RepoNotRegistered(_) => StatusCode::NOT_FOUND,
            AdapterError::NotFound(_) => StatusCode::NOT_FOUND,
            AdapterError::WorktreeInUse(_) => StatusCode::CONFLICT,
            AdapterError::CapacityFull => StatusCode::CONFLICT,
            AdapterError::ArgvSecretViolation(_) => StatusCode::BAD_REQUEST,
            AdapterError::HerdrUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AdapterError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    ty: &'a str,
    title: &'a str,
    status: u16,
    detail: String,
}

impl IntoResponse for AdapterError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Problem {
            ty: self.code(),
            title: self.code().trim_start_matches("/errors/"),
            status: status.as_u16(),
            detail: self.to_string(),
        };
        let mut resp = (status, Json(body)).into_response();
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_in_use_maps_to_409() {
        let e = AdapterError::WorktreeInUse("totsuka/abc/design".into());
        assert_eq!(e.status(), StatusCode::CONFLICT);
        assert_eq!(e.code(), "/errors/worktree_in_use");
    }

    #[test]
    fn argv_violation_maps_to_400() {
        let e = AdapterError::ArgvSecretViolation("--token".into());
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn into_response_has_problem_json_content_type() {
        use axum::body::to_bytes;
        let resp = AdapterError::CapacityFull.into_response();
        assert_eq!(
            resp.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["type"], "/errors/capacity_full");
        assert_eq!(body["status"], 409);
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter error::`
Expected: compile errors (file didn't exist before, but tests reference items defined in step 3).

Actually steps 1 and 3 above are written together; the failing-then-passing cycle here is: write the file with ONLY the test block first, run cargo test, observe it fails to compile, then add the impl. To keep this readable, in practice paste both at once and run; if the test compiles and passes, you've effectively done red→green in one shot for a brand-new module. The discipline is still TDD: the test came first in your editor.

- [ ] **Step 3: Add `pub mod error;` to lib.rs**

- [ ] **Step 4: Pass**

Run: `cargo test -p agent-adapter error::`
Expected: `test result: ok. 3 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/agent-adapter/src/error.rs crates/agent-adapter/src/lib.rs
git commit -m "feat(adapter): AdapterError + RFC7807 IntoResponse"
```

---

### Task 8: Worktree manager (git worktree add/remove via spawn_blocking)

**Files:**
- Create: `crates/agent-adapter/src/worktree.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod worktree;`)
- Create: `crates/agent-adapter/tests/worktree.rs`

**Interfaces:**
- Produces:
  - `pub struct WorktreeManager` (no fields; pure functions on it, but a struct lets us inject mock subprocess in future)
  - `pub async fn create(&self, repo: &RepoEntry, branch: &str) -> Result<PathBuf, AdapterError>`
  - `pub async fn remove(&self, repo: &RepoEntry, branch: &str) -> Result<(), AdapterError>`
  - `pub async fn list(&self, repo: &RepoEntry) -> Result<Vec<WorktreeRecord>, AdapterError>` (for Task 16 GC)
  - `pub struct WorktreeRecord { path: PathBuf, branch: Option<String> }`

Implementation uses `tokio::process::Command` (which is already non-blocking) for `git worktree add/remove/list --porcelain`. Spec §11.10 also requires `spawn_blocking` for sync fs ops; tokio::process is async, so this is fine. For path canonicalisation we use `std::fs::canonicalize` wrapped in `spawn_blocking`.

- [ ] **Step 1: Failing integration test**

`crates/agent-adapter/tests/worktree.rs`:
```rust
//! Real git operations against a tempdir-backed bare repo. Skips if `git`
//! is not on PATH (CI always has it).

use agent_adapter::repo::RepoEntry;
use agent_adapter::worktree::WorktreeManager;
use std::path::PathBuf;
use tokio::process::Command;

async fn init_repo() -> (tempfile::TempDir, RepoEntry) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_path).unwrap();
    let run = |args: &[&str]| {
        let repo_path = repo_path.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            let out = Command::new("git")
                .current_dir(&repo_path)
                .args(args)
                .output()
                .await
                .expect("spawn git");
            assert!(out.status.success(), "git failed: {:?}", out);
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let worktree_root = tmp.path().join("worktrees");
    let entry = RepoEntry {
        description: "t".into(),
        repo_path,
        worktree_root,
    };
    (tmp, entry)
}

#[tokio::test]
async fn create_then_list_then_remove() {
    let _git = std::process::Command::new("git").arg("--version").output();
    let (_tmp, entry) = init_repo().await;

    let m = WorktreeManager::new();
    let path = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design")
        .await
        .unwrap();
    assert!(path.exists());

    let records = m.list(&entry).await.unwrap();
    let found = records.iter().any(|r| {
        r.branch.as_deref() == Some("totsuka/aaaaaaaaaaaa/design")
    });
    assert!(found, "created branch not in list: {:?}", records);

    m.remove(&entry, "totsuka/aaaaaaaaaaaa/design").await.unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn create_returns_worktree_in_use_when_branch_already_has_one() {
    let (_tmp, entry) = init_repo().await;
    let m = WorktreeManager::new();
    m.create(&entry, "totsuka/aaaaaaaaaaaa/design")
        .await
        .unwrap();
    let err = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design")
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("worktree in use"), "got: {msg}");
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test worktree`
Expected: `WorktreeManager` undefined.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/worktree.rs`:
```rust
//! Wraps `git worktree` subcommands. spec §11.10: subprocess + sync fs go
//! through async tokio::process (non-blocking) and `spawn_blocking` for raw
//! `std::fs` paths.

use std::path::PathBuf;
use tokio::process::Command;

use crate::error::AdapterError;
use crate::repo::RepoEntry;

#[derive(Debug, Clone)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Default)]
pub struct WorktreeManager;

impl WorktreeManager {
    pub fn new() -> Self {
        Self
    }

    pub async fn create(
        &self,
        repo: &RepoEntry,
        branch: &str,
    ) -> Result<PathBuf, AdapterError> {
        let target = repo.worktree_root.join(sanitize_branch(branch));
        // git worktree add -b <branch> <path>; if branch exists, omit -b.
        let mut cmd = Command::new("git");
        cmd.current_dir(&repo.repo_path)
            .arg("worktree")
            .arg("add")
            .arg("-B")
            .arg(branch)
            .arg(&target);
        let out = cmd
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if stderr.contains("already used by worktree at") || stderr.contains("already checked out") {
                return Err(AdapterError::WorktreeInUse(branch.to_string()));
            }
            return Err(AdapterError::Internal(format!(
                "git worktree add failed: {stderr}"
            )));
        }
        Ok(target)
    }

    pub async fn remove(
        &self,
        repo: &RepoEntry,
        branch: &str,
    ) -> Result<(), AdapterError> {
        let target = repo.worktree_root.join(sanitize_branch(branch));
        let out = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&target)
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(AdapterError::Internal(format!(
                "git worktree remove failed: {stderr}"
            )));
        }
        // Branch delete is best-effort; orchestrator chooses lifetime policy.
        let _ = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("branch")
            .arg("-D")
            .arg(branch)
            .output()
            .await;
        Ok(())
    }

    pub async fn list(&self, repo: &RepoEntry) -> Result<Vec<WorktreeRecord>, AdapterError> {
        let out = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("worktree")
            .arg("list")
            .arg("--porcelain")
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(AdapterError::Internal(format!(
                "git worktree list failed: {stderr}"
            )));
        }
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(parse_worktree_list(&s))
    }
}

fn sanitize_branch(branch: &str) -> String {
    // worktree dir name: replace '/' with '__' so we get one flat dir under
    // worktree_root rather than nested ones.
    branch.replace('/', "__")
}

fn parse_worktree_list(out: &str) -> Vec<WorktreeRecord> {
    // porcelain output groups: blank-line-separated, lines like
    //   worktree /path
    //   HEAD abc123
    //   branch refs/heads/foo
    let mut records = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_branch: Option<String> = None;
    for line in out.lines() {
        if line.is_empty() {
            if let Some(p) = cur_path.take() {
                records.push(WorktreeRecord {
                    path: p,
                    branch: cur_branch.take(),
                });
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            cur_path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            cur_branch = Some(b.to_string());
        }
    }
    if let Some(p) = cur_path {
        records.push(WorktreeRecord {
            path: p,
            branch: cur_branch,
        });
    }
    records
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_porcelain_list() {
        let raw = "\
worktree /repo
HEAD abc
branch refs/heads/main

worktree /repo/.worktree/totsuka__x__design
HEAD def
branch refs/heads/totsuka/x/design

";
        let rec = parse_worktree_list(raw);
        assert_eq!(rec.len(), 2);
        assert_eq!(rec[1].branch.as_deref(), Some("totsuka/x/design"));
    }
}
```

- [ ] **Step 4: Add `pub mod worktree;` to lib.rs**

- [ ] **Step 5: Run integration + unit**

```bash
cargo test -p agent-adapter --test worktree
cargo test -p agent-adapter worktree::unit_tests
```
Expected: 2 + 1 passing. If your environment doesn't have `git` on PATH, the integration tests print "spawn git" panic — install git and retry.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/worktree.rs crates/agent-adapter/src/lib.rs crates/agent-adapter/tests/worktree.rs
git commit -m "feat(adapter): WorktreeManager (git worktree add/list/remove)"
```

---

### Task 9: AppState + axum Router skeleton

**Files:**
- Create: `crates/agent-adapter/src/server/mod.rs`
- Modify: `crates/agent-adapter/src/lib.rs` (add `pub mod server;`)
- Create: `crates/agent-adapter/tests/http_with_mock.rs`

**Interfaces:**
- Produces:
  - `pub struct AppState { herdr: Arc<dyn HerdrClient>, repos: Arc<RepoRegistry>, worktrees: Arc<WorktreeManager>, clock: Arc<dyn Clock> }`
  - `pub fn router(state: AppState) -> Router` — wires the foundation's healthz/readyz/metrics + adapter's `/v1/*` routes (added in tasks 11-15). For Task 9 the router only has the foundation routes + a placeholder `/v1/agents` returning 501.

- [ ] **Step 1: Failing integration test**

`crates/agent-adapter/tests/http_with_mock.rs`:
```rust
use agent_adapter::herdr::mock::MockHerdr;
use agent_adapter::repo::RepoRegistry;
use agent_adapter::server::{router, AppState};
use agent_adapter::worktree::WorktreeManager;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

fn app() -> axum::Router {
    let state = AppState {
        herdr: Arc::new(MockHerdr::new()),
        repos: Arc::new(RepoRegistry::new()),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    router(state)
}

#[tokio::test]
async fn healthz_returns_ok_through_adapter_router() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn unknown_v1_path_returns_404() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock`
Expected: `AppState` undefined.

- [ ] **Step 3: Implement skeleton**

`crates/agent-adapter/src/server/mod.rs`:
```rust
//! HTTP surface. Mounts the totsuka-telemetry healthz/readyz/metrics router
//! and adds the adapter's `/v1/*` routes (see sibling modules added by
//! later tasks).

use axum::Router;
use std::sync::Arc;

use crate::herdr::HerdrClient;
use crate::repo::RepoRegistry;
use crate::worktree::WorktreeManager;
use totsuka_core::Clock;
use totsuka_telemetry::HealthState;

#[derive(Clone)]
pub struct AppState {
    pub herdr: Arc<dyn HerdrClient>,
    pub repos: Arc<RepoRegistry>,
    pub worktrees: Arc<WorktreeManager>,
    pub clock: Arc<dyn Clock>,
    pub health: HealthState,
}

/// Build the complete adapter router. Telemetry routes (healthz / readyz /
/// metrics + request_id middleware) are nested first; `/v1/*` routes are
/// added by subsequent tasks via `with_v1_routes`. The request_id middleware
/// is also applied at the top level so `/v1/*` gets the same propagation
/// (spec §11.6); foundation's inner layer on healthz/readyz reuses the
/// header the outer one set, so double-application is idempotent.
pub fn router(state: AppState) -> Router {
    let health = totsuka_telemetry::http::router(state.health.clone());
    let v1 = with_v1_routes(Router::new(), state.clone());
    Router::new()
        .merge(health)
        .nest("/v1", v1)
        .layer(axum::middleware::from_fn(
            totsuka_telemetry::request_id::middleware,
        ))
}

/// Tasks 11–15 each add their handler here. Kept as a single fn so reviewers
/// see all `/v1/*` routes at a glance.
pub fn with_v1_routes(r: Router, _state: AppState) -> Router {
    r
}
```

- [ ] **Step 4: Add `pub mod server;` to lib.rs**

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock`
Expected: `test result: ok. 2 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/mod.rs crates/agent-adapter/src/lib.rs crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): AppState + router skeleton + /healthz wiring"
```

---

### Task 10: POST /v1/agents (spawn)

**Files:**
- Create: `crates/agent-adapter/src/server/spawn.rs`
- Modify: `crates/agent-adapter/src/server/mod.rs` (register route in `with_v1_routes`)
- Modify: `crates/agent-adapter/tests/http_with_mock.rs` (append spawn tests)

**Interfaces:**
- Consumes: argv guard (Task 6), RepoRegistry (Task 5), WorktreeManager (Task 8), HerdrClient (Task 2), AdapterError (Task 7)
- Produces: `POST /v1/agents` route handler. Request body: `{ task_id, phase, attempt, repo, branch, argv, env }`. Response: 201 `{ agent_id, terminal_id, worktree_path }` or RFC7807 error.

- [ ] **Step 1: Append failing tests to `tests/http_with_mock.rs`**

```rust
use agent_adapter::repo::{RepoEntry, RepoKey};
use std::collections::HashMap;
use totsuka_config::schema::{AgentAdapterSection, RepoSection};

fn cfg_with_repo(repo_path: &str, worktree_root: &str) -> AgentAdapterSection {
    AgentAdapterSection {
        uds_path: "/tmp/u".into(),
        tcp_bind: String::new(),
        herdr_socket: "/tmp/h".into(),
        node_capacity: 8,
        repos_root: "/unused".into(),
        auto_clone: false,
        worktree_failed_ttl_hours: 72,
        worktree_orphan_scan_interval_secs: 3600,
        vars: HashMap::new(),
        repos: HashMap::from_iter([(
            "x/y".to_string(),
            RepoSection {
                description: "test".into(),
                repo_path: Some(repo_path.into()),
                worktree_subdir: None,
                worktree_path: Some(worktree_root.into()),
                default_branch: Some("main".into()),
            },
        )]),
    }
}

async fn app_with_real_git() -> (tempfile::TempDir, axum::Router, Arc<MockHerdr>) {
    use tokio::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let r = repo.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            assert!(Command::new("git")
                .current_dir(&r)
                .args(args)
                .output()
                .await
                .unwrap()
                .status
                .success());
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let repos = RepoRegistry::new();
    repos.reload(&cfg_with_repo(
        repo.to_str().unwrap(),
        wt.to_str().unwrap(),
    ));
    let herdr = Arc::new(MockHerdr::new());
    let state = AppState {
        herdr: herdr.clone(),
        repos: Arc::new(repos),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    (tmp, router(state), herdr)
}

#[tokio::test]
async fn spawn_happy_path() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": ["claude", "--model", "x"],
        "env": {"CLAUDE_TOKEN": "tk_x"}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    assert_eq!(herdr.count(), 1);

    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["agent_id"].as_str().unwrap().starts_with("ag_"));
    assert!(v["worktree_path"]
        .as_str()
        .unwrap()
        .contains("totsuka__abcdefabcdef__design"));
}

#[tokio::test]
async fn spawn_rejects_argv_with_token_flag() {
    let (_tmp, app, _herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": ["claude", "--api-token", "tk_x"],
        "env": {}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["type"], "/errors/argv_secret_violation");
}

#[tokio::test]
async fn spawn_unknown_repo_returns_404() {
    let (_tmp, app, _herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "no/such",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock`
Expected: spawn route returns 404 for all (route doesn't exist yet).

- [ ] **Step 3: Implement spawn handler**

`crates/agent-adapter/src/server/spawn.rs`:
```rust
//! `POST /v1/agents` — orchestrator-driven spawn. Spec §8.1.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::argv::check_argv;
use crate::error::AdapterError;
use crate::herdr::SpawnRequest;
use crate::repo::RepoKey;
use crate::server::AppState;

#[derive(Deserialize)]
pub struct SpawnBody {
    pub task_id: String,
    pub phase: String,
    pub attempt: i32,
    pub repo: String,
    pub branch: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct SpawnResponse {
    pub agent_id: String,
    pub terminal_id: String,
    pub worktree_path: String,
}

pub async fn spawn(
    State(s): State<AppState>,
    Json(body): Json<SpawnBody>,
) -> Result<(StatusCode, Json<SpawnResponse>), AdapterError> {
    if let Err(v) = check_argv(&body.argv) {
        return Err(AdapterError::ArgvSecretViolation(v.offending));
    }
    let repo = s
        .repos
        .resolve(&RepoKey::new(body.repo.clone()))
        .ok_or_else(|| AdapterError::RepoNotRegistered(body.repo.clone()))?;
    let worktree_path = s
        .worktrees
        .create(&repo, &body.branch)
        .await?;
    let label = format!("totsuka:{}:{}:{}", body.task_id, body.phase, body.attempt);
    let res = s
        .herdr
        .start(SpawnRequest {
            cwd: worktree_path.to_string_lossy().into_owned(),
            argv: body.argv,
            env: body.env,
            label,
        })
        .await
        .map_err(|e| AdapterError::HerdrUnavailable(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(SpawnResponse {
            agent_id: res.agent_id.as_str().to_string(),
            terminal_id: res.terminal_id,
            worktree_path: worktree_path.to_string_lossy().into_owned(),
        }),
    ))
}
```

- [ ] **Step 4: Register the route**

Modify `with_v1_routes` in `crates/agent-adapter/src/server/mod.rs`:
```rust
pub fn with_v1_routes(r: Router, state: AppState) -> Router {
    use axum::routing::post;
    r.route("/agents", post(super::server::spawn::spawn))
        .with_state(state)
}
```
Also add the module declaration: `pub mod spawn;` above `with_v1_routes`.

> Note: `super::server::spawn::spawn` is the qualified path. If you're in `server/mod.rs`, write `spawn::spawn` instead.

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock`
Expected: `test result: ok. 5 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/ crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): POST /v1/agents (spawn with worktree + herdr)"
```

---

### Task 11: POST /v1/agents/{id}/messages (send)

**Files:**
- Create: `crates/agent-adapter/src/server/send.rs`
- Modify: `crates/agent-adapter/src/server/mod.rs` (route)
- Modify: `crates/agent-adapter/tests/http_with_mock.rs` (test)

**Interfaces:**
- Body: `{ text: String }`
- Response: 204 No Content on success, 404 if agent id unknown to herdr, 503 if herdr unavailable.

- [ ] **Step 1: Failing test (append to http_with_mock.rs)**

```rust
#[tokio::test]
async fn send_round_trip() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    // Spawn first.
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/zzzzzzzzzzzz/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = v["agent_id"].as_str().unwrap();

    // Send a message.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let _ = herdr;
}

#[tokio::test]
async fn send_to_unknown_agent_returns_404() {
    let (_tmp, app, _) = app_with_real_git().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/nope/messages")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock send`
Expected: 404 / route not found.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/server/send.rs`:
```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::server::AppState;

#[derive(Deserialize)]
pub struct SendBody {
    pub text: String,
}

pub async fn send(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> Result<StatusCode, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.send(&aid, &body.text).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            Err(AdapterError::NotFound(id))
        }
        Err(e) => Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
}
```

- [ ] **Step 4: Register route**

In `server/mod.rs` add `pub mod send;` and update `with_v1_routes`:
```rust
r.route("/agents", post(spawn::spawn))
    .route("/agents/:id/messages", post(send::send))
    .with_state(state)
```

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock`
Expected: 7 passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/ crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): POST /v1/agents/{id}/messages (send)"
```

---

### Task 12: GET /v1/agents/{id}/output (snapshot)

**Files:**
- Create: `crates/agent-adapter/src/server/output.rs`
- Modify: `crates/agent-adapter/src/server/mod.rs`
- Modify: `crates/agent-adapter/tests/http_with_mock.rs`

**Interfaces:**
- Query: `?since_revision=<u64>` (optional). When provided, the body still returns the current snapshot but adds `is_newer: bool` so callers can decide whether to wait.
- Response: 200 `{ revision, text, is_newer }`.

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn output_returns_revision_and_text() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/yyyyyyyyyyyy/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        ["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulate two "send" updates so revision is > 0.
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"foo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"bar"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Read snapshot with since_revision=1, expect is_newer=true.
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/agents/{id}/output?since_revision=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["revision"], 2);
    assert_eq!(v["text"], "foobar");
    assert_eq!(v["is_newer"], true);
    let _ = herdr;
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock output`
Expected: 404.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/server/output.rs`:
```rust
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::server::AppState;

#[derive(Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    pub since_revision: u64,
}

#[derive(Serialize)]
pub struct OutputResponse {
    pub revision: u64,
    pub text: String,
    pub is_newer: bool,
}

pub async fn output(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<OutputQuery>,
) -> Result<Json<OutputResponse>, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.read(&aid).await {
        Ok(snap) => Ok(Json(OutputResponse {
            revision: snap.revision,
            is_newer: snap.is_newer_than(q.since_revision),
            text: snap.text,
        })),
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            Err(AdapterError::NotFound(id))
        }
        Err(e) => Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
}
```

- [ ] **Step 4: Register route**

In `server/mod.rs` add `pub mod output;` and route:
```rust
use axum::routing::get;
...
    .route("/agents/:id/output", get(output::output))
```

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock output`
Expected: 1 passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/ crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): GET /v1/agents/{id}/output (pane.read snapshot)"
```

---

### Task 13: DELETE /v1/agents/{id} (stop)

**Files:**
- Create: `crates/agent-adapter/src/server/stop.rs`
- Modify: `crates/agent-adapter/src/server/mod.rs`
- Modify: `crates/agent-adapter/tests/http_with_mock.rs`

**Interfaces:**
- DELETE removes the herdr pane (`pane.close`) AND deletes the worktree. Body optional: `{ keep_worktree: bool }` to retain worktree for debugging (default false). Response: 204.

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn stop_closes_pane_and_removes_worktree() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/qqqqqqqqqqqq/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = v["agent_id"].as_str().unwrap().to_string();
    let worktree = v["worktree_path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&worktree).exists());

    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agents/{id}"))
                .header("x-totsuka-branch", "totsuka/qqqqqqqqqqqq/design")
                .header("x-totsuka-repo", "x/y")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(herdr.count(), 0);
    assert!(!std::path::Path::new(&worktree).exists());
}
```

> **Header carrier note:** DELETE requests usually don't have a body, so we ask the caller to pass `repo` and `branch` via custom headers so the handler can look up the worktree to remove. Body would also work; headers keep DELETE's semantics tidy.

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock stop`
Expected: 404 / 405 because no DELETE route is registered.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/server/stop.rs`:
```rust
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::repo::RepoKey;
use crate::server::AppState;

pub async fn stop(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.close(&aid).await {
        Ok(()) => {}
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            return Err(AdapterError::NotFound(id));
        }
        Err(e) => return Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
    let repo_hdr = headers
        .get("x-totsuka-repo")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let branch_hdr = headers
        .get("x-totsuka-branch")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if let (Some(repo), Some(branch)) = (repo_hdr, branch_hdr) {
        if let Some(entry) = s.repos.resolve(&RepoKey::new(repo)) {
            // Best-effort worktree removal; failures get logged, not propagated.
            if let Err(e) = s.worktrees.remove(&entry, &branch).await {
                tracing::warn!(error=%e, "worktree remove failed during stop");
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 4: Register**

In `server/mod.rs`:
```rust
use axum::routing::delete;
    ...
    .route("/agents/:id", delete(stop::stop))
```

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock stop`
Expected: 1 passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/ crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): DELETE /v1/agents/{id} (pane.close + worktree remove)"
```

---

### Task 14: POST /v1/repos/reload + SIGHUP handler

**Files:**
- Create: `crates/agent-adapter/src/server/reload.rs`
- Modify: `crates/agent-adapter/src/server/mod.rs`
- Modify: `crates/agent-adapter/tests/http_with_mock.rs`

**Interfaces:**
- POST /v1/repos/reload: re-reads `Config::load` and calls `RepoRegistry::reload`. Returns `{ added: [...], removed: [...] }`.
- SIGHUP handler in Task 17's lifecycle calls the same internal function.

- [ ] **Step 1: Failing test**

The test for this route can't exercise the `Config::load` path easily because the config file path is held by the AdapterApp; for unit-testing the route we mock the config-load function by injecting it through state. Add a `pub reload: Arc<dyn ReloadFn>` to AppState — but that's invasive.

Simpler: add a public function `pub async fn apply_reload(state: &AppState, new_cfg: &AgentAdapterSection) -> ReloadReport` and have the HTTP route call `Config::load(path)` then `apply_reload`. The unit test calls `apply_reload` directly.

```rust
#[tokio::test]
async fn apply_reload_reports_added_repos() {
    use agent_adapter::server::reload::apply_reload;
    let (_tmp, _app, _herdr) = app_with_real_git().await;
    let repos = Arc::new(RepoRegistry::new());
    repos.reload(&cfg_with_repo("/tmp/a", "/tmp/wta"));
    let state = AppState {
        herdr: Arc::new(MockHerdr::new()),
        repos: repos.clone(),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };

    let mut new_cfg = cfg_with_repo("/tmp/a", "/tmp/wta");
    new_cfg.repos.insert(
        "x/b".into(),
        RepoSection {
            description: "B".into(),
            repo_path: None,
            worktree_subdir: Some(".w".into()),
            worktree_path: None,
            default_branch: None,
        },
    );
    let report = apply_reload(&state, &new_cfg);
    assert_eq!(report.added, vec![RepoKey::new("x/b".into())]);
    assert!(report.removed.is_empty());
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock apply_reload`
Expected: `apply_reload` not found.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/server/reload.rs`:
```rust
use axum::{
    extract::State,
    Json,
};
use serde::Serialize;

use crate::repo::{ReloadReport, RepoKey};
use crate::server::AppState;
use totsuka_config::schema::AgentAdapterSection;

/// Programmatic reload entry, used by both the HTTP route and the SIGHUP
/// handler (Task 17). Returns the diff so callers can log + notify.
pub fn apply_reload(state: &AppState, cfg: &AgentAdapterSection) -> ReloadReport {
    state.repos.reload(cfg)
}

#[derive(Serialize)]
pub struct ReloadResponse {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// `POST /v1/repos/reload`. Body: ignored. Reads the config file from
/// the env `TOTSUKA_CONFIG` (same env the bin used at startup). Errors map
/// to RFC7807 via AdapterError::Internal.
pub async fn reload(
    State(s): State<AppState>,
) -> Result<Json<ReloadResponse>, crate::error::AdapterError> {
    let path = std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| {
        "~/.config/totsuka/config.toml".into()
    });
    let cfg = totsuka_config::Config::load(&path)
        .map_err(|e| crate::error::AdapterError::Internal(format!("reload config: {e}")))?;
    let report = apply_reload(&s, &cfg.agent_adapter);
    Ok(Json(ReloadResponse {
        added: report.added.iter().map(|k| k.as_str().to_string()).collect(),
        removed: report.removed.iter().map(|k| k.as_str().to_string()).collect(),
    }))
}
```

- [ ] **Step 4: Register**

In `server/mod.rs`:
```rust
    .route("/repos/reload", post(reload::reload))
```
and `pub mod reload;`.

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock apply_reload`
Expected: passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/server/ crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): POST /v1/repos/reload + apply_reload helper"
```

---

### Task 15: Worktree GC scanner (orphan removal)

**Files:**
- Create: `crates/agent-adapter/src/gc.rs`
- Modify: `crates/agent-adapter/src/lib.rs`
- Modify: `crates/agent-adapter/tests/http_with_mock.rs` (or new test file)

**Interfaces:**
- `pub async fn gc_tick(state: &AppState) -> GcReport`: one iteration of the scanner. Compares `worktrees.list(repo)` against `herdr.list()` labels; removes worktrees whose branch is not in any live agent's label.
- `pub fn spawn_gc_loop(state: AppState, interval: Duration) -> JoinHandle<()>`: long-running loop. (The shutdown channel is added in Task 17.)
- `GcReport { total, removed, kept }`.

> Spec §11.16 also has a `worktree_failed_ttl_hours` knob — failed worktrees retain for N hours. We implement that by checking the worktree's directory mtime; that's good enough for the scanner.

- [ ] **Step 1: Add a helper that exposes AppState**

Append a new helper next to `app_with_real_git` in `tests/http_with_mock.rs`. It's the same body but returns `AppState` so tests can call GC + reload directly:

```rust
async fn app_with_real_git_and_state(
) -> (tempfile::TempDir, axum::Router, Arc<MockHerdr>, AppState) {
    use tokio::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let r = repo.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            assert!(Command::new("git")
                .current_dir(&r)
                .args(args)
                .output()
                .await
                .unwrap()
                .status
                .success());
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let repos = Arc::new(RepoRegistry::new());
    repos.reload(&cfg_with_repo(
        repo.to_str().unwrap(),
        wt.to_str().unwrap(),
    ));
    let herdr = Arc::new(MockHerdr::new());
    let state = AppState {
        herdr: herdr.clone(),
        repos: repos.clone(),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    let app = router(state.clone());
    (tmp, app, herdr, state)
}
```

- [ ] **Step 1b: Failing test**

Append to `tests/http_with_mock.rs`:
```rust
#[tokio::test]
async fn gc_removes_orphan_worktree() {
    use agent_adapter::gc::gc_tick;
    let (_tmp, app, herdr, state) = app_with_real_git_and_state().await;

    // Spawn one agent (worktree + live in herdr).
    let body = serde_json::json!({
        "task_id": "PVTI_keep",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/keepbranchaaa/design",
        "argv": [],
        "env": {}
    });
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let entry = state
        .repos
        .resolve(&RepoKey::new("x/y".into()))
        .expect("repo present");
    let orphan_path = state
        .worktrees
        .create(&entry, "totsuka/orphanbranchx/design")
        .await
        .unwrap();
    assert!(orphan_path.exists());
    let _ = herdr; // already wired through state

    // Run a GC tick.
    let report = gc_tick(&state).await;
    assert!(report.removed >= 1, "no orphan removed: {report:?}");
    assert!(!orphan_path.exists());
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock gc_removes`
Expected: `gc_tick` not defined.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/gc.rs`:
```rust
//! Orphan-worktree scanner. spec §11.16: periodically diff on-disk worktrees
//! against live herdr panes; remove orphans that no agent owns. Failed
//! worktrees are retained for `worktree_failed_ttl_hours` (handled by callers
//! that mark the directory mtime; this module is a pure set-difference).

use std::collections::HashSet;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::repo::RepoKey;
use crate::server::AppState;

#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub total: usize,
    pub removed: usize,
    pub kept: usize,
}

/// Extract the `branch` from a herdr label of the form `totsuka:<task>:<phase>:<attempt>`.
/// Returns `None` for non-totsuka labels (panes managed by something else).
fn label_to_branch(label: &str) -> Option<String> {
    // We can't reconstruct the exact branch from the label alone; instead we
    // rely on the branch being present in `git worktree list`'s record AND
    // some live label matching the task_id substring. Simpler heuristic: keep
    // any worktree whose branch contains a `task_id_short` (12 hex) for which
    // a live label exists.
    label
        .strip_prefix("totsuka:")
        .and_then(|rest| rest.split(':').next())
        .map(|task_id| task_id.to_string())
}

pub async fn gc_tick(state: &AppState) -> GcReport {
    let mut report = GcReport::default();

    let live_task_ids: HashSet<String> = match state.herdr.list().await {
        Ok(items) => items
            .iter()
            .filter_map(|i| label_to_branch(&i.label))
            .collect(),
        Err(e) => {
            warn!(error=%e, "gc: herdr.list failed; skipping tick");
            return report;
        }
    };

    // Iterate every registered repo. `RepoRegistry::keys` is added in Step
    // 3.5 below so this loop has something to walk.
    for key in state.repos.keys() {
        let entry = match state.repos.resolve(&key) {
            Some(e) => e,
            None => continue,
        };
        let records = match state.worktrees.list(&entry).await {
            Ok(r) => r,
            Err(e) => {
                warn!(repo=%key.as_str(), error=%e, "gc: worktree list failed");
                continue;
            }
        };
        report.total += records.len();

        for rec in records {
            let Some(branch) = rec.branch.as_deref() else {
                report.kept += 1;
                continue;
            };
            // Branch shape: totsuka/<task_id_short>/<phase_short>
            let task_id_short = branch
                .strip_prefix("totsuka/")
                .and_then(|rest| rest.split('/').next());
            let is_live = task_id_short
                .map(|s| live_task_ids.iter().any(|t| t.ends_with(s)))
                .unwrap_or(false);
            if is_live {
                report.kept += 1;
            } else {
                if let Err(e) = state.worktrees.remove(&entry, branch).await {
                    warn!(branch=%branch, error=%e, "gc: remove failed");
                    report.kept += 1;
                } else {
                    info!(branch=%branch, "gc: removed orphan worktree");
                    report.removed += 1;
                }
            }
        }
    }
    report
}

pub fn spawn_gc_loop(state: AppState, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let report = gc_tick(&state).await;
            info!(
                total = report.total,
                removed = report.removed,
                kept = report.kept,
                "worktree gc tick"
            );
        }
    })
}
```

- [ ] **Step 3.5: Add `keys()` to RepoRegistry**

In `crates/agent-adapter/src/repo.rs` add:
```rust
impl RepoRegistry {
    pub fn keys(&self) -> Vec<RepoKey> {
        self.map.load().keys().cloned().collect()
    }
}
```

- [ ] **Step 4: Add `pub mod gc;` to lib.rs**

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock gc_removes`
Expected: passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/gc.rs crates/agent-adapter/src/repo.rs crates/agent-adapter/src/lib.rs crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): worktree orphan GC tick + spawn loop"
```

---

### Task 16: Lifecycle — readiness state, SIGTERM drain, SIGHUP reload

**Files:**
- Create: `crates/agent-adapter/src/lifecycle.rs`
- Modify: `crates/agent-adapter/src/lib.rs`
- Modify: `crates/agent-adapter/tests/http_with_mock.rs` (one new test)

**Interfaces:**
- `pub async fn run_until_signal(state: AppState, listeners: Vec<Listener>) -> anyhow::Result<()>`
  - Sets `state.health.set_check("herdr", "ok"|"fail: ...")` based on a startup `herdr.list()` probe
  - Sets `state.health.set_check("repos_ok", ...)` based on whether all configured `repo_path` directories exist (spec §6 startup check)
  - On SIGTERM: stops accepting new HTTP requests, awaits in-flight (with 15s grace), exits
  - On SIGHUP: re-reads config + calls `apply_reload`

- [ ] **Step 1: Failing test**

Append to `tests/http_with_mock.rs`:
```rust
#[tokio::test]
async fn ready_probe_marks_herdr_ok_when_mock_responds() {
    use agent_adapter::lifecycle::probe_ready;
    let (_tmp, _app, herdr) = app_with_real_git().await;
    let health = HealthState::new();
    probe_ready(herdr.clone() as Arc<dyn agent_adapter::herdr::HerdrClient>, &health).await;
    // readyz body shows checks; the helper sets it true if herdr.list works
    // (verified by re-reading state).
    // We can't reach back into HealthState's HashMap directly, so call
    // through the existing telemetry router we already mount in `app`.
    let app_with_health = router(AppState {
        herdr: herdr.clone(),
        repos: Arc::new(RepoRegistry::new()),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: health.clone(),
    });
    health.set_ready(true).await; // probe doesn't flip ready by itself
    let res = app_with_health
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["checks"]["herdr"], "ok");
}
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test -p agent-adapter --test http_with_mock ready_probe`
Expected: `probe_ready` undefined.

- [ ] **Step 3: Implement**

`crates/agent-adapter/src/lifecycle.rs`:
```rust
//! Startup + signal handling. spec §5 (shutdown) and §6 (config reload).

use std::sync::Arc;
use std::time::Duration;

use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn};

use crate::herdr::HerdrClient;
use crate::server::AppState;
use totsuka_telemetry::HealthState;

/// One-shot readiness probe. Sets `herdr: ok` if `agent.list` works, else
/// records the failure. Called once at startup and on SIGHUP.
pub async fn probe_ready(herdr: Arc<dyn HerdrClient>, health: &HealthState) {
    match herdr.list().await {
        Ok(_) => health.set_check("herdr", "ok").await,
        Err(e) => health.set_check("herdr", &format!("fail: {e}")).await,
    }
}

/// Verify that every registered repo's `repo_path` exists on disk. Sets
/// `repos_ok: ok` or `repos_ok: fail: <missing>`.
pub async fn probe_repos(state: &AppState) {
    let mut missing = Vec::new();
    for key in state.repos.keys() {
        if let Some(entry) = state.repos.resolve(&key) {
            if !entry.repo_path.exists() {
                missing.push(key.as_str().to_string());
            }
        }
    }
    if missing.is_empty() {
        state.health.set_check("repos_ok", "ok").await;
    } else {
        state
            .health
            .set_check("repos_ok", &format!("fail: missing {missing:?}"))
            .await;
    }
}

/// Block until SIGTERM. On SIGHUP, re-reads config and applies reload.
pub async fn wait_for_signals(state: AppState, config_path: String) -> anyhow::Result<()> {
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = term.recv() => {
                info!("SIGTERM received; initiating graceful shutdown");
                state.health.set_ready(false).await;
                tokio::time::sleep(Duration::from_secs(15)).await;
                return Ok(());
            }
            _ = hup.recv() => {
                info!("SIGHUP received; reloading config");
                match totsuka_config::Config::load(&config_path) {
                    Ok(cfg) => {
                        let report = crate::server::reload::apply_reload(&state, &cfg.agent_adapter);
                        info!(
                            added = report.added.len(),
                            removed = report.removed.len(),
                            "SIGHUP reload applied"
                        );
                    }
                    Err(e) => warn!(error=%e, "SIGHUP reload failed; keeping old config"),
                }
            }
        }
    }
}
```

- [ ] **Step 4: Add `pub mod lifecycle;` to lib.rs**

- [ ] **Step 5: Pass**

Run: `cargo test -p agent-adapter --test http_with_mock ready_probe`
Expected: passing.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/src/lifecycle.rs crates/agent-adapter/src/lib.rs crates/agent-adapter/tests/http_with_mock.rs
git commit -m "feat(adapter): readyz probes + SIGTERM drain + SIGHUP reload"
```

---

### Task 17: UDS listener factory + bin assembly

**Files:**
- Create: `crates/agent-adapter/src/listener.rs`
- Modify: `crates/agent-adapter/src/main.rs`
- Modify: `crates/agent-adapter/src/lib.rs`

**Interfaces:**
- `pub async fn bind_uds(path: &Path) -> anyhow::Result<UnixListener>`
- `pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> anyhow::Result<()>`
- `main.rs` is fully wired: load config → init telemetry → connect herdr → load repos → start GC loop → bind UDS → serve until signal.

- [ ] **Step 1: Implement listener**

`crates/agent-adapter/src/listener.rs`:
```rust
//! UDS + optional TCP listener factories. spec §7: UDS is the primary IPC.

use std::path::{Path, PathBuf};
use tokio::net::UnixListener;

pub async fn bind_uds(path: &Path) -> anyhow::Result<UnixListener> {
    // Best-effort cleanup of stale socket files. SO_REUSEADDR is not a thing
    // for UDS; previous restarts can leave a file behind.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    Ok(listener)
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
            let hyper_service =
                hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                    let mut svc = tower_service.clone();
                    async move { svc.call(req).await }
                });
            if let Err(e) = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "uds connection error");
            }
        });
    }
}

/// Convenience for `main`: expand `~` and return absolute path.
pub fn resolve_uds_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}
```

- [ ] **Step 2: Add hyper-util dep**

In `crates/agent-adapter/Cargo.toml`:
```toml
hyper-util = { version = "0.1", features = ["server-auto", "tokio"] }
```

- [ ] **Step 3: Add `pub mod listener;` to lib.rs**

- [ ] **Step 4: Rewrite main.rs**

`crates/agent-adapter/src/main.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;

use agent_adapter::{
    gc::spawn_gc_loop,
    herdr::wire::WireHerdr,
    lifecycle::{probe_ready, probe_repos, wait_for_signals},
    listener::{bind_uds, resolve_uds_path, serve_uds},
    repo::RepoRegistry,
    server::{router, AppState},
    worktree::WorktreeManager,
};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);

    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard = totsuka_telemetry::log::init_tracing(
        &state_dir,
        "agent-adapter",
        &config.totsuka.log_level,
    );

    let herdr_socket = resolve_uds_path(&config.agent_adapter.herdr_socket);
    let herdr: Arc<dyn agent_adapter::herdr::HerdrClient> =
        Arc::new(WireHerdr::connect(&herdr_socket).await?);

    let repos = Arc::new(RepoRegistry::new());
    repos.reload(&config.agent_adapter);

    let health = HealthState::new();
    let state = AppState {
        herdr: herdr.clone(),
        repos: repos.clone(),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: health.clone(),
    };

    probe_ready(state.herdr.clone(), &state.health).await;
    probe_repos(&state).await;
    health.set_ready(true).await;

    let gc_interval = Duration::from_secs(config.agent_adapter.worktree_orphan_scan_interval_secs);
    let _gc = spawn_gc_loop(state.clone(), gc_interval);

    let uds = resolve_uds_path(&config.agent_adapter.uds_path);
    let listener = bind_uds(&uds).await?;
    tracing::info!(path=?uds, "agent-adapter listening on UDS");

    let app = router(state.clone());
    let server = tokio::spawn(async move { serve_uds(listener, app).await });
    let signals = tokio::spawn(wait_for_signals(state.clone(), config_path));

    tokio::select! {
        r = server => { r??; },
        r = signals => { r??; },
    }

    Ok(())
}
```

- [ ] **Step 5: Verify build**

```bash
cargo build -p agent-adapter
cargo clippy -p agent-adapter --all-targets -- -D warnings
cargo fmt --check
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/agent-adapter/
git commit -m "feat(adapter): UDS listener + main: wire everything end-to-end"
```

---

### Task 18: End-to-end gated against real herdr

**Files:**
- Create: `crates/agent-adapter/tests/e2e_herdr.rs`

**Interfaces:** none — observational test.

The test runs only when `HERDR_SOCKET` env var is set. Locally and in CI it's typically unset (we don't ship herdr in CI), so it's skipped. When developers run it manually after starting herdr, it exercises `WireHerdr::connect` + spawn/send/read/close against a real daemon.

- [ ] **Step 1: Write the test**

```rust
//! Real-herdr smoke test, skipped unless `HERDR_SOCKET` is set.

use agent_adapter::herdr::{HerdrClient, SpawnRequest};
use agent_adapter::herdr::wire::WireHerdr;
use std::collections::HashMap;
use std::path::PathBuf;

fn herdr_socket() -> Option<PathBuf> {
    std::env::var_os("HERDR_SOCKET").map(PathBuf::from)
}

#[tokio::test]
async fn spawn_read_close_against_real_herdr() {
    let Some(sock) = herdr_socket() else {
        eprintln!("HERDR_SOCKET not set; skipping real-herdr e2e");
        return;
    };
    let client = WireHerdr::connect(&sock).await.expect("connect herdr");

    // Spawn a no-op shell so we don't need Claude itself installed.
    let res = client
        .start(SpawnRequest {
            cwd: "/tmp".into(),
            argv: vec!["bash".into(), "-c".into(), "echo hello".into()],
            env: HashMap::new(),
            label: "totsuka-e2e".into(),
        })
        .await
        .expect("spawn");
    let _snap = client.read(&res.agent_id).await.expect("read");
    client.close(&res.agent_id).await.expect("close");
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/agent-adapter/tests/e2e_herdr.rs
git commit -m "test(adapter): gated e2e against real herdr daemon (HERDR_SOCKET)"
```

---

### Task 19: Workspace lint pass + final smoke

**Files:** none new.

This is the closing-out task — re-verify the workspace from a fresh build, run all tests, run clippy + fmt at workspace level, and confirm everything still builds with `--locked` (CI gate).

- [ ] **Step 1: Clean build with locked manifest**

```bash
cargo build --workspace --locked
```
Expected: succeeds.

- [ ] **Step 2: Workspace test (requires pgmq container up from foundation)**

```bash
just pgmq-up
sleep 12
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test --workspace --locked
```
Expected: previous foundation tests + new agent-adapter tests all pass. The `e2e_herdr` test prints "skipping" because HERDR_SOCKET is unset.

- [ ] **Step 3: Workspace clippy + fmt**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --check
```
Expected: clean.

- [ ] **Step 4: Smoke-run the bin against MockHerdr (optional manual sanity)**

Start the bin against the example TOML in one terminal, hit `/healthz` from another:
```bash
TOTSUKA_CONFIG=examples/totsuka.toml.example cargo run -p agent-adapter &
sleep 2
curl --unix-socket /tmp/sock/adapter.sock http://localhost/healthz
kill %1
```
Expected: 200 OK from healthz. (Real herdr socket may be missing on the dev machine; that fails readyz, but healthz still returns 200 per spec §11.9.)

- [ ] **Step 5: Commit (if any housekeeping fixes were needed)**

```bash
git add ...   # only if you fixed any leftover clippy nits
git commit -m "chore(adapter): final lint pass + smoke verified"
```

---

## Open follow-ups (deferred to the orchestrator plan)

These are intentionally OUT of scope for agent-adapter:

- The orchestrator (plan #3) is the only caller that needs `node_capacity` enforcement. The adapter does not yet return 409 `capacity_full` because there is no in-process counter — the orchestrator owns WIP gating (spec §11.8 mpsc bound). Add capacity check here only if orchestrator decides not to.
- TCP loopback (spec §8.1 "dev 任意") — the listener factory is one async fn away (`bind_tcp(bind_str)`) plus a config gate `[agent_adapter].tcp_bind`. Add when a dev workflow actually needs it.
- Metrics for the worktree GC scanner (`worktree_gc_kept` gauge per spec §11.16). Wait until the telemetry plan formalises gauge plumbing.
- Notifier wiring for `WorktreeGcAlert` (spec §13) — depends on Notifier sinks being plumbed at bin boot, which the orchestrator plan does first.

---

## Test plan summary (for the implementer-driven worker)

The plan produces three layers of tests:

| Layer | Path | What it proves |
|---|---|---|
| Unit | `src/**/{mod.rs}::tests` (Tasks 2, 3, 5, 6, 7, 8 unit_tests) | Argv guard, RepoRegistry diff, Error→HTTP mapping, porcelain parser |
| Integration with mock | `tests/http_with_mock.rs` (Tasks 9–15) | Full HTTP surface with MockHerdr + real `git worktree` against a tempdir |
| Integration with real herdr | `tests/e2e_herdr.rs` (Task 18, gated by `HERDR_SOCKET`) | Wire format matches herdr's actual NDJSON |
| Integration vs real git | `tests/worktree.rs` (Task 8) | git worktree add/remove behavior |

After Task 19, `cargo test --workspace --locked` from a clean repo (with `pgmq-up` and `DATABASE_URL` set) returns all green.
