# qa-service Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Slack Socket Mode listener that classifies questions by repo via an LLM, drives a Claude agent through agent-adapter to answer, posts the answer back to Slack, and creates GitHub Project Inbox items from reactions — with thread continuity, restart recovery, and provider-agnostic LLM dispatch.

**Architecture:** Crate `crates/qa-service/` (bin + lib). Layered: `slack/` (Socket Mode WS + Web API) + `adapter_client/` (HTTP-over-UDS to agent-adapter, mirror of orchestrator's) + `gh_inbox/` (GraphQL `addProjectV2DraftIssue`) + `classifier/` (provider abstraction: anthropic + openai_compat dispatched per `[qa_service.classifier].provider`) → `repo_select` (threshold + on_low_confidence policy) + `mode` (auto vs delegated) + `reaction` (Slack reaction → Inbox) → `answer` (spawn-or-reuse + poll-extract + post) + `catchup` (Slack history穴埋め) + `sweeper` (idle pane close) + `recovery` (reconcile thread mapping ↔ adapter agent list). The bin wires lifecycle + UDS healthz/readyz. All time via `Arc<dyn Clock>`; all secrets via `Secret<String>`.

**Tech Stack:** Rust stable / tokio / sqlx (postgres + chrono) / axum (healthz over UDS) / reqwest (rustls, Slack Web + GitHub) / tokio-tungstenite (Slack Socket Mode WS) / hyperlocal + hyper-util (adapter UDS) / serde + serde_json / anyhow (bin) / thiserror (lib) / async-trait

## Global Constraints

(spec §11 verbatim, plus §8.4)

- Rust toolchain: **stable**, `[profile.release] panic = "abort"`, `tokio::task::block_in_place` clippy-denied at workspace level
- Schema versioning (spec §11.1): `const MIN_SCHEMA_VERSION: i32 = 6; const TARGET_SCHEMA_VERSION: i32 = 6;`. Mismatch → `SchemaOutOfRange` + exit 1
- Time (spec §11.5): all `DateTime` via `Arc<dyn Clock>`; `Utc::now()` direct call is clippy-denied. Storage UTC, display Asia/Tokyo
- Errors (spec §11.6): lib `thiserror`, bin `anyhow`; HTTP errors → RFC7807 `/errors/<kind>`
- Secrets (spec §11.7): `Secret<String>` for `slack_app_token`, `slack_bot_token`, `classifier.api_key`. `.expose()` only at outbound HTTPS / WS connect call sites
- Bounded channels (spec §11.8): Slack event → handler queue bounded at **128**, full → **drop oldest** (slack-side buffer also exists, log `channel_full_total{channel="slack_inbound"}`)
- Concurrency cap: `qa_service.answer.max_concurrent_answers` (default 4) — `tokio::sync::Semaphore` gates the answer pipeline
- Determinism for catchup: `event_key_slack(event_id)` → `slack:event:{event_id}` (already in `totsuka_core::key`)
- Provider abstraction (spec §8.4): two impls (`anthropic.rs` + `openai_compat.rs`) cover all 5 provider strings (`anthropic` / `openai` / `openrouter` / `litellm` / `openai_compatible`)
- Structured output: Anthropic via tool_use force; OpenAI-compat via `response_format = {type: "json_schema", ...}`
- Repo description must be non-empty for every `[agent_adapter.repos.*]` (readyz NG otherwise)
- Sentinel-based answer detection: `<<TOTSUKA_DONE>>` (configurable); also `stable_revision_secs` quiescence + `answer_timeout_secs` cap (truncate + warn)
- agent-adapter is the only path to herdr; qa-service never talks to herdr directly
- IPC matrix (spec §7): qa-service exposes healthz/readyz on **UDS** (`${state_dir}/sock/qa-service.sock`); supervisor probes via UDS
- 1Password commit-signing policy: every commit MUST use `git -c commit.gpgsign=false -c tag.gpgsign=false commit ...`
- Pre-flight: foundation (PR #1) + agent-adapter (PR #2) + orchestrator (PR #3/#4) + github-watcher (PR #5) all merged into main

---

## File Structure

```
crates/qa-service/
├── Cargo.toml                          [Create] bin + lib
└── src/
    ├── main.rs                         [Create] anyhow entry; wire everything
    ├── lib.rs                          [Create] QaApp + module re-exports
    ├── error.rs                        [Create] QaError + code()
    ├── schema_check.rs                 [Create] MIN/TARGET_SCHEMA_VERSION + check_schema_version
    ├── adapter_client/
    │   ├── mod.rs                      [Create] AdapterClient trait + SpawnReq/SendReq/ReadReq/StopReq
    │   ├── uds.rs                      [Create] HyperlocalAdapter (mirrors orchestrator's UDS client)
    │   └── mock.rs                     [Create] MockAdapter
    ├── thread_map.rs                   [Create] qa_thread_agent CRUD
    ├── classifier/
    │   ├── mod.rs                      [Create] Classifier trait + dispatch factory
    │   ├── schema.rs                   [Create] RepoCandidate, ClassifyRequest, ClassifyResponse
    │   ├── prompt.rs                   [Create] prompt template (question + thread + repos)
    │   ├── retry.rs                    [Create] exp backoff for 429/5xx/parse failure
    │   ├── anthropic.rs                [Create] Anthropic Messages API + tool_use force
    │   ├── openai_compat.rs            [Create] OpenAI-style + json_schema response_format
    │   └── mock.rs                     [Create] MockClassifier
    ├── repo_select.rs                  [Create] threshold + on_low_confidence policy
    ├── mode.rs                         [Create] AnswerMode enum (auto | delegated)
    ├── slack/
    │   ├── mod.rs                      [Create] module re-exports
    │   ├── web.rs                      [Create] Slack web API client (postMessage / postEphemeral / history / replies)
    │   ├── socket.rs                   [Create] Socket Mode WebSocket client + envelope ACK loop
    │   ├── envelope.rs                 [Create] envelope parser (events.message / events.reaction_added / hello / disconnect)
    │   └── mock.rs                     [Create] MockSlackClient
    ├── question_filter.rs              [Create] allowed_user_ids + mention/thread filter
    ├── answer/
    │   ├── mod.rs                      [Create] re-exports + AnswerResult enum
    │   ├── pipeline.rs                 [Create] spawn-or-reuse + poll + extract + post
    │   └── extract.rs                  [Create] sentinel + tag fallback + truncate
    ├── reaction.rs                     [Create] reaction_added → Inbox path
    ├── gh_inbox.rs                     [Create] GraphQL addProjectV2DraftIssue (variables-based)
    ├── catchup.rs                      [Create] startup history sweep
    ├── sweeper.rs                      [Create] idle pane close loop
    ├── recovery.rs                     [Create] reconcile qa_thread_agent vs adapter agent.list
    ├── lifecycle.rs                    [Create] readyz probes + signals
    └── listener.rs                     [Create] UDS healthz/readyz listener
crates/qa-service/tests/
├── schema_check.rs                     [Create] DB handshake (skip on no DATABASE_URL)
├── thread_map.rs                       [Create] qa_thread_agent CRUD against real DB
├── classifier_anthropic.rs             [Create] wire-body assertion via TCP stub (tool_use forced)
├── classifier_openai_compat.rs         [Create] wire-body assertion via TCP stub (response_format)
├── classifier_dispatch.rs              [Create] provider string → impl factory selection
├── repo_select.rs                      [Create] threshold + 3 fallback policies
├── slack_envelope.rs                   [Create] envelope parse for events.message / reaction_added / hello / disconnect
├── slack_web.rs                        [Create] postMessage / postEphemeral against TCP stub
├── question_filter.rs                  [Create] allowed_user_ids + mention/thread cases
├── answer_extract.rs                   [Create] sentinel / tag-only / fallback / truncate
├── gh_inbox.rs                         [Create] GraphQL injection regression (variables-based)
├── e2e_high_conf_answer.rs             [Create] MockClassifier high_conf → MockAdapter spawn → MockSlack post
├── e2e_thread_continuation.rs          [Create] existing mapping → send not spawn
└── e2e_recovery.rs                     [Create] reconcile orphans on startup
```

Workspace edits: add `"crates/qa-service"` to members.

---

## Tasks

### Task 1: Crate scaffold + bin/lib split

**Files:**
- Create: `crates/qa-service/Cargo.toml`
- Create: `crates/qa-service/src/main.rs`
- Create: `crates/qa-service/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: foundation crates
- Produces: `qa_service::QaApp::new(config, clock) -> Self` + `async fn run(self) -> anyhow::Result<()>` (stub)

- [ ] **Step 1: Add to workspace**

Append `"crates/qa-service"` to `Cargo.toml [workspace] members` (alphabetical between orchestrator and totsuka-bus).

- [ ] **Step 2: Crate Cargo.toml**

`crates/qa-service/Cargo.toml`:
```toml
[package]
name = "qa-service"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[[bin]]
name = "qa-service"
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
regex       = { workspace = true }
tracing-subscriber = { workspace = true }
tokio-util  = { version = "0.7", features = ["rt"] }
hyper-util  = { version = "0.1", features = ["client", "client-legacy", "tokio", "server-auto"] }
http-body-util = "0.1"
tokio-tungstenite = { version = "0.24", default-features = false, features = ["rustls-tls-native-roots", "connect"] }
url         = "2.5"

[dev-dependencies]
tokio    = { workspace = true, features = ["test-util"] }
tempfile = "3.12"
uuid     = { workspace = true }
```

- [ ] **Step 3: lib.rs stub**

`crates/qa-service/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub struct QaApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl QaApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("qa-service stub: nothing to do yet");
        Ok(())
    }
}
```

- [ ] **Step 4: main.rs stub**

`crates/qa-service/src/main.rs`:
```rust
use std::sync::Arc;

use qa_service::QaApp;
use totsuka_config::Config;
use totsuka_core::SystemClock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(Config::load(&config_path)?);
    tracing_subscriber::fmt().with_env_filter("info").init();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    QaApp::new(config, clock).run().await
}
```

- [ ] **Step 5: Verify**

```bash
cargo check --workspace
cargo build -p qa-service
```
Expected: both succeed.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): bin/lib scaffold + workspace wire-up"
```

---

### Task 2: QaError + RFC7807 mapping

**Files:**
- Create: `crates/qa-service/src/error.rs`
- Modify: `crates/qa-service/src/lib.rs` (`pub mod error;`)

**Interfaces:**
- Produces: `pub enum QaError` with variants `Sqlx(#[from] sqlx::Error)`, `Bus(#[from] totsuka_bus::pgmq::BusError)`, `Http(#[from] reqwest::Error)`, `WebSocket(String)`, `Adapter(String)`, `Classifier(String)`, `Slack(String)`, `GraphQl(String)`, `SchemaOutOfRange { got, min, target }`, `RepoNotRegistered(String)`, `Internal(String)`, plus `code() -> &'static str` returning `/errors/<kind>`

- [ ] **Step 1: Implement + tests**

`crates/qa-service/src/error.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QaError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(String),
    #[error("adapter: {0}")]
    Adapter(String),
    #[error("classifier: {0}")]
    Classifier(String),
    #[error("slack: {0}")]
    Slack(String),
    #[error("graphql: {0}")]
    GraphQl(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl QaError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Http(_) => "/errors/http",
            Self::WebSocket(_) => "/errors/websocket",
            Self::Adapter(_) => "/errors/adapter",
            Self::Classifier(_) => "/errors/classifier",
            Self::Slack(_) => "/errors/slack",
            Self::GraphQl(_) => "/errors/graphql",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::RepoNotRegistered(_) => "/errors/repo_not_registered",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn schema_oor_codes() {
        assert_eq!(QaError::SchemaOutOfRange { got: 3, min: 6, target: 6 }.code(), "/errors/schema_out_of_range");
    }
    #[test] fn websocket_codes() {
        assert_eq!(QaError::WebSocket("drop".into()).code(), "/errors/websocket");
    }
    #[test] fn classifier_codes() {
        assert_eq!(QaError::Classifier("provider 500".into()).code(), "/errors/classifier");
    }
}
```

- [ ] **Step 2: Wire + run**

Add `pub mod error;` to `crates/qa-service/src/lib.rs`.

```bash
cargo test -p qa-service error::
```
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/qa-service/src/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): QaError + RFC7807 code()"
```

---

### Task 3: Schema-version handshake

**Files:**
- Create: `crates/qa-service/src/schema_check.rs`
- Create: `crates/qa-service/tests/schema_check.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces: `pub const MIN_SCHEMA_VERSION: i32 = 6; pub const TARGET_SCHEMA_VERSION: i32 = 6; pub async fn check_schema_version(pool: &PgPool) -> Result<i32, QaError>`

- [ ] **Step 1: Implement**

`crates/qa-service/src/schema_check.rs`:
```rust
//! spec §11.1 bin↔DB handshake. qa-service reads max(schema_meta.version)
//! and validates against the bin's compiled range. Mirrors orchestrator /
//! github-watcher implementations.

use crate::error::QaError;
use sqlx::PgPool;

pub const MIN_SCHEMA_VERSION: i32 = 6;
pub const TARGET_SCHEMA_VERSION: i32 = 6;

pub async fn check_schema_version(pool: &PgPool) -> Result<i32, QaError> {
    let row: (Option<i32>,) = sqlx::query_as("SELECT max(version) FROM schema_meta")
        .fetch_one(pool)
        .await?;
    let got = row.0.ok_or_else(|| {
        QaError::Internal("schema_meta is empty; run sqlx migrate".into())
    })?;
    if got < MIN_SCHEMA_VERSION || got > TARGET_SCHEMA_VERSION {
        return Err(QaError::SchemaOutOfRange {
            got,
            min: MIN_SCHEMA_VERSION,
            target: TARGET_SCHEMA_VERSION,
        });
    }
    Ok(got)
}
```

`crates/qa-service/tests/schema_check.rs`:
```rust
use qa_service::schema_check::{check_schema_version, TARGET_SCHEMA_VERSION};
use sqlx::postgres::PgPoolOptions;

fn db_url() -> Option<String> { std::env::var("DATABASE_URL").ok() }

#[tokio::test]
async fn returns_target_version_against_migrated_db() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    assert_eq!(check_schema_version(&pool).await.unwrap(), TARGET_SCHEMA_VERSION);
}
```

Add `pub mod schema_check;` to `lib.rs`.

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service --test schema_check
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): schema_meta version handshake (MIN/TARGET = 6)"
```

---

### Task 4: AdapterClient — HTTP-over-UDS to agent-adapter

**Files:**
- Create: `crates/qa-service/src/adapter_client/mod.rs`
- Create: `crates/qa-service/src/adapter_client/uds.rs`
- Create: `crates/qa-service/src/adapter_client/mock.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct SpawnReq { pub task_id: String, pub phase: String, pub attempt: i32, pub repo: String, pub branch: String, pub argv: Vec<String>, pub env: HashMap<String, Secret<String>> }` — hand-written Debug elides env
  - `pub struct SpawnRes { pub agent_id: String, pub terminal_id: String, pub worktree_path: String }`
  - `pub struct ReadRes { pub revision: u64, pub text: String, pub is_newer: bool }`
  - `pub struct AgentSummary { pub agent_id: String, pub terminal_id: String, pub label: String }` (returned by `list`)
  - `#[async_trait] pub trait AdapterClient: Send + Sync { async fn spawn(&self, SpawnReq) -> Result<SpawnRes, QaError>; async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError>; async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, QaError>; async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), QaError>; async fn list(&self) -> Result<Vec<AgentSummary>, QaError> }`
  - `pub struct HyperlocalAdapter { socket: PathBuf, client: ... }` with `new(PathBuf) -> Self`
  - `pub struct MockAdapter` with `Arc<Mutex<MockState>>` and setters: `set_spawn_response(SpawnRes)`, `set_read_response(ReadRes)`, `set_list_response(Vec<AgentSummary>)`, `expected_sends()`, `expected_stops()`

- [ ] **Step 1: mod.rs — trait + types**

`crates/qa-service/src/adapter_client/mod.rs`:
```rust
//! HTTP-over-UDS client to agent-adapter. Mirrors crates/orchestrator/src/
//! adapter_client/* but adds `list()` for restart recovery (spec §8.4
//! 再起動時のリカバリ — reconcile qa_thread_agent vs adapter's agent.list).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use totsuka_core::Secret;

use crate::error::QaError;

pub mod mock;
pub mod uds;
pub use mock::MockAdapter;
pub use uds::HyperlocalAdapter;

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

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub terminal_id: String,
    pub label: String,
}

#[async_trait]
pub trait AdapterClient: Send + Sync + 'static {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, QaError>;
    async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError>;
    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, QaError>;
    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), QaError>;
    async fn list(&self) -> Result<Vec<AgentSummary>, QaError>;
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

- [ ] **Step 2: uds.rs — HyperlocalAdapter**

`crates/qa-service/src/adapter_client/uds.rs`:
```rust
use async_trait::async_trait;
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyperlocal::UnixConnector;
use std::collections::HashMap;
use std::path::PathBuf;

use super::{AdapterClient, AgentSummary, ReadRes, SpawnReq, SpawnRes, WireSpawn};
use crate::error::QaError;

pub struct HyperlocalAdapter {
    socket: PathBuf,
    client: hyper_util::client::legacy::Client<UnixConnector, http_body_util::Full<Bytes>>,
}

impl HyperlocalAdapter {
    pub fn new(socket: PathBuf) -> Self {
        let client = hyper_util::client::legacy::Client::builder(
            hyper_util::rt::TokioExecutor::new(),
        )
        .build::<_, http_body_util::Full<Bytes>>(UnixConnector);
        Self { socket, client }
    }

    async fn call_json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, QaError> {
        let uri: hyper::Uri = hyperlocal::Uri::new(&self.socket, path).into();
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(Bytes::from(body.to_string())))
            .map_err(|e| QaError::Adapter(format!("build req: {e}")))?;
        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| QaError::Adapter(format!("send: {e}")))?;
        let status = resp.status();
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .map_err(|e| QaError::Adapter(format!("read: {e}")))?
            .to_bytes();
        if !status.is_success() {
            return Err(QaError::Adapter(format!(
                "{} {}: {}",
                status.as_u16(),
                path,
                String::from_utf8_lossy(&body)
            )));
        }
        if body.is_empty() {
            return serde_json::from_str("null").map_err(|e| QaError::Adapter(e.to_string()));
        }
        serde_json::from_slice(&body).map_err(|e| QaError::Adapter(e.to_string()))
    }
}

#[async_trait]
impl AdapterClient for HyperlocalAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, QaError> {
        let env: HashMap<&str, &str> = req
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose().as_str()))
            .collect();
        let wire = WireSpawn {
            task_id: &req.task_id,
            phase: &req.phase,
            attempt: req.attempt,
            repo: &req.repo,
            branch: &req.branch,
            argv: &req.argv,
            env,
        };
        let v = serde_json::to_value(&wire).map_err(|e| QaError::Adapter(e.to_string()))?;
        self.call_json(Method::POST, "/v1/agents", v).await
    }
    async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError> {
        let body = serde_json::json!({ "text": text });
        let _: serde_json::Value = self
            .call_json(Method::POST, &format!("/v1/agents/{agent_id}/messages"), body)
            .await?;
        Ok(())
    }
    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, QaError> {
        self.call_json(
            Method::GET,
            &format!("/v1/agents/{agent_id}/output?since_revision={since_revision}"),
            serde_json::Value::Null,
        )
        .await
    }
    async fn stop(&self, agent_id: &str, _repo: &str, _branch: &str) -> Result<(), QaError> {
        let _: serde_json::Value = self
            .call_json(Method::DELETE, &format!("/v1/agents/{agent_id}"), serde_json::Value::Null)
            .await?;
        Ok(())
    }
    async fn list(&self) -> Result<Vec<AgentSummary>, QaError> {
        self.call_json(Method::GET, "/v1/agents", serde_json::Value::Null).await
    }
}
```

- [ ] **Step 3: mock.rs — MockAdapter**

`crates/qa-service/src/adapter_client/mock.rs`:
```rust
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct MockState {
    spawn_response: Option<SpawnRes>,
    read_response: Option<ReadRes>,
    list_response: Vec<AgentSummary>,
    sends: Vec<(String, String)>,
    stops: Vec<(String, String, String)>,
    spawns: Vec<SpawnReq>,
}

pub struct MockAdapter {
    state: Mutex<MockState>,
}

impl Default for MockAdapter {
    fn default() -> Self { Self::new() }
}

impl MockAdapter {
    pub fn new() -> Self { Self { state: Mutex::new(MockState::default()) } }
    pub fn set_spawn_response(&self, r: SpawnRes) {
        self.state.lock().unwrap().spawn_response = Some(r);
    }
    pub fn set_read_response(&self, r: ReadRes) {
        self.state.lock().unwrap().read_response = Some(r);
    }
    pub fn set_list_response(&self, r: Vec<AgentSummary>) {
        self.state.lock().unwrap().list_response = r;
    }
    pub fn expected_sends(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().sends.clone()
    }
    pub fn expected_stops(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().stops.clone()
    }
    pub fn expected_spawns(&self) -> Vec<SpawnReq> {
        self.state.lock().unwrap().spawns.clone()
    }
}

#[async_trait]
impl AdapterClient for MockAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, QaError> {
        let mut s = self.state.lock().unwrap();
        s.spawns.push(req);
        s.spawn_response
            .clone()
            .ok_or_else(|| QaError::Adapter("MockAdapter has no spawn_response set".into()))
    }
    async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError> {
        self.state.lock().unwrap().sends.push((agent_id.into(), text.into()));
        Ok(())
    }
    async fn read(&self, _agent_id: &str, _since: u64) -> Result<ReadRes, QaError> {
        self.state
            .lock()
            .unwrap()
            .read_response
            .clone()
            .ok_or_else(|| QaError::Adapter("MockAdapter has no read_response set".into()))
    }
    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), QaError> {
        self.state.lock().unwrap().stops.push((agent_id.into(), repo.into(), branch.into()));
        Ok(())
    }
    async fn list(&self) -> Result<Vec<AgentSummary>, QaError> {
        Ok(self.state.lock().unwrap().list_response.clone())
    }
}
```

Add `pub mod adapter_client;` to `crates/qa-service/src/lib.rs`.

- [ ] **Step 4: Build + commit**

```bash
cargo build -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): AdapterClient (HTTP-over-UDS) + Mock"
```

---

### Task 5: ThreadMap repository (qa_thread_agent CRUD)

**Files:**
- Create: `crates/qa-service/src/thread_map.rs`
- Create: `crates/qa-service/tests/thread_map.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct ThreadMapping { pub thread_ts: String, pub terminal_id: String, pub repo: String, pub last_activity_at: DateTime<Utc>, pub created_at: DateTime<Utc> }`
  - `pub struct ThreadMapRepo { pool: PgPool, clock: Arc<dyn Clock> }`
  - `pub async fn get(thread_ts: &str) -> Result<Option<ThreadMapping>, QaError>`
  - `pub async fn upsert(m: &ThreadMapping) -> Result<(), QaError>`
  - `pub async fn touch(thread_ts: &str) -> Result<(), QaError>` — sets `last_activity_at = clock.now()`
  - `pub async fn list_idle(idle_threshold: DateTime<Utc>) -> Result<Vec<ThreadMapping>, QaError>`
  - `pub async fn delete(thread_ts: &str) -> Result<(), QaError>`
  - `pub async fn list_all() -> Result<Vec<ThreadMapping>, QaError>` (for restart recovery)

- [ ] **Step 1: Implement**

`crates/qa-service/src/thread_map.rs`:
```rust
//! spec §8.4 — qa_thread_agent table: maps Slack thread_ts → herdr terminal_id.

use crate::error::QaError;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use totsuka_core::Clock;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMapping {
    pub thread_ts: String,
    pub terminal_id: String,
    pub repo: String,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

pub struct ThreadMapRepo {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl ThreadMapRepo {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self { Self { pool, clock } }

    pub async fn get(&self, thread_ts: &str) -> Result<Option<ThreadMapping>, QaError> {
        let row = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, last_activity_at, created_at
               FROM qa_thread_agent WHERE thread_ts = $1",
        )
        .bind(thread_ts)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ThreadMapping {
            thread_ts: r.get("thread_ts"),
            terminal_id: r.get("terminal_id"),
            repo: r.get("repo"),
            last_activity_at: r.get("last_activity_at"),
            created_at: r.get("created_at"),
        }))
    }

    pub async fn upsert(&self, m: &ThreadMapping) -> Result<(), QaError> {
        sqlx::query(
            "INSERT INTO qa_thread_agent (thread_ts, terminal_id, repo, last_activity_at)
                  VALUES ($1, $2, $3, $4)
                  ON CONFLICT (thread_ts) DO UPDATE
                    SET terminal_id      = EXCLUDED.terminal_id,
                        repo             = EXCLUDED.repo,
                        last_activity_at = EXCLUDED.last_activity_at",
        )
        .bind(&m.thread_ts)
        .bind(&m.terminal_id)
        .bind(&m.repo)
        .bind(m.last_activity_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch(&self, thread_ts: &str) -> Result<(), QaError> {
        let now = self.clock.now();
        sqlx::query(
            "UPDATE qa_thread_agent SET last_activity_at = $2 WHERE thread_ts = $1",
        )
        .bind(thread_ts)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_idle(&self, idle_before: DateTime<Utc>) -> Result<Vec<ThreadMapping>, QaError> {
        let rows = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, last_activity_at, created_at
               FROM qa_thread_agent
              WHERE last_activity_at < $1",
        )
        .bind(idle_before)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| ThreadMapping {
            thread_ts: r.get("thread_ts"),
            terminal_id: r.get("terminal_id"),
            repo: r.get("repo"),
            last_activity_at: r.get("last_activity_at"),
            created_at: r.get("created_at"),
        }).collect())
    }

    pub async fn delete(&self, thread_ts: &str) -> Result<(), QaError> {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(thread_ts)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_all(&self) -> Result<Vec<ThreadMapping>, QaError> {
        let rows = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, last_activity_at, created_at FROM qa_thread_agent",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| ThreadMapping {
            thread_ts: r.get("thread_ts"),
            terminal_id: r.get("terminal_id"),
            repo: r.get("repo"),
            last_activity_at: r.get("last_activity_at"),
            created_at: r.get("created_at"),
        }).collect())
    }
}
```

Add `pub mod thread_map;` to `lib.rs`.

- [ ] **Step 2: Integration tests**

`crates/qa-service/tests/thread_map.rs`:
```rust
use chrono::{Duration, Utc};
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::SystemClock;

fn db_url() -> Option<String> { std::env::var("DATABASE_URL").ok() }
fn ts() -> String { format!("t_{}", uuid::Uuid::new_v4().simple()) }

#[tokio::test]
async fn upsert_get_round_trip() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let repo = ThreadMapRepo::new(pool, Arc::new(SystemClock));
    let tts = ts();
    let m = ThreadMapping {
        thread_ts: tts.clone(),
        terminal_id: "term_1".into(),
        repo: "acme/r".into(),
        last_activity_at: Utc::now(),
        created_at: Utc::now(),
    };
    repo.upsert(&m).await.unwrap();
    let got = repo.get(&tts).await.unwrap().unwrap();
    assert_eq!(got.terminal_id, "term_1");
    assert_eq!(got.repo, "acme/r");
    repo.delete(&tts).await.unwrap();
}

#[tokio::test]
async fn touch_advances_last_activity() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let repo = ThreadMapRepo::new(pool, Arc::new(SystemClock));
    let tts = ts();
    let initial = Utc::now() - Duration::hours(1);
    repo.upsert(&ThreadMapping {
        thread_ts: tts.clone(),
        terminal_id: "term_2".into(),
        repo: "acme/r".into(),
        last_activity_at: initial,
        created_at: initial,
    }).await.unwrap();
    repo.touch(&tts).await.unwrap();
    let got = repo.get(&tts).await.unwrap().unwrap();
    assert!(got.last_activity_at > initial);
    repo.delete(&tts).await.unwrap();
}

#[tokio::test]
async fn list_idle_filters_by_threshold() {
    let Some(url) = db_url() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let repo = ThreadMapRepo::new(pool, Arc::new(SystemClock));
    let old_ts = ts();
    let new_ts = ts();
    let now = Utc::now();
    repo.upsert(&ThreadMapping {
        thread_ts: old_ts.clone(),
        terminal_id: "term_old".into(),
        repo: "acme/r".into(),
        last_activity_at: now - Duration::hours(2),
        created_at: now - Duration::hours(2),
    }).await.unwrap();
    repo.upsert(&ThreadMapping {
        thread_ts: new_ts.clone(),
        terminal_id: "term_new".into(),
        repo: "acme/r".into(),
        last_activity_at: now,
        created_at: now,
    }).await.unwrap();
    let idle = repo.list_idle(now - Duration::hours(1)).await.unwrap();
    let ids: Vec<&str> = idle.iter().map(|m| m.terminal_id.as_str()).collect();
    assert!(ids.contains(&"term_old"));
    assert!(!ids.contains(&"term_new"));
    repo.delete(&old_ts).await.unwrap();
    repo.delete(&new_ts).await.unwrap();
}
```

- [ ] **Step 3: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service --test thread_map
```
Expected: 3 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): ThreadMap repository (qa_thread_agent CRUD)"
```

---

### Task 6: Classifier types + prompt builder + retry helper

**Files:**
- Create: `crates/qa-service/src/classifier/mod.rs`
- Create: `crates/qa-service/src/classifier/schema.rs`
- Create: `crates/qa-service/src/classifier/prompt.rs`
- Create: `crates/qa-service/src/classifier/retry.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct RepoCandidate { pub repo: String, pub description: String }` — input (totsuka.toml `[agent_adapter.repos.*]`)
  - `pub struct ClassifyRequest { pub question: String, pub thread_context: Option<String>, pub candidates: Vec<RepoCandidate> }`
  - `pub struct RepoVerdict { pub repo: String, pub confidence: f64, pub rationale: String }` — one row of the LLM's response
  - `pub struct ClassifyResponse { pub top_candidates: Vec<RepoVerdict>, pub provider: String, pub model: String, pub latency_ms: u64 }`
  - `#[async_trait] pub trait Classifier: Send + Sync + 'static { async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError>; fn provider(&self) -> &str; fn model(&self) -> &str }`
  - `pub fn build_prompt(req: &ClassifyRequest, top_n: u32) -> (String /* system */, String /* user */)`
  - `pub async fn with_classify_retry<F, Fut, T>(max_attempts: u32, mut op: F) -> Result<T, QaError>` — retries `QaError::Classifier` and HTTP 429/5xx; exp backoff 1s/4s/8s capped 30s

- [ ] **Step 1: schema.rs**

`crates/qa-service/src/classifier/schema.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoCandidate {
    pub repo: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyRequest {
    pub question: String,
    pub thread_context: Option<String>,
    pub candidates: Vec<RepoCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoVerdict {
    pub repo: String,
    pub confidence: f64,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyResponse {
    pub top_candidates: Vec<RepoVerdict>,
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
}
```

- [ ] **Step 2: prompt.rs**

`crates/qa-service/src/classifier/prompt.rs`:
```rust
//! Prompt template shared by every Classifier impl. Output schema documented
//! in the system message so off-the-rails responses are still parseable.

use super::schema::ClassifyRequest;

pub fn build_prompt(req: &ClassifyRequest, top_n: u32) -> (String, String) {
    let system = format!(
        "You classify a user question to one of the candidate repositories. \
         Return the top {top_n} most-likely repos as JSON: \
         {{\"top_candidates\": [{{\"repo\": \"owner/name\", \"confidence\": 0.0..1.0, \"rationale\": \"...\"}}]}}. \
         Sort by confidence descending. Only choose repos from the candidate list.");

    let mut user = String::new();
    if let Some(ctx) = &req.thread_context {
        user.push_str("Thread context:\n");
        user.push_str(ctx);
        user.push_str("\n\n");
    }
    user.push_str("Question:\n");
    user.push_str(&req.question);
    user.push_str("\n\nCandidate repositories:\n");
    for c in &req.candidates {
        user.push_str(&format!("- {}: {}\n", c.repo, c.description));
    }
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::schema::RepoCandidate;

    #[test]
    fn prompt_contains_top_n_and_all_repos() {
        let req = ClassifyRequest {
            question: "Where is the auth flow?".into(),
            thread_context: Some("Earlier: tried to log in".into()),
            candidates: vec![
                RepoCandidate { repo: "acme/web".into(), description: "frontend".into() },
                RepoCandidate { repo: "acme/api".into(), description: "auth backend".into() },
            ],
        };
        let (sys, user) = build_prompt(&req, 3);
        assert!(sys.contains("top 3"));
        assert!(user.contains("Earlier: tried to log in"));
        assert!(user.contains("Where is the auth flow?"));
        assert!(user.contains("acme/web"));
        assert!(user.contains("acme/api"));
        assert!(user.contains("auth backend"));
    }

    #[test]
    fn prompt_omits_thread_context_block_when_none() {
        let req = ClassifyRequest {
            question: "q".into(),
            thread_context: None,
            candidates: vec![RepoCandidate { repo: "a/b".into(), description: "x".into() }],
        };
        let (_sys, user) = build_prompt(&req, 1);
        assert!(!user.contains("Thread context:"));
    }
}
```

- [ ] **Step 3: retry.rs**

`crates/qa-service/src/classifier/retry.rs`:
```rust
use crate::error::QaError;
use std::future::Future;
use std::time::Duration;

pub async fn with_classify_retry<F, Fut, T>(max_attempts: u32, mut op: F) -> Result<T, QaError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, QaError>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < max_attempts && is_retryable(&e) => {
                let backoff = backoff_secs(attempt);
                tracing::warn!(error=%e, attempt, "classifier retrying in {backoff}s");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_retryable(e: &QaError) -> bool {
    matches!(e, QaError::Http(_))
        || matches!(e, QaError::Classifier(s) if s.contains("429") || s.contains("5") )
}

fn backoff_secs(attempt: u32) -> u64 {
    let s: u64 = 4u64.saturating_pow(attempt.saturating_sub(1));
    s.min(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_after_one_retryable_error() {
        let calls = AtomicU32::new(0);
        let r: u32 = with_classify_retry(3, || async {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 { Err(QaError::Classifier("500 internal".into())) } else { Ok(42) }
        }).await.unwrap();
        assert_eq!(r, 42);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let r: Result<u32, _> = with_classify_retry(2, || async {
            Err::<u32, _>(QaError::Classifier("500 internal".into()))
        }).await;
        assert!(matches!(r, Err(QaError::Classifier(_))));
    }
}
```

- [ ] **Step 4: mod.rs — trait + factory placeholder**

`crates/qa-service/src/classifier/mod.rs`:
```rust
//! LLM repo classifier. See spec §8.4 — 2 impls (anthropic + openai_compat)
//! cover 4 mandatory providers (anthropic / openai / openrouter / litellm)
//! plus 1 catch-all (openai_compatible). Factory dispatch by provider string.

use async_trait::async_trait;

pub mod mock;
pub mod prompt;
pub mod retry;
pub mod schema;

pub use mock::MockClassifier;
pub use prompt::build_prompt;
pub use retry::with_classify_retry;
pub use schema::{ClassifyRequest, ClassifyResponse, RepoCandidate, RepoVerdict};

use crate::error::QaError;

#[async_trait]
pub trait Classifier: Send + Sync + 'static {
    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError>;
    fn provider(&self) -> &str;
    fn model(&self) -> &str;
}
```

(`anthropic.rs`, `openai_compat.rs`, and the `build` factory are added in Tasks 7-9.)

- [ ] **Step 5: MockClassifier**

`crates/qa-service/src/classifier/mock.rs`:
```rust
use super::*;
use std::sync::Mutex;

pub struct MockClassifier {
    response: Mutex<ClassifyResponse>,
}

impl MockClassifier {
    pub fn new(response: ClassifyResponse) -> Self { Self { response: Mutex::new(response) } }
    pub fn set_response(&self, r: ClassifyResponse) { *self.response.lock().unwrap() = r; }
}

#[async_trait]
impl Classifier for MockClassifier {
    async fn classify(&self, _req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        Ok(self.response.lock().unwrap().clone())
    }
    fn provider(&self) -> &str { "mock" }
    fn model(&self) -> &str { "mock-model" }
}
```

Add `pub mod classifier;` to `lib.rs`.

- [ ] **Step 6: Run + commit**

```bash
cargo test -p qa-service classifier::
```
Expected: 4 passed (2 prompt + 2 retry).

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): classifier types + prompt + retry + Mock"
```

---

### Task 7: Anthropic classifier (Messages API + tool_use force)

**Files:**
- Create: `crates/qa-service/src/classifier/anthropic.rs`
- Create: `crates/qa-service/tests/classifier_anthropic.rs`
- Modify: `crates/qa-service/src/classifier/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct AnthropicClassifier { client, endpoint, api_key (Secret), model, max_tokens, top_n, request_timeout }`
  - `pub fn new(api_key, model, max_tokens, top_n, request_timeout, override_endpoint: Option<String>) -> Self` (defaults to `https://api.anthropic.com/v1/messages`)
  - Implements `Classifier` — POSTs `{"model": ..., "max_tokens": ..., "system": ..., "messages": [{"role": "user", "content": ...}], "tools": [{"name": "classify_repo", "input_schema": {...}}], "tool_choice": {"type": "tool", "name": "classify_repo"}}`; reads tool_use input back into ClassifyResponse.
- Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`.

- [ ] **Step 1: anthropic.rs**

`crates/qa-service/src/classifier/anthropic.rs`:
```rust
//! Anthropic Messages API classifier. tool_use is forced via tool_choice so
//! the response is always structured JSON matching ClassifyResponse.top_candidates.

use super::{
    prompt::build_prompt, schema::{ClassifyRequest, ClassifyResponse, RepoVerdict}, Classifier,
};
use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use totsuka_core::Secret;

pub struct AnthropicClassifier {
    client: Client,
    endpoint: String,
    api_key: Secret<String>,
    model: String,
    max_tokens: u32,
    top_n: u32,
    request_timeout: Duration,
}

impl AnthropicClassifier {
    pub fn new(
        api_key: Secret<String>,
        model: String,
        max_tokens: u32,
        top_n: u32,
        request_timeout: Duration,
        override_endpoint: Option<String>,
    ) -> Self {
        let endpoint = override_endpoint
            .unwrap_or_else(|| "https://api.anthropic.com/v1/messages".into());
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint,
            api_key,
            model,
            max_tokens,
            top_n,
            request_timeout,
        }
    }

    fn tool_schema(top_n: u32) -> Value {
        json!({
            "name": "classify_repo",
            "description": "Return the most-likely candidate repositories for the question.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "top_candidates": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": top_n,
                        "items": {
                            "type": "object",
                            "properties": {
                                "repo":       { "type": "string" },
                                "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                                "rationale":  { "type": "string" }
                            },
                            "required": ["repo", "confidence"]
                        }
                    }
                },
                "required": ["top_candidates"]
            }
        })
    }
}

#[async_trait]
impl Classifier for AnthropicClassifier {
    fn provider(&self) -> &str { "anthropic" }
    fn model(&self) -> &str { &self.model }

    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        let (system, user) = build_prompt(&req, self.top_n);
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": [ { "role": "user", "content": user } ],
            "tools": [ Self::tool_schema(self.top_n) ],
            "tool_choice": { "type": "tool", "name": "classify_repo" }
        });
        let start = Instant::now();
        let resp = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .timeout(self.request_timeout)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(QaError::Classifier(format!("anthropic {status}: {v}")));
        }
        let content = v["content"].as_array().ok_or_else(|| {
            QaError::Classifier(format!("anthropic: missing content array: {v}"))
        })?;
        let tool_input = content
            .iter()
            .find(|c| c["type"] == "tool_use")
            .and_then(|c| c.get("input"))
            .cloned()
            .ok_or_else(|| QaError::Classifier(format!("anthropic: no tool_use block: {v}")))?;
        let verdicts: Vec<RepoVerdict> = serde_json::from_value(tool_input["top_candidates"].clone())
            .map_err(|e| QaError::Classifier(format!("anthropic tool_use parse: {e}")))?;
        Ok(ClassifyResponse {
            top_candidates: verdicts,
            provider: self.provider().into(),
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}
```

- [ ] **Step 2: Regression test (wire-body + parsed response)**

`crates/qa-service/tests/classifier_anthropic.rs`:
```rust
use qa_service::classifier::{AnthropicClassifier, Classifier, ClassifyRequest, RepoCandidate};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const RESP: &str = r#"{
  "content": [
    { "type": "tool_use", "name": "classify_repo",
      "input": { "top_candidates": [
        { "repo": "acme/api", "confidence": 0.91, "rationale": "auth handler lives here" },
        { "repo": "acme/web", "confidence": 0.42, "rationale": "frontend" }
      ] } }
  ]
}"#;

#[tokio::test]
async fn anthropic_forces_tool_use_and_parses_top_candidates() {
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
        let body = RESP;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        buf
    });

    let c = AnthropicClassifier::new(
        Secret::new("sk-ant-test".into()),
        "claude-haiku-4-5-20251001".into(),
        256, 3, Duration::from_secs(15),
        Some(format!("http://{addr}/v1/messages")),
    );
    let req = ClassifyRequest {
        question: "Where does login live?".into(),
        thread_context: None,
        candidates: vec![
            RepoCandidate { repo: "acme/api".into(), description: "auth backend".into() },
            RepoCandidate { repo: "acme/web".into(), description: "frontend".into() },
        ],
    };
    let out = c.classify(req).await.unwrap();
    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    // Wire body asserts: tool_choice forced, tool name present, prompt contains repos.
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], "classify_repo");
    assert_eq!(body["tools"][0]["name"], "classify_repo");
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.contains("acme/api"));
    assert!(user.contains("Where does login live?"));

    // Parsed response asserts.
    assert_eq!(out.top_candidates.len(), 2);
    assert_eq!(out.top_candidates[0].repo, "acme/api");
    assert!((out.top_candidates[0].confidence - 0.91).abs() < 1e-9);
    assert_eq!(out.provider, "anthropic");
}
```

Re-export in `crates/qa-service/src/classifier/mod.rs`: add `pub mod anthropic; pub use anthropic::AnthropicClassifier;`.

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test classifier_anthropic
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): Anthropic classifier (tool_use forced)"
```

---

### Task 8: OpenAI-compat classifier (response_format json_schema)

**Files:**
- Create: `crates/qa-service/src/classifier/openai_compat.rs`
- Create: `crates/qa-service/tests/classifier_openai_compat.rs`
- Modify: `crates/qa-service/src/classifier/mod.rs`

**Interfaces:**
- Produces: `pub struct OpenAiCompatClassifier { client, endpoint, api_key (Secret), model, max_tokens, top_n, request_timeout }` + `Classifier` impl. Endpoint resolution: explicit `override_endpoint` first, else `{api_base}/chat/completions` where `api_base` defaults per provider (`openai` → `https://api.openai.com/v1`, `openrouter` → `https://openrouter.ai/api/v1`, `litellm` / `openai_compatible` → required, no default). The constructor takes the already-resolved `endpoint` so default resolution happens in the factory (Task 9).
- POSTs `{"model": ..., "max_tokens": ..., "messages": [system, user], "response_format": {"type": "json_schema", "json_schema": {"name": "classify_repo", "strict": true, "schema": {...}}}}`. Header: `Authorization: Bearer {api_key}`.

- [ ] **Step 1: openai_compat.rs**

`crates/qa-service/src/classifier/openai_compat.rs`:
```rust
//! OpenAI-style Chat Completions classifier — shared across openai /
//! openrouter / litellm / openai_compatible. response_format json_schema
//! forces structured output.

use super::{
    prompt::build_prompt, schema::{ClassifyRequest, ClassifyResponse, RepoVerdict}, Classifier,
};
use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use totsuka_core::Secret;

pub struct OpenAiCompatClassifier {
    client: Client,
    endpoint: String,
    api_key: Secret<String>,
    model: String,
    max_tokens: u32,
    top_n: u32,
    request_timeout: Duration,
    provider_name: String,
}

impl OpenAiCompatClassifier {
    pub fn new(
        provider_name: String,
        endpoint: String,
        api_key: Secret<String>,
        model: String,
        max_tokens: u32,
        top_n: u32,
        request_timeout: Duration,
    ) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint,
            api_key,
            model,
            max_tokens,
            top_n,
            request_timeout,
            provider_name,
        }
    }

    fn response_schema(top_n: u32) -> Value {
        json!({
            "type": "object",
            "properties": {
                "top_candidates": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": top_n,
                    "items": {
                        "type": "object",
                        "properties": {
                            "repo":       { "type": "string" },
                            "confidence": { "type": "number" },
                            "rationale":  { "type": "string" }
                        },
                        "required": ["repo", "confidence", "rationale"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["top_candidates"],
            "additionalProperties": false
        })
    }
}

#[async_trait]
impl Classifier for OpenAiCompatClassifier {
    fn provider(&self) -> &str { &self.provider_name }
    fn model(&self) -> &str { &self.model }

    async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        let (system, user) = build_prompt(&req, self.top_n);
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user   }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "classify_repo",
                    "strict": true,
                    "schema": Self::response_schema(self.top_n)
                }
            }
        });
        let start = Instant::now();
        let resp = self
            .client
            .post(&self.endpoint)
            .header("authorization", format!("Bearer {}", self.api_key.expose()))
            .header("content-type", "application/json")
            .timeout(self.request_timeout)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(QaError::Classifier(format!("{} {status}: {v}", self.provider_name)));
        }
        let content = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| {
            QaError::Classifier(format!("{}: missing choices[0].message.content: {v}", self.provider_name))
        })?;
        let parsed: Value = serde_json::from_str(content).map_err(|e| {
            QaError::Classifier(format!("{}: content not JSON: {e}", self.provider_name))
        })?;
        let verdicts: Vec<RepoVerdict> = serde_json::from_value(parsed["top_candidates"].clone())
            .map_err(|e| QaError::Classifier(format!("{}: top_candidates parse: {e}", self.provider_name)))?;
        Ok(ClassifyResponse {
            top_candidates: verdicts,
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}
```

- [ ] **Step 2: Regression test**

`crates/qa-service/tests/classifier_openai_compat.rs`:
```rust
use qa_service::classifier::{ClassifyRequest, Classifier, OpenAiCompatClassifier, RepoCandidate};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const RESP: &str = r#"{
  "choices": [
    { "message": { "role": "assistant",
                   "content": "{\"top_candidates\":[{\"repo\":\"acme/api\",\"confidence\":0.83,\"rationale\":\"auth\"},{\"repo\":\"acme/web\",\"confidence\":0.21,\"rationale\":\"ui\"}]}" } }
  ]
}"#;

#[tokio::test]
async fn openai_compat_forces_json_schema_and_parses_content() {
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
        let body = RESP;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        buf
    });

    let c = OpenAiCompatClassifier::new(
        "openrouter".into(),
        format!("http://{addr}/v1/chat/completions"),
        Secret::new("sk-or-test".into()),
        "anthropic/claude-3-5-haiku".into(),
        256, 3, Duration::from_secs(15),
    );
    let req = ClassifyRequest {
        question: "auth flow?".into(),
        thread_context: None,
        candidates: vec![
            RepoCandidate { repo: "acme/api".into(), description: "backend".into() },
            RepoCandidate { repo: "acme/web".into(), description: "frontend".into() },
        ],
    };
    let out = c.classify(req).await.unwrap();
    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["name"], "classify_repo");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);

    assert_eq!(out.top_candidates.len(), 2);
    assert_eq!(out.top_candidates[0].repo, "acme/api");
    assert!((out.top_candidates[0].confidence - 0.83).abs() < 1e-9);
    assert_eq!(out.provider, "openrouter");
}
```

Re-export in `mod.rs`: `pub mod openai_compat; pub use openai_compat::OpenAiCompatClassifier;`.

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test classifier_openai_compat
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): OpenAI-compat classifier (response_format json_schema)"
```

---

### Task 9: Classifier factory (dispatch by `[qa_service.classifier].provider`)

**Files:**
- Modify: `crates/qa-service/src/classifier/mod.rs`
- Create: `crates/qa-service/tests/classifier_dispatch.rs`

**Interfaces:**
- Produces: `pub fn build(cfg: &ClassifierSection) -> Result<Arc<dyn Classifier>, QaError>` — selects `AnthropicClassifier` for `"anthropic"`, `OpenAiCompatClassifier` for `"openai" | "openrouter" | "litellm" | "openai_compatible"`. Default endpoints per spec §6: `anthropic` → `https://api.anthropic.com/v1/messages`, `openai` → `https://api.openai.com/v1/chat/completions`, `openrouter` → `https://openrouter.ai/api/v1/chat/completions`, `litellm` / `openai_compatible` → required `api_base` (else error). Unknown provider → `QaError::Classifier(format!("unknown provider: {p}"))`.

- [ ] **Step 1: Append factory to mod.rs**

Append to `crates/qa-service/src/classifier/mod.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;
use totsuka_config::schema::ClassifierSection;

pub fn build(cfg: &ClassifierSection) -> Result<Arc<dyn Classifier>, QaError> {
    let timeout = Duration::from_secs(cfg.request_timeout_secs);
    match cfg.provider.as_str() {
        "anthropic" => {
            let endpoint = if cfg.api_base.is_empty() {
                None
            } else {
                Some(format!("{}/v1/messages", cfg.api_base.trim_end_matches('/')))
            };
            Ok(Arc::new(anthropic::AnthropicClassifier::new(
                cfg.api_key.clone(),
                cfg.model.clone(),
                cfg.max_tokens,
                cfg.top_candidates,
                timeout,
                endpoint,
            )))
        }
        provider @ ("openai" | "openrouter" | "litellm" | "openai_compatible") => {
            let base = if cfg.api_base.is_empty() {
                match provider {
                    "openai" => "https://api.openai.com/v1".to_string(),
                    "openrouter" => "https://openrouter.ai/api/v1".to_string(),
                    _ => return Err(QaError::Classifier(format!(
                        "{provider}: api_base is required (no default)"
                    ))),
                }
            } else {
                cfg.api_base.trim_end_matches('/').to_string()
            };
            let endpoint = format!("{base}/chat/completions");
            Ok(Arc::new(openai_compat::OpenAiCompatClassifier::new(
                provider.into(),
                endpoint,
                cfg.api_key.clone(),
                cfg.model.clone(),
                cfg.max_tokens,
                cfg.top_candidates,
                timeout,
            )))
        }
        other => Err(QaError::Classifier(format!("unknown provider: {other}"))),
    }
}
```

- [ ] **Step 2: Dispatch test**

`crates/qa-service/tests/classifier_dispatch.rs`:
```rust
use qa_service::classifier::build;
use qa_service::error::QaError;
use totsuka_config::schema::ClassifierSection;
use totsuka_core::Secret;

fn cfg(provider: &str, api_base: &str) -> ClassifierSection {
    ClassifierSection {
        provider: provider.into(),
        model: "m".into(),
        api_base: api_base.into(),
        api_key: Secret::new("k".into()),
        max_tokens: 256,
        confidence_threshold: 0.7,
        top_candidates: 3,
        on_low_confidence: "delegated_reaction".into(),
        include_thread_context: true,
        request_timeout_secs: 15,
    }
}

#[test]
fn anthropic_builds() {
    let c = build(&cfg("anthropic", "")).unwrap();
    assert_eq!(c.provider(), "anthropic");
}

#[test]
fn openai_builds_with_default_endpoint() {
    let c = build(&cfg("openai", "")).unwrap();
    assert_eq!(c.provider(), "openai");
}

#[test]
fn openrouter_builds_with_default_endpoint() {
    let c = build(&cfg("openrouter", "")).unwrap();
    assert_eq!(c.provider(), "openrouter");
}

#[test]
fn litellm_requires_api_base() {
    let err = build(&cfg("litellm", "")).unwrap_err();
    assert!(matches!(err, QaError::Classifier(s) if s.contains("litellm")));
    let c = build(&cfg("litellm", "http://localhost:4000")).unwrap();
    assert_eq!(c.provider(), "litellm");
}

#[test]
fn openai_compatible_requires_api_base() {
    let err = build(&cfg("openai_compatible", "")).unwrap_err();
    assert!(matches!(err, QaError::Classifier(s) if s.contains("openai_compatible")));
}

#[test]
fn unknown_provider_errors() {
    let err = build(&cfg("does_not_exist", "")).unwrap_err();
    assert!(matches!(err, QaError::Classifier(s) if s.contains("unknown provider")));
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test classifier_dispatch
```
Expected: 6 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): classifier factory dispatch (5 providers via 2 impls)"
```

---

### Task 10: RepoSelector (threshold + on_low_confidence policy)

**Files:**
- Create: `crates/qa-service/src/repo_select.rs`
- Create: `crates/qa-service/tests/repo_select.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum SelectOutcome { HighConfidence { repo: String, verdict: RepoVerdict }, LowConfidenceDelegated { candidates: Vec<RepoVerdict> }, LowConfidenceRefused, LowConfidenceUseTop1 { repo: String, verdict: RepoVerdict } }`
  - `pub struct RepoSelector { threshold: f64, on_low: LowConfidencePolicy }`
  - `pub enum LowConfidencePolicy { DelegatedReaction, Refuse, UseTop1 }` (parsed from string in `from_cfg`)
  - `pub fn from_cfg(threshold: f64, on_low: &str) -> Result<Self, QaError>`
  - `pub fn decide(&self, response: &ClassifyResponse) -> SelectOutcome`

- [ ] **Step 1: Implement**

`crates/qa-service/src/repo_select.rs`:
```rust
//! spec §8.4 step 3: apply confidence_threshold + on_low_confidence policy
//! to a classifier response.

use crate::classifier::{ClassifyResponse, RepoVerdict};
use crate::error::QaError;

#[derive(Debug, Clone, PartialEq)]
pub enum LowConfidencePolicy {
    DelegatedReaction,
    Refuse,
    UseTop1,
}

impl LowConfidencePolicy {
    pub fn parse(s: &str) -> Result<Self, QaError> {
        match s {
            "delegated_reaction" => Ok(Self::DelegatedReaction),
            "refuse"             => Ok(Self::Refuse),
            "use_top1"           => Ok(Self::UseTop1),
            other => Err(QaError::Internal(format!("unknown on_low_confidence: {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectOutcome {
    HighConfidence { repo: String, verdict: RepoVerdict },
    LowConfidenceDelegated { candidates: Vec<RepoVerdict> },
    LowConfidenceRefused,
    LowConfidenceUseTop1 { repo: String, verdict: RepoVerdict },
}

pub struct RepoSelector {
    threshold: f64,
    on_low: LowConfidencePolicy,
}

impl RepoSelector {
    pub fn from_cfg(threshold: f64, on_low: &str) -> Result<Self, QaError> {
        Ok(Self { threshold, on_low: LowConfidencePolicy::parse(on_low)? })
    }

    pub fn decide(&self, response: &ClassifyResponse) -> SelectOutcome {
        let Some(top) = response.top_candidates.first() else {
            return SelectOutcome::LowConfidenceRefused;
        };
        if top.confidence >= self.threshold {
            return SelectOutcome::HighConfidence {
                repo: top.repo.clone(),
                verdict: top.clone(),
            };
        }
        match self.on_low {
            LowConfidencePolicy::DelegatedReaction => SelectOutcome::LowConfidenceDelegated {
                candidates: response.top_candidates.clone(),
            },
            LowConfidencePolicy::Refuse => SelectOutcome::LowConfidenceRefused,
            LowConfidencePolicy::UseTop1 => SelectOutcome::LowConfidenceUseTop1 {
                repo: top.repo.clone(),
                verdict: top.clone(),
            },
        }
    }
}
```

Add `pub mod repo_select;` to `lib.rs`.

- [ ] **Step 2: Unit tests**

`crates/qa-service/tests/repo_select.rs`:
```rust
use qa_service::classifier::{ClassifyResponse, RepoVerdict};
use qa_service::repo_select::{RepoSelector, SelectOutcome};

fn response(verdicts: Vec<RepoVerdict>) -> ClassifyResponse {
    ClassifyResponse {
        top_candidates: verdicts,
        provider: "mock".into(),
        model: "m".into(),
        latency_ms: 1,
    }
}
fn v(repo: &str, confidence: f64) -> RepoVerdict {
    RepoVerdict { repo: repo.into(), confidence, rationale: "".into() }
}

#[test]
fn high_confidence_picks_top1() {
    let sel = RepoSelector::from_cfg(0.70, "refuse").unwrap();
    let r = response(vec![v("acme/api", 0.91), v("acme/web", 0.30)]);
    match sel.decide(&r) {
        SelectOutcome::HighConfidence { repo, verdict } => {
            assert_eq!(repo, "acme/api");
            assert!((verdict.confidence - 0.91).abs() < 1e-9);
        }
        other => panic!("expected HighConfidence, got {other:?}"),
    }
}

#[test]
fn delegated_reaction_returns_candidates() {
    let sel = RepoSelector::from_cfg(0.70, "delegated_reaction").unwrap();
    let r = response(vec![v("acme/api", 0.42), v("acme/web", 0.31)]);
    match sel.decide(&r) {
        SelectOutcome::LowConfidenceDelegated { candidates } => {
            assert_eq!(candidates.len(), 2);
            assert_eq!(candidates[0].repo, "acme/api");
        }
        other => panic!("expected LowConfidenceDelegated, got {other:?}"),
    }
}

#[test]
fn refuse_returns_refused() {
    let sel = RepoSelector::from_cfg(0.70, "refuse").unwrap();
    let r = response(vec![v("acme/api", 0.42)]);
    assert_eq!(sel.decide(&r), SelectOutcome::LowConfidenceRefused);
}

#[test]
fn use_top1_forces_top1_below_threshold() {
    let sel = RepoSelector::from_cfg(0.70, "use_top1").unwrap();
    let r = response(vec![v("acme/api", 0.40)]);
    match sel.decide(&r) {
        SelectOutcome::LowConfidenceUseTop1 { repo, .. } => assert_eq!(repo, "acme/api"),
        other => panic!("expected LowConfidenceUseTop1, got {other:?}"),
    }
}

#[test]
fn empty_response_refuses() {
    let sel = RepoSelector::from_cfg(0.70, "delegated_reaction").unwrap();
    assert_eq!(sel.decide(&response(vec![])), SelectOutcome::LowConfidenceRefused);
}

#[test]
fn invalid_policy_string_errors() {
    assert!(RepoSelector::from_cfg(0.70, "made_up_policy").is_err());
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test repo_select
```
Expected: 6 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): RepoSelector (threshold + on_low_confidence policy)"
```

---

### Task 11: AnswerMode enum + Mode parsing

**Files:**
- Create: `crates/qa-service/src/mode.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum AnswerMode { Auto, Delegated }`
  - `pub fn parse(s: &str) -> Result<AnswerMode, QaError>`

- [ ] **Step 1: Implement + tests**

`crates/qa-service/src/mode.rs`:
```rust
//! spec §8.4: default_mode = "auto" | "delegated".

use crate::error::QaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerMode { Auto, Delegated }

impl AnswerMode {
    pub fn parse(s: &str) -> Result<Self, QaError> {
        match s {
            "auto"      => Ok(Self::Auto),
            "delegated" => Ok(Self::Delegated),
            other => Err(QaError::Internal(format!("unknown default_mode: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn parses_known() {
        assert_eq!(AnswerMode::parse("auto").unwrap(), AnswerMode::Auto);
        assert_eq!(AnswerMode::parse("delegated").unwrap(), AnswerMode::Delegated);
    }
    #[test] fn rejects_unknown() {
        assert!(AnswerMode::parse("xyz").is_err());
    }
}
```

Add `pub mod mode;` to `lib.rs`.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p qa-service mode::
```
Expected: 2 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): AnswerMode enum"
```

---

### Task 12: Slack Web API client (postMessage / postEphemeral / history / replies)

**Files:**
- Create: `crates/qa-service/src/slack/mod.rs`
- Create: `crates/qa-service/src/slack/web.rs`
- Create: `crates/qa-service/src/slack/mock.rs`
- Create: `crates/qa-service/tests/slack_web.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct SlackMessage { pub channel: String, pub user: String, pub text: String, pub ts: String, pub thread_ts: Option<String>, pub event_id: String }`
  - `pub struct SlackPostResult { pub ts: String }`
  - `#[async_trait] pub trait SlackClient: Send + Sync + 'static { async fn post_message(&self, channel: &str, thread_ts: Option<&str>, text: &str) -> Result<SlackPostResult, QaError>; async fn post_ephemeral(&self, channel: &str, user: &str, thread_ts: Option<&str>, text: &str) -> Result<(), QaError>; async fn conversation_history(&self, channel: &str, oldest: Option<&str>, limit: u32) -> Result<Vec<SlackMessage>, QaError>; async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError>; async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError> }`
  - `pub struct HttpSlackClient { client, endpoint, bot_token (Secret) }` + `new(bot_token, override_endpoint: Option<String>) -> Self` (default `https://slack.com/api`)
  - `pub struct MockSlackClient` (mod `mock.rs`) with `Arc<Mutex<MockState>>`

- [ ] **Step 1: web.rs**

`crates/qa-service/src/slack/web.rs`:
```rust
//! Slack Web API client — POST application/x-www-form-urlencoded with bot
//! token; responses are JSON with {"ok": bool, "error": "...", ...} envelope.

use crate::error::QaError;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use totsuka_core::Secret;

use super::{SlackClient, SlackMessage, SlackPostResult};

pub struct HttpSlackClient {
    client: Client,
    endpoint: String,
    bot_token: Secret<String>,
}

impl HttpSlackClient {
    pub fn new(bot_token: Secret<String>, override_endpoint: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint: override_endpoint
                .unwrap_or_else(|| "https://slack.com/api".into())
                .trim_end_matches('/')
                .to_string(),
            bot_token,
        }
    }

    async fn post_form(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, QaError> {
        let url = format!("{}/{}", self.endpoint, method);
        let resp = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.bot_token.expose()))
            .header("content-type", "application/x-www-form-urlencoded; charset=utf-8")
            .form(params)
            .send()
            .await?;
        let v: Value = resp.json().await?;
        if !v["ok"].as_bool().unwrap_or(false) {
            return Err(QaError::Slack(format!(
                "{method}: {}",
                v["error"].as_str().unwrap_or("unknown")
            )));
        }
        Ok(v)
    }
}

#[derive(Deserialize)]
struct PostMessageResp {
    ts: String,
}

#[async_trait]
impl SlackClient for HttpSlackClient {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError> {
        let mut params: Vec<(&str, &str)> = vec![("channel", channel), ("text", text)];
        if let Some(t) = thread_ts {
            params.push(("thread_ts", t));
        }
        let v = self.post_form("chat.postMessage", &params).await?;
        let parsed: PostMessageResp = serde_json::from_value(v)
            .map_err(|e| QaError::Slack(format!("postMessage parse: {e}")))?;
        Ok(SlackPostResult { ts: parsed.ts })
    }

    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError> {
        let mut params: Vec<(&str, &str)> =
            vec![("channel", channel), ("user", user), ("text", text)];
        if let Some(t) = thread_ts {
            params.push(("thread_ts", t));
        }
        self.post_form("chat.postEphemeral", &params).await?;
        Ok(())
    }

    async fn conversation_history(
        &self,
        channel: &str,
        oldest: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError> {
        let limit_s = limit.to_string();
        let mut params: Vec<(&str, &str)> = vec![("channel", channel), ("limit", &limit_s)];
        if let Some(o) = oldest {
            params.push(("oldest", o));
        }
        let v = self.post_form("conversations.history", &params).await?;
        parse_messages(channel, &v)
    }

    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError> {
        let v = self
            .post_form("conversations.replies", &[("channel", channel), ("ts", thread_ts)])
            .await?;
        parse_messages(channel, &v)
    }

    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError> {
        self.post_form(
            "reactions.add",
            &[("channel", channel), ("timestamp", ts), ("name", name)],
        )
        .await?;
        Ok(())
    }
}

fn parse_messages(channel: &str, v: &Value) -> Result<Vec<SlackMessage>, QaError> {
    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let ts = m["ts"].as_str().unwrap_or("").to_string();
        out.push(SlackMessage {
            channel: channel.into(),
            user: m["user"].as_str().unwrap_or("").to_string(),
            text: m["text"].as_str().unwrap_or("").to_string(),
            ts: ts.clone(),
            thread_ts: m["thread_ts"].as_str().map(str::to_string),
            event_id: ts, // history msgs have no event_id; use ts for dedupe
        });
    }
    Ok(out)
}
```

- [ ] **Step 2: mod.rs + mock.rs**

`crates/qa-service/src/slack/mod.rs`:
```rust
use async_trait::async_trait;
use crate::error::QaError;

pub mod envelope;
pub mod mock;
pub mod socket;
pub mod web;

pub use mock::MockSlackClient;
pub use web::HttpSlackClient;

#[derive(Debug, Clone, PartialEq)]
pub struct SlackMessage {
    pub channel: String,
    pub user: String,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlackPostResult {
    pub ts: String,
}

#[async_trait]
pub trait SlackClient: Send + Sync + 'static {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError>;

    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError>;

    async fn conversation_history(
        &self,
        channel: &str,
        oldest: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError>;

    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError>;

    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError>;
}
```

`crates/qa-service/src/slack/mock.rs`:
```rust
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MockState {
    posts: Vec<(String, Option<String>, String, String /* returned ts */)>,
    ephemerals: Vec<(String, String, Option<String>, String)>,
    reactions: Vec<(String, String, String)>,
    history: HashMap<String, Vec<SlackMessage>>,
    replies: HashMap<(String, String), Vec<SlackMessage>>,
    next_post_ts: u64,
}

pub struct MockSlackClient {
    state: Mutex<MockState>,
}

impl Default for MockSlackClient {
    fn default() -> Self { Self::new() }
}

impl MockSlackClient {
    pub fn new() -> Self {
        let mut s = MockState::default();
        s.next_post_ts = 17_500_000_000;
        Self { state: Mutex::new(s) }
    }
    pub fn set_history(&self, channel: &str, msgs: Vec<SlackMessage>) {
        self.state.lock().unwrap().history.insert(channel.into(), msgs);
    }
    pub fn set_replies(&self, channel: &str, thread_ts: &str, msgs: Vec<SlackMessage>) {
        self.state.lock().unwrap().replies.insert((channel.into(), thread_ts.into()), msgs);
    }
    pub fn posts(&self) -> Vec<(String, Option<String>, String, String)> {
        self.state.lock().unwrap().posts.clone()
    }
    pub fn ephemerals(&self) -> Vec<(String, String, Option<String>, String)> {
        self.state.lock().unwrap().ephemerals.clone()
    }
    pub fn reactions(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().reactions.clone()
    }
}

#[async_trait]
impl SlackClient for MockSlackClient {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<SlackPostResult, QaError> {
        let mut s = self.state.lock().unwrap();
        s.next_post_ts += 1;
        let ts = format!("{}.000000", s.next_post_ts);
        s.posts.push((channel.into(), thread_ts.map(str::to_string), text.into(), ts.clone()));
        Ok(SlackPostResult { ts })
    }
    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), QaError> {
        self.state.lock().unwrap().ephemerals.push((
            channel.into(),
            user.into(),
            thread_ts.map(str::to_string),
            text.into(),
        ));
        Ok(())
    }
    async fn conversation_history(
        &self,
        channel: &str,
        _oldest: Option<&str>,
        _limit: u32,
    ) -> Result<Vec<SlackMessage>, QaError> {
        Ok(self.state.lock().unwrap().history.get(channel).cloned().unwrap_or_default())
    }
    async fn replies(&self, channel: &str, thread_ts: &str) -> Result<Vec<SlackMessage>, QaError> {
        Ok(self.state.lock().unwrap().replies
            .get(&(channel.into(), thread_ts.into())).cloned().unwrap_or_default())
    }
    async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<(), QaError> {
        self.state.lock().unwrap().reactions.push((channel.into(), ts.into(), name.into()));
        Ok(())
    }
}
```

Add `pub mod slack;` to `lib.rs`. Stub `slack/socket.rs` and `slack/envelope.rs` as empty modules (single `pub fn placeholder() {}`); Tasks 13-14 fill them in.

- [ ] **Step 3: TCP stub integration test**

`crates/qa-service/tests/slack_web.rs`:
```rust
use qa_service::slack::{HttpSlackClient, SlackClient};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

async fn one_shot_stub(payload: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut cl = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" { break; }
            if let Some(v) = line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")) {
                cl = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; cl];
        reader.read_exact(&mut body).await.unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            payload.len(),
            payload,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn post_message_returns_ts_on_ok() {
    let addr = one_shot_stub(r#"{"ok":true,"ts":"17500000001.000200"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let r = c.post_message("C1", None, "hi").await.unwrap();
    assert_eq!(r.ts, "17500000001.000200");
}

#[tokio::test]
async fn post_message_errors_on_not_ok() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"channel_not_found"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let err = c.post_message("C1", None, "hi").await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("channel_not_found"), "got: {s}");
}

#[tokio::test]
async fn history_returns_parsed_messages() {
    let payload = r#"{"ok":true,"messages":[
      {"user":"U1","text":"hello","ts":"17500000001.000100","thread_ts":null},
      {"user":"U2","text":"there","ts":"17500000002.000100","thread_ts":"17500000001.000100"}
    ]}"#;
    let addr = one_shot_stub(payload).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let msgs = c.conversation_history("C1", None, 10).await.unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].text, "hello");
    assert_eq!(msgs[1].thread_ts.as_deref(), Some("17500000001.000100"));
}
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p qa-service --test slack_web
```
Expected: 3 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): Slack Web API client + Mock"
```

---

### Task 13: Slack Socket Mode envelope parser

**Files:**
- Create: `crates/qa-service/src/slack/envelope.rs` (replace placeholder)
- Create: `crates/qa-service/tests/slack_envelope.rs`

**Interfaces:**
- Produces:
  - `pub enum SlackEnvelope { Hello, Disconnect { reason: String }, EventsApi { envelope_id: String, event: SlackEvent } }` — server-initiated frames.
  - `pub enum SlackEvent { Message(SlackMessage), ReactionAdded { user: String, channel: String, item_ts: String, reaction: String, event_ts: String, event_id: String }, Other }`
  - `pub fn parse(raw: &str) -> Result<SlackEnvelope, QaError>` — `serde_json`-based; unknown envelope `type` → `QaError::Slack("unknown envelope type: {t}")`; unknown event subtypes → `SlackEvent::Other` (caller discards).

- [ ] **Step 1: envelope.rs**

`crates/qa-service/src/slack/envelope.rs`:
```rust
//! Slack Socket Mode envelope parser. The Socket Mode endpoint serves a
//! WebSocket of JSON envelopes; events_api envelopes must be ACK'd by
//! sending `{"envelope_id": "..."}` back on the same socket.

use crate::error::QaError;
use serde_json::Value;

use super::SlackMessage;

#[derive(Debug, Clone, PartialEq)]
pub enum SlackEnvelope {
    Hello,
    Disconnect { reason: String },
    EventsApi { envelope_id: String, event: SlackEvent },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlackEvent {
    Message(SlackMessage),
    ReactionAdded {
        user: String,
        channel: String,
        item_ts: String,
        reaction: String,
        event_ts: String,
        event_id: String,
    },
    Other,
}

pub fn parse(raw: &str) -> Result<SlackEnvelope, QaError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| QaError::Slack(format!("envelope parse: {e}")))?;
    match v["type"].as_str() {
        Some("hello") => Ok(SlackEnvelope::Hello),
        Some("disconnect") => Ok(SlackEnvelope::Disconnect {
            reason: v["reason"].as_str().unwrap_or("unknown").into(),
        }),
        Some("events_api") => {
            let envelope_id = v["envelope_id"].as_str()
                .ok_or_else(|| QaError::Slack("events_api missing envelope_id".into()))?
                .to_string();
            let event = parse_event(&v["payload"]["event"], &v["payload"])?;
            Ok(SlackEnvelope::EventsApi { envelope_id, event })
        }
        Some(other) => Err(QaError::Slack(format!("unknown envelope type: {other}"))),
        None => Err(QaError::Slack("envelope missing type".into())),
    }
}

fn parse_event(ev: &Value, payload: &Value) -> Result<SlackEvent, QaError> {
    let event_id = payload["event_id"].as_str().unwrap_or("").to_string();
    match ev["type"].as_str() {
        Some("message") => {
            // Ignore bot messages, message_changed/deleted subtypes — only top-level
            // user messages reach the question pipeline.
            if ev["subtype"].is_string() || ev["bot_id"].is_string() {
                return Ok(SlackEvent::Other);
            }
            let ts = ev["ts"].as_str().unwrap_or("").to_string();
            Ok(SlackEvent::Message(SlackMessage {
                channel: ev["channel"].as_str().unwrap_or("").to_string(),
                user: ev["user"].as_str().unwrap_or("").to_string(),
                text: ev["text"].as_str().unwrap_or("").to_string(),
                ts: ts.clone(),
                thread_ts: ev["thread_ts"].as_str().map(str::to_string),
                event_id,
            }))
        }
        Some("reaction_added") => Ok(SlackEvent::ReactionAdded {
            user: ev["user"].as_str().unwrap_or("").to_string(),
            channel: ev["item"]["channel"].as_str().unwrap_or("").to_string(),
            item_ts: ev["item"]["ts"].as_str().unwrap_or("").to_string(),
            reaction: ev["reaction"].as_str().unwrap_or("").to_string(),
            event_ts: ev["event_ts"].as_str().unwrap_or("").to_string(),
            event_id,
        }),
        _ => Ok(SlackEvent::Other),
    }
}
```

- [ ] **Step 2: Tests**

`crates/qa-service/tests/slack_envelope.rs`:
```rust
use qa_service::slack::envelope::{parse, SlackEnvelope, SlackEvent};

#[test]
fn parses_hello() {
    let r = parse(r#"{"type":"hello","num_connections":1}"#).unwrap();
    assert_eq!(r, SlackEnvelope::Hello);
}

#[test]
fn parses_disconnect_with_reason() {
    let r = parse(r#"{"type":"disconnect","reason":"warning"}"#).unwrap();
    match r {
        SlackEnvelope::Disconnect { reason } => assert_eq!(reason, "warning"),
        _ => panic!(),
    }
}

#[test]
fn parses_events_api_message() {
    let raw = r#"{"type":"events_api","envelope_id":"env-1","payload":{
      "event_id":"Ev0001",
      "event":{"type":"message","channel":"C1","user":"U1","text":"hi","ts":"17500000001.000100"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { envelope_id, event: SlackEvent::Message(m) } => {
            assert_eq!(envelope_id, "env-1");
            assert_eq!(m.text, "hi");
            assert_eq!(m.event_id, "Ev0001");
            assert_eq!(m.channel, "C1");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_reaction_added() {
    let raw = r#"{"type":"events_api","envelope_id":"env-2","payload":{
      "event_id":"Ev0002",
      "event":{"type":"reaction_added","user":"U1","reaction":"memo",
               "item":{"type":"message","channel":"C1","ts":"17500000001.000100"},
               "event_ts":"17500000003.000100"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event: SlackEvent::ReactionAdded { reaction, channel, item_ts, .. }, .. } => {
            assert_eq!(reaction, "memo");
            assert_eq!(channel, "C1");
            assert_eq!(item_ts, "17500000001.000100");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ignores_subtype_messages() {
    let raw = r#"{"type":"events_api","envelope_id":"e","payload":{
      "event_id":"Ev","event":{"type":"message","subtype":"message_changed","channel":"C1"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event, .. } => assert_eq!(event, SlackEvent::Other),
        _ => panic!(),
    }
}

#[test]
fn ignores_bot_messages() {
    let raw = r#"{"type":"events_api","envelope_id":"e","payload":{
      "event_id":"Ev","event":{"type":"message","bot_id":"B1","channel":"C1","text":"x","ts":"1"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event, .. } => assert_eq!(event, SlackEvent::Other),
        _ => panic!(),
    }
}

#[test]
fn unknown_envelope_type_errors() {
    assert!(parse(r#"{"type":"slash_commands","..":""}"#).is_err());
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test slack_envelope
```
Expected: 7 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): Slack Socket Mode envelope parser"
```

---

### Task 14: Slack Socket Mode WebSocket loop

**Files:**
- Create: `crates/qa-service/src/slack/socket.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub struct SocketModeConfig { pub app_token: Secret<String>, pub apps_connections_endpoint: String /* default https://slack.com/api/apps.connections.open */ }`
  - `pub async fn fetch_socket_url(client: &reqwest::Client, cfg: &SocketModeConfig) -> Result<String, QaError>` — POSTs to apps.connections.open, returns the WSS URL
  - `pub async fn run_socket_loop(cfg: SocketModeConfig, http: Arc<reqwest::Client>, on_event: mpsc::Sender<SlackEvent>, shutdown: CancellationToken) -> Result<(), QaError>` — connect via tokio-tungstenite, await hello, loop: receive envelope → if EventsApi, send ACK + forward event via channel; on disconnect, reconnect with exp backoff (1s, 2s, 4s, cap 30s).
  - ACK frame: `{"envelope_id": "..."}` — must be sent back BEFORE forwarding the event so Slack doesn't retry.
  - Channel-full → `drop oldest` (`try_send` failure path): `tracing::warn!(channel="slack_inbound", "channel full; dropping event")` + metric `channel_full_total{channel="slack_inbound"}` (deferred to telemetry pass).

- [ ] **Step 1: Implement**

`crates/qa-service/src/slack/socket.rs`:
```rust
//! Slack Socket Mode WebSocket loop. Connection lifecycle:
//!   1. POST apps.connections.open → returns WSS URL
//!   2. Open WS, await `hello`
//!   3. For each `events_api` envelope: ACK first, then forward event
//!   4. On `disconnect` or transport error: reconnect with exp backoff
//!
//! ACK-before-forward avoids Slack retry storms when downstream is slow.

use crate::error::QaError;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use totsuka_core::Secret;

use super::envelope::{parse, SlackEnvelope, SlackEvent};

pub struct SocketModeConfig {
    pub app_token: Secret<String>,
    pub apps_connections_endpoint: String,
}

impl SocketModeConfig {
    pub fn new(app_token: Secret<String>) -> Self {
        Self {
            app_token,
            apps_connections_endpoint: "https://slack.com/api/apps.connections.open".into(),
        }
    }
}

pub async fn fetch_socket_url(client: &Client, cfg: &SocketModeConfig) -> Result<String, QaError> {
    let resp = client
        .post(&cfg.apps_connections_endpoint)
        .header("authorization", format!("Bearer {}", cfg.app_token.expose()))
        .header("content-type", "application/x-www-form-urlencoded")
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    if !v["ok"].as_bool().unwrap_or(false) {
        return Err(QaError::Slack(format!(
            "apps.connections.open: {}",
            v["error"].as_str().unwrap_or("unknown")
        )));
    }
    v["url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| QaError::Slack("apps.connections.open: missing url".into()))
}

pub async fn run_socket_loop(
    cfg: SocketModeConfig,
    http: Arc<Client>,
    on_event: mpsc::Sender<SlackEvent>,
    shutdown: CancellationToken,
) -> Result<(), QaError> {
    let mut attempt: u32 = 0;
    loop {
        if shutdown.is_cancelled() { return Ok(()); }
        match try_one_connection(&cfg, &http, &on_event, &shutdown).await {
            Ok(()) => {
                attempt = 0;
                tracing::info!("socket-mode disconnected cleanly; reconnecting");
            }
            Err(e) => {
                attempt = (attempt + 1).min(5);
                let backoff = 2u64.saturating_pow(attempt - 1).min(30);
                tracing::warn!(error=%e, "socket-mode error; reconnecting in {backoff}s");
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                }
            }
        }
    }
}

async fn try_one_connection(
    cfg: &SocketModeConfig,
    http: &Arc<Client>,
    on_event: &mpsc::Sender<SlackEvent>,
    shutdown: &CancellationToken,
) -> Result<(), QaError> {
    let url = fetch_socket_url(http, cfg).await?;
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| QaError::WebSocket(format!("connect: {e}")))?;
    let (mut sink, mut stream) = ws.split();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            msg = stream.next() => {
                let Some(msg) = msg else { return Ok(()); };
                let msg = msg.map_err(|e| QaError::WebSocket(format!("recv: {e}")))?;
                match msg {
                    Message::Text(raw) => {
                        let env = parse(&raw)?;
                        match env {
                            SlackEnvelope::Hello => {
                                tracing::info!("socket-mode hello received");
                            }
                            SlackEnvelope::Disconnect { reason } => {
                                tracing::info!(%reason, "socket-mode disconnect requested");
                                return Ok(());
                            }
                            SlackEnvelope::EventsApi { envelope_id, event } => {
                                // ACK first (Slack will retry within 3s otherwise).
                                let ack = serde_json::json!({ "envelope_id": envelope_id });
                                sink.send(Message::Text(ack.to_string()))
                                    .await
                                    .map_err(|e| QaError::WebSocket(format!("ack: {e}")))?;
                                // Drop-oldest semantics: try_send; on full, log.
                                if let Err(e) = on_event.try_send(event) {
                                    tracing::warn!(error=%e, channel="slack_inbound",
                                        "channel full; dropping event");
                                }
                            }
                        }
                    }
                    Message::Ping(p) => {
                        sink.send(Message::Pong(p)).await
                            .map_err(|e| QaError::WebSocket(format!("pong: {e}")))?;
                    }
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}
```

Add `futures-util = "0.3"` to `crates/qa-service/Cargo.toml` dependencies (tokio-tungstenite re-exports it but explicit is clearer; pin the same minor as `hyper-util`'s transitive resolution).

- [ ] **Step 2: Build check**

```bash
cargo check -p qa-service
```
Expected: clean.

The socket loop is exercised by Task 24 (e2e — high-conf answer) via MockSlackClient — the WebSocket itself is hard to unit-test without a live Slack endpoint, so we rely on Task 13's envelope tests for the parsing path and the e2e for end-to-end wiring.

- [ ] **Step 3: Commit**

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): Slack Socket Mode WebSocket loop"
```

---

### Task 15: Question filter (allowed_user_ids + mention/thread continuation)

**Files:**
- Create: `crates/qa-service/src/question_filter.rs`
- Create: `crates/qa-service/tests/question_filter.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum Trigger { Mention, ThreadContinuation, None }`
  - `pub struct QuestionFilter { allowed_user_ids: HashSet<String>, bot_user_id: String }`
  - `pub fn new(allowed_user_ids: Vec<String>, bot_user_id: String) -> Self`
  - `pub fn evaluate(&self, msg: &SlackMessage, existing_mapping: bool) -> Trigger` — returns:
    - `Trigger::None` if `msg.user` not in `allowed_user_ids`.
    - `Trigger::Mention` if msg.text contains `<@{bot_user_id}>`.
    - `Trigger::ThreadContinuation` if `msg.thread_ts.is_some()` AND `existing_mapping == true`.
    - `Trigger::None` otherwise.

- [ ] **Step 1: Implement**

`crates/qa-service/src/question_filter.rs`:
```rust
//! Decide whether a Slack message should be treated as a question for the
//! QA pipeline. Two trigger paths:
//!   * Mention: text contains `<@bot_user_id>` (top-level invocation)
//!   * Thread continuation: thread_ts present AND mapping already exists
//!
//! Author must be in allowed_user_ids; bot-authored messages are already
//! filtered upstream by the envelope parser.

use crate::slack::SlackMessage;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Mention,
    ThreadContinuation,
    None,
}

pub struct QuestionFilter {
    allowed_user_ids: HashSet<String>,
    bot_user_id: String,
}

impl QuestionFilter {
    pub fn new(allowed_user_ids: Vec<String>, bot_user_id: String) -> Self {
        Self {
            allowed_user_ids: allowed_user_ids.into_iter().collect(),
            bot_user_id,
        }
    }

    pub fn evaluate(&self, msg: &SlackMessage, existing_mapping: bool) -> Trigger {
        if !self.allowed_user_ids.contains(&msg.user) {
            return Trigger::None;
        }
        if msg.text.contains(&format!("<@{}>", self.bot_user_id)) {
            return Trigger::Mention;
        }
        if msg.thread_ts.is_some() && existing_mapping {
            return Trigger::ThreadContinuation;
        }
        Trigger::None
    }
}
```

Add `pub mod question_filter;` to `lib.rs`.

- [ ] **Step 2: Unit tests**

`crates/qa-service/tests/question_filter.rs`:
```rust
use qa_service::question_filter::{QuestionFilter, Trigger};
use qa_service::slack::SlackMessage;

fn msg(user: &str, text: &str, thread_ts: Option<&str>) -> SlackMessage {
    SlackMessage {
        channel: "C1".into(),
        user: user.into(),
        text: text.into(),
        ts: "17500000001.000100".into(),
        thread_ts: thread_ts.map(str::to_string),
        event_id: "Ev1".into(),
    }
}

#[test]
fn rejects_non_allowed_user() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_OTHER", "<@U_BOT> hi", None), false), Trigger::None);
}

#[test]
fn detects_mention_on_top_level_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_ALLOWED", "<@U_BOT> hi", None), false), Trigger::Mention);
}

#[test]
fn detects_thread_continuation_only_with_existing_mapping() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "more", Some("17500000001.000100")), true),
        Trigger::ThreadContinuation,
    );
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "more", Some("17500000001.000100")), false),
        Trigger::None,
    );
}

#[test]
fn mention_takes_precedence_over_thread_continuation() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "<@U_BOT> in-thread", Some("17500000001.000100")), true),
        Trigger::Mention,
    );
}

#[test]
fn top_level_no_mention_returns_none() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_ALLOWED", "hi", None), false), Trigger::None);
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test question_filter
```
Expected: 5 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): question filter (allowed_user_ids + mention/thread)"
```

---

### Task 16: Answer extract (sentinel + tag + truncate)

**Files:**
- Create: `crates/qa-service/src/answer/mod.rs`
- Create: `crates/qa-service/src/answer/extract.rs`
- Create: `crates/qa-service/tests/answer_extract.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub enum AnswerExtraction { TagDelimited(String), FallbackTail(String), Empty }`
  - `pub struct ExtractConfig<'a> { pub sentinel: &'a str, pub open_tag: &'a str, pub close_tag: &'a str, pub max_chars: usize, pub fallback_tail_lines: usize }`
  - `pub fn extract(snapshot: &str, cfg: &ExtractConfig) -> AnswerExtraction` — strategy:
    1. Search for `open_tag` then `close_tag` AFTER it; if both present, return TagDelimited.
    2. Else: take last `fallback_tail_lines` lines of snapshot up to `sentinel` (exclusive); return FallbackTail. (Warn log fires at the caller; extract.rs stays pure.)
    3. Empty snapshot → Empty.
    All non-Empty outputs are byte-truncated to `max_chars` (UTF-8 safe: trim to char boundary).

- [ ] **Step 1: mod.rs + extract.rs**

`crates/qa-service/src/answer/mod.rs`:
```rust
pub mod extract;
pub mod pipeline;

pub use extract::{extract, AnswerExtraction, ExtractConfig};
```

`crates/qa-service/src/answer/extract.rs`:
```rust
//! Pure functions for pulling the answer text out of a pane snapshot.
//! Strategy: sentinel-bounded extraction first; on tag absence, fall back to
//! the last N lines before sentinel; UTF-8-safe truncate to max_chars.

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerExtraction {
    TagDelimited(String),
    FallbackTail(String),
    Empty,
}

#[derive(Debug, Clone)]
pub struct ExtractConfig<'a> {
    pub sentinel: &'a str,
    pub open_tag: &'a str,
    pub close_tag: &'a str,
    pub max_chars: usize,
    pub fallback_tail_lines: usize,
}

pub fn extract(snapshot: &str, cfg: &ExtractConfig) -> AnswerExtraction {
    if snapshot.is_empty() {
        return AnswerExtraction::Empty;
    }
    // Sentinel may or may not be present; we only need it to bound the FallbackTail.
    let bounded = match snapshot.find(cfg.sentinel) {
        Some(idx) => &snapshot[..idx],
        None => snapshot,
    };
    if let Some(o) = bounded.find(cfg.open_tag) {
        let after = o + cfg.open_tag.len();
        if let Some(rel_c) = bounded[after..].find(cfg.close_tag) {
            let body = &bounded[after..after + rel_c];
            return AnswerExtraction::TagDelimited(truncate(body, cfg.max_chars));
        }
    }
    // Fallback: last N lines of bounded section.
    let lines: Vec<&str> = bounded.lines().collect();
    let n = cfg.fallback_tail_lines.min(lines.len());
    if n == 0 {
        return AnswerExtraction::Empty;
    }
    let tail = lines[lines.len() - n..].join("\n");
    if tail.trim().is_empty() {
        AnswerExtraction::Empty
    } else {
        AnswerExtraction::FallbackTail(truncate(&tail, cfg.max_chars))
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_chars * 4);
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars { break; }
        out.push(c);
    }
    out
}
```

Add `pub mod answer;` to `lib.rs`. The `pipeline` submodule is filled in by Task 17 (stub it with `pub fn placeholder() {}` for now).

- [ ] **Step 2: Unit tests**

`crates/qa-service/tests/answer_extract.rs`:
```rust
use qa_service::answer::{extract, AnswerExtraction, ExtractConfig};

fn cfg<'a>() -> ExtractConfig<'a> {
    ExtractConfig {
        sentinel: "<<TOTSUKA_DONE>>",
        open_tag: "<answer>",
        close_tag: "</answer>",
        max_chars: 100,
        fallback_tail_lines: 3,
    }
}

#[test]
fn extracts_tag_delimited_content() {
    let snap = "noise\n<answer>here is the answer</answer>\n<<TOTSUKA_DONE>>\ntail";
    assert_eq!(
        extract(snap, &cfg()),
        AnswerExtraction::TagDelimited("here is the answer".into())
    );
}

#[test]
fn falls_back_to_tail_when_no_tags() {
    let snap = "noise\nline-a\nline-b\nline-c\n<<TOTSUKA_DONE>>\nignored";
    match extract(snap, &cfg()) {
        AnswerExtraction::FallbackTail(s) => {
            assert!(s.contains("line-a") || s.contains("line-b") || s.contains("line-c"));
            assert!(!s.contains("ignored"));
        }
        other => panic!("expected FallbackTail, got {other:?}"),
    }
}

#[test]
fn truncates_long_answer_at_max_chars() {
    let body = "x".repeat(500);
    let snap = format!("<answer>{body}</answer><<TOTSUKA_DONE>>");
    match extract(&snap, &cfg()) {
        AnswerExtraction::TagDelimited(s) => assert_eq!(s.chars().count(), 100),
        other => panic!("expected TagDelimited, got {other:?}"),
    }
}

#[test]
fn returns_empty_for_empty_snapshot() {
    assert_eq!(extract("", &cfg()), AnswerExtraction::Empty);
}

#[test]
fn returns_empty_when_no_lines_and_no_tags() {
    assert_eq!(extract("\n\n\n", &cfg()), AnswerExtraction::Empty);
}

#[test]
fn utf8_safe_truncate() {
    let body = "あ".repeat(200); // 200 chars, 600 bytes
    let snap = format!("<answer>{body}</answer><<TOTSUKA_DONE>>");
    match extract(&snap, &cfg()) {
        AnswerExtraction::TagDelimited(s) => assert_eq!(s.chars().count(), 100),
        other => panic!("got {other:?}"),
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test answer_extract
```
Expected: 6 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): answer extract (sentinel + tag + utf8-safe truncate)"
```

---

### Task 17: Answer pipeline (spawn-or-reuse + poll + extract + post)

**Files:**
- Create: `crates/qa-service/src/answer/pipeline.rs` (replace placeholder)

**Interfaces:**
- Produces:
  - `pub enum AnswerOutcome { Posted { ts: String }, Truncated { ts: String }, SpawnFailed(String), ExtractFallback { ts: String } }`
  - `pub struct AnswerInput { pub channel: String, pub user: String, pub thread_ts: String /* canonical; for top-level msg = msg.ts */, pub question: String, pub repo: String, pub mode: AnswerMode }`
  - `pub struct AnswerCtx { pub adapter: Arc<dyn AdapterClient>, pub slack: Arc<dyn SlackClient>, pub thread_map: Arc<ThreadMapRepo>, pub clock: Arc<dyn Clock>, pub answer_cfg: AnswerSection /* from totsuka_config */, pub system_prompt_template: String }`
  - `pub async fn handle_answer(ctx: &AnswerCtx, input: AnswerInput) -> Result<AnswerOutcome, QaError>` — fetches existing mapping → if present, `adapter.send(terminal_id, question)`; if absent, `adapter.spawn(...)` with `task_id = format!("qa-{}", thread_ts)`, `phase = "answer"`, `attempt = 0`, branch = `"qa/" + sanitize(thread_ts)`, argv = `[system_prompt_template]` (interpolated with sentinel/tag values), env = empty. Then polls `adapter.read(agent_id, 0)` at `poll_interval_ms`, watching revision stability + sentinel; on done → extract → post via slack; touch thread_map.

- [ ] **Step 1: pipeline.rs**

`crates/qa-service/src/answer/pipeline.rs`:
```rust
//! Answer flow: spawn-or-reuse agent → poll snapshot → extract → post.
//! Concurrency-gated outside this module (Semaphore in the dispatch loop).

use std::sync::Arc;
use std::time::{Duration, Instant};
use totsuka_config::schema::AnswerSection;
use totsuka_core::Clock;

use super::extract::{extract, AnswerExtraction, ExtractConfig};
use crate::adapter_client::{AdapterClient, SpawnReq};
use crate::error::QaError;
use crate::mode::AnswerMode;
use crate::slack::{SlackClient, SlackPostResult};
use crate::thread_map::{ThreadMapRepo, ThreadMapping};

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerOutcome {
    Posted { ts: String },
    Truncated { ts: String },
    SpawnFailed(String),
    ExtractFallback { ts: String },
}

pub struct AnswerInput {
    pub channel: String,
    pub user: String,
    pub thread_ts: String,
    pub question: String,
    pub repo: String,
    pub mode: AnswerMode,
}

pub struct AnswerCtx {
    pub adapter: Arc<dyn AdapterClient>,
    pub slack: Arc<dyn SlackClient>,
    pub thread_map: Arc<ThreadMapRepo>,
    pub clock: Arc<dyn Clock>,
    pub answer_cfg: AnswerSection,
    pub system_prompt_template: String,
}

pub async fn handle_answer(
    ctx: &AnswerCtx,
    input: AnswerInput,
) -> Result<AnswerOutcome, QaError> {
    // 1. Resolve or spawn the agent.
    let existing = ctx.thread_map.get(&input.thread_ts).await?;
    let agent_id = match existing {
        Some(m) => {
            // Send the new message to the existing agent.
            ctx.adapter.send(&m.terminal_id, &input.question).await?;
            ctx.thread_map.touch(&input.thread_ts).await?;
            m.terminal_id
        }
        None => {
            let argv = vec![interpolate_prompt(
                &ctx.system_prompt_template,
                &ctx.answer_cfg,
            )];
            let req = SpawnReq {
                task_id: format!("qa-{}", &input.thread_ts),
                phase: "answer".into(),
                attempt: 0,
                repo: input.repo.clone(),
                branch: format!("qa/{}", sanitize_branch(&input.thread_ts)),
                argv,
                env: Default::default(),
            };
            let res = match ctx.adapter.spawn(req).await {
                Ok(r) => r,
                Err(e) => return Ok(AnswerOutcome::SpawnFailed(e.to_string())),
            };
            // Send the question once the agent is up.
            ctx.adapter.send(&res.terminal_id, &input.question).await?;
            let now = ctx.clock.now();
            ctx.thread_map.upsert(&ThreadMapping {
                thread_ts: input.thread_ts.clone(),
                terminal_id: res.terminal_id.clone(),
                repo: input.repo.clone(),
                last_activity_at: now,
                created_at: now,
            }).await?;
            res.terminal_id
        }
    };

    // 2. Poll for output until sentinel / quiescence / timeout.
    let cfg = &ctx.answer_cfg;
    let mut prev_revision: u64 = 0;
    let mut last_change = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(cfg.answer_timeout_secs);
    let stable = Duration::from_secs(cfg.stable_revision_secs);
    let mut latest_snapshot = String::new();
    let mut hit_timeout = false;
    loop {
        if Instant::now() >= deadline {
            hit_timeout = true;
            break;
        }
        let snap = ctx.adapter.read(&agent_id, 0).await?;
        if snap.revision != prev_revision {
            prev_revision = snap.revision;
            last_change = Instant::now();
            latest_snapshot = snap.text.clone();
        }
        if latest_snapshot.contains(&cfg.sentinel) {
            break;
        }
        if last_change.elapsed() >= stable && !latest_snapshot.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(cfg.poll_interval_ms)).await;
    }

    // 3. Extract.
    let extract_cfg = ExtractConfig {
        sentinel: &cfg.sentinel,
        open_tag: &cfg.answer_open_tag,
        close_tag: &cfg.answer_close_tag,
        max_chars: 40_000,
        fallback_tail_lines: 40,
    };
    let extraction = extract(&latest_snapshot, &extract_cfg);
    let (text, kind) = match extraction {
        AnswerExtraction::TagDelimited(s) => (s, "tag"),
        AnswerExtraction::FallbackTail(s) => {
            tracing::warn!(
                thread_ts = %input.thread_ts,
                "answer tag missing; posting fallback tail"
            );
            (s, "fallback")
        }
        AnswerExtraction::Empty => {
            tracing::warn!(thread_ts = %input.thread_ts, "no answer text extracted");
            (String::from("(no answer produced)"), "empty")
        }
    };

    // 4. Post.
    let SlackPostResult { ts } = match input.mode {
        AnswerMode::Auto => ctx.slack
            .post_message(&input.channel, Some(&input.thread_ts), &text)
            .await?,
        AnswerMode::Delegated => {
            ctx.slack
                .post_ephemeral(&input.channel, &input.user, Some(&input.thread_ts), &text)
                .await?;
            SlackPostResult { ts: format!("ephemeral-{}", input.thread_ts) }
        }
    };

    ctx.thread_map.touch(&input.thread_ts).await?;

    Ok(match (hit_timeout, kind) {
        (true, _) => AnswerOutcome::Truncated { ts },
        (_, "fallback") => AnswerOutcome::ExtractFallback { ts },
        _ => AnswerOutcome::Posted { ts },
    })
}

fn interpolate_prompt(template: &str, cfg: &AnswerSection) -> String {
    template
        .replace("{sentinel}", &cfg.sentinel)
        .replace("{open_tag}", &cfg.answer_open_tag)
        .replace("{close_tag}", &cfg.answer_close_tag)
}

fn sanitize_branch(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}
```

- [ ] **Step 2: Build check + commit**

```bash
cargo check -p qa-service
```
Expected: clean.

The pipeline is exercised by Tasks 24-25 e2e tests. No unit test here — `handle_answer` is a coordinator function whose value is in end-to-end correctness, not isolated branches.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): answer pipeline (spawn-or-reuse + poll + extract + post)"
```

---

### Task 18: GitHub Inbox creator (GraphQL addProjectV2DraftIssue, injection-safe)

**Files:**
- Create: `crates/qa-service/src/gh_inbox.rs`
- Create: `crates/qa-service/tests/gh_inbox.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct GhInboxClient { client, endpoint, token: Secret<String> }`
  - `pub fn new(token: Secret<String>, override_endpoint: Option<String>) -> Self` (default `https://api.github.com/graphql`)
  - `pub async fn create_draft(project_node_id: &str, title: &str, body: &str) -> Result<String /* item id */, QaError>` — variables-based GraphQL `addProjectV2DraftIssue`. Same const+variables pattern as orchestrator's writeback after PR #4.

- [ ] **Step 1: Implement**

`crates/qa-service/src/gh_inbox.rs`:
```rust
//! Create a DraftIssue in the GitHub Project Inbox column.
//!
//! GraphQL injection prevention: project_node_id, title, body MUST go through
//! `variables`, never `format!`-interpolated into the document — same shape
//! as orchestrator's gh_writeback after PR #4 and github-watcher's
//! gh_client/graphql.rs.

use crate::error::QaError;
use reqwest::Client;
use serde_json::{json, Value};
use totsuka_core::Secret;

const MUTATION: &str = r#"
    mutation($input: AddProjectV2DraftIssueInput!) {
      addProjectV2DraftIssue(input: $input) {
        projectItem { id }
      }
    }
"#;

pub struct GhInboxClient {
    client: Client,
    endpoint: String,
    token: Secret<String>,
}

impl GhInboxClient {
    pub fn new(token: Secret<String>, override_endpoint: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .user_agent("totsuka-qa-service")
                .build()
                .expect("reqwest client"),
            endpoint: override_endpoint
                .unwrap_or_else(|| "https://api.github.com/graphql".into()),
            token,
        }
    }

    pub async fn create_draft(
        &self,
        project_node_id: &str,
        title: &str,
        body: &str,
    ) -> Result<String, QaError> {
        let req_body = json!({
            "query": MUTATION,
            "variables": {
                "input": {
                    "projectId": project_node_id,
                    "title":     title,
                    "body":      body,
                }
            }
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(self.token.expose())
            .json(&req_body)
            .send()
            .await?;
        let v: Value = resp.json().await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(QaError::GraphQl(errors[0].to_string()));
            }
        }
        v.pointer("/data/addProjectV2DraftIssue/projectItem/id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| QaError::GraphQl(format!("addProjectV2DraftIssue: missing item id: {v}")))
    }
}
```

Add `pub mod gh_inbox;` to `lib.rs`.

- [ ] **Step 2: Injection regression test**

`crates/qa-service/tests/gh_inbox.rs`:
```rust
use qa_service::gh_inbox::GhInboxClient;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

#[tokio::test]
async fn malicious_inputs_land_in_variables_not_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body_resp = r#"{"data":{"addProjectV2DraftIssue":{"projectItem":{"id":"PVTI_OK"}}}}"#;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut cl = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" { break; }
            if let Some(v) = line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")) {
                cl = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0u8; cl];
        reader.read_exact(&mut buf).await.unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body_resp.len(),
            body_resp,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        buf
    });

    let client = GhInboxClient::new(
        Secret::new("tok".into()),
        Some(format!("http://{addr}/graphql")),
    );
    let evil_id    = r#""}}}) { __typename } mutation Pwn { __typename "#;
    let evil_title = r#"</title><script>alert(1)</script>"#;
    let id = client.create_draft(evil_id, evil_title, "body").await.unwrap();
    assert_eq!(id, "PVTI_OK");

    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    let q = body["query"].as_str().expect("query string present");
    assert!(!q.contains("__typename"), "query contaminated: {q}");
    assert!(!q.contains("script"),     "query contaminated: {q}");
    assert_eq!(body["variables"]["input"]["projectId"], evil_id);
    assert_eq!(body["variables"]["input"]["title"],     evil_title);
    assert_eq!(body["variables"]["input"]["body"],      "body");
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p qa-service --test gh_inbox
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): GitHub Inbox creator (addProjectV2DraftIssue, variables-based)"
```

---

### Task 19: Reaction handler (reaction_added → GitHub Inbox)

**Files:**
- Create: `crates/qa-service/src/reaction.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct ReactionCtx { pub slack: Arc<dyn SlackClient>, pub inbox: Arc<GhInboxClient>, pub project_node_id: String, pub trigger_emoji: String }`
  - `pub async fn handle_reaction(ctx: &ReactionCtx, channel: &str, item_ts: &str, reaction: &str) -> Result<Option<String>, QaError>` — if `reaction == trigger_emoji`, fetches the reacted-to message via `slack.replies` (single-message fetch), builds title from first 80 chars + body from full text + Slack link, calls `inbox.create_draft`, returns `Some(item_id)`; else returns `Ok(None)`.

- [ ] **Step 1: Implement + tests**

`crates/qa-service/src/reaction.rs`:
```rust
//! Slack reaction_added → GitHub Project Inbox DraftIssue.
//! Only fires when reaction == [qa_service].reaction_trigger.

use crate::error::QaError;
use crate::gh_inbox::GhInboxClient;
use crate::slack::SlackClient;
use std::sync::Arc;

pub struct ReactionCtx {
    pub slack: Arc<dyn SlackClient>,
    pub inbox: Arc<GhInboxClient>,
    pub project_node_id: String,
    pub trigger_emoji: String,
}

pub async fn handle_reaction(
    ctx: &ReactionCtx,
    channel: &str,
    item_ts: &str,
    reaction: &str,
) -> Result<Option<String>, QaError> {
    if reaction != ctx.trigger_emoji {
        return Ok(None);
    }
    let msgs = ctx.slack.replies(channel, item_ts).await?;
    let original = msgs.iter().find(|m| m.ts == item_ts).ok_or_else(|| {
        QaError::Slack(format!("reacted message {item_ts} not found in {channel}"))
    })?;
    let title: String = original.text.chars().take(80).collect();
    let body = format!(
        "{}\n\nSource: Slack channel {}, ts {} (reacted by user)\n",
        original.text, channel, item_ts
    );
    let id = ctx
        .inbox
        .create_draft(&ctx.project_node_id, &title, &body)
        .await?;
    Ok(Some(id))
}
```

Add `pub mod reaction;` to `lib.rs`.

(No unit test for this thin coordinator — the `gh_inbox` regression test covers the injection path; e2e or future integration covers the slack→inbox wiring.)

- [ ] **Step 2: Build + commit**

```bash
cargo check -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): reaction handler (Slack reaction → GitHub Inbox draft)"
```

---

### Task 20: Pane sweeper (idle close)

**Files:**
- Create: `crates/qa-service/src/sweeper.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub async fn run_sweeper(thread_map: Arc<ThreadMapRepo>, adapter: Arc<dyn AdapterClient>, clock: Arc<dyn Clock>, idle_ttl: chrono::Duration, tick_secs: u64, shutdown: CancellationToken) -> Result<(), QaError>` — every `tick_secs`, calls `thread_map.list_idle(now - idle_ttl)`; for each idle mapping, calls `adapter.stop(terminal_id, repo, branch_placeholder)` and `thread_map.delete(thread_ts)`. The `branch_placeholder` is `"qa/"` + sanitised thread_ts (matches the spawn convention from Task 17); the adapter's `stop` impl ignores `repo`/`branch` in this flow (DELETE /v1/agents/:id needs only the agent id).

- [ ] **Step 1: Implement**

`crates/qa-service/src/sweeper.rs`:
```rust
//! Idle pane sweeper. spec §8.4 — close panes whose thread has been silent
//! for [qa_service.answer].pane_idle_ttl_secs.

use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use crate::thread_map::ThreadMapRepo;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use totsuka_core::Clock;

pub async fn run_sweeper(
    thread_map: Arc<ThreadMapRepo>,
    adapter: Arc<dyn AdapterClient>,
    clock: Arc<dyn Clock>,
    idle_ttl: chrono::Duration,
    tick_secs: u64,
    shutdown: CancellationToken,
) -> Result<(), QaError> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let cutoff = clock.now() - idle_ttl;
                let idle = match thread_map.list_idle(cutoff).await {
                    Ok(v) => v,
                    Err(e) => { tracing::warn!(error=%e, "sweeper list_idle failed"); continue; }
                };
                for m in idle {
                    let branch = format!("qa/{}", sanitize(&m.thread_ts));
                    if let Err(e) = adapter.stop(&m.terminal_id, &m.repo, &branch).await {
                        tracing::warn!(error=%e, thread_ts=%m.thread_ts, "sweeper stop failed");
                        continue;
                    }
                    if let Err(e) = thread_map.delete(&m.thread_ts).await {
                        tracing::warn!(error=%e, thread_ts=%m.thread_ts, "sweeper delete failed");
                    }
                }
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}
```

Add `pub mod sweeper;` to `lib.rs`.

- [ ] **Step 2: Build + commit**

```bash
cargo check -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): pane idle sweeper"
```

---

### Task 21: Restart recovery (reconcile thread mapping ↔ adapter agent list)

**Files:**
- Create: `crates/qa-service/src/recovery.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct RecoveryReport { pub kept: usize, pub mapping_orphans_deleted: usize, pub pane_orphans_closed: usize }`
  - `pub async fn reconcile(thread_map: &ThreadMapRepo, adapter: &dyn AdapterClient) -> Result<RecoveryReport, QaError>` — on startup:
    1. `let agents = adapter.list().await?;` → set of `terminal_id`.
    2. `let mappings = thread_map.list_all().await?;`
    3. For each mapping: if agent exists → keep; else → `thread_map.delete(thread_ts)` (mapping orphan).
    4. For each agent not referenced by any mapping AND whose label starts with `"totsuka:qa-"` → `adapter.stop(agent_id, "", "")` (pane orphan).
    5. Return RecoveryReport for telemetry.

- [ ] **Step 1: Implement**

`crates/qa-service/src/recovery.rs`:
```rust
//! On startup, reconcile qa_thread_agent vs agent-adapter's agent.list:
//! * mapping ∧ agent → keep
//! * mapping ∧ ¬agent → DELETE mapping (next thread message will spawn fresh)
//! * ¬mapping ∧ agent (qa-labelled) → close pane (avoid leak)
//!
//! See spec §8.4 「再起動時のリカバリ」.

use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use crate::thread_map::ThreadMapRepo;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub kept: usize,
    pub mapping_orphans_deleted: usize,
    pub pane_orphans_closed: usize,
}

pub async fn reconcile(
    thread_map: &ThreadMapRepo,
    adapter: &dyn AdapterClient,
) -> Result<RecoveryReport, QaError> {
    let agents = adapter.list().await?;
    let mappings = thread_map.list_all().await?;

    let alive: HashSet<String> = agents.iter().map(|a| a.terminal_id.clone()).collect();
    let mut kept = 0usize;
    let mut mapping_orphans_deleted = 0usize;

    let mapped: HashSet<String> = mappings.iter().map(|m| m.terminal_id.clone()).collect();

    for m in &mappings {
        if alive.contains(&m.terminal_id) {
            kept += 1;
        } else {
            thread_map.delete(&m.thread_ts).await?;
            mapping_orphans_deleted += 1;
        }
    }

    let mut pane_orphans_closed = 0usize;
    for a in &agents {
        if !a.label.starts_with("totsuka:qa-") {
            // Not a qa-service agent — leave it alone.
            continue;
        }
        if mapped.contains(&a.terminal_id) {
            continue;
        }
        if let Err(e) = adapter.stop(&a.agent_id, "", "").await {
            tracing::warn!(error=%e, agent_id=%a.agent_id, "recovery: pane orphan close failed");
            continue;
        }
        pane_orphans_closed += 1;
    }

    tracing::info!(
        kept,
        mapping_orphans_deleted,
        pane_orphans_closed,
        "qa-service recovery complete"
    );
    Ok(RecoveryReport { kept, mapping_orphans_deleted, pane_orphans_closed })
}
```

Add `pub mod recovery;` to `lib.rs`.

- [ ] **Step 2: Build + commit**

```bash
cargo check -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): restart recovery (reconcile thread_map vs agent.list)"
```

---

### Task 22: Slack catchup on startup

**Files:**
- Create: `crates/qa-service/src/catchup.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct CatchupCfg { pub channels: Vec<String>, pub oldest_ts: Option<String> /* from catchup_cursor or None */, pub limit: u32 }`
  - `pub async fn run_catchup_once(slack: &dyn SlackClient, pool: &PgPool, channels: &[String], default_oldest: Option<String>) -> Result<usize, QaError>` — for each channel, fetches `conversation_history(channel, oldest_or_cursor, limit=100)`, persists max `ts` per channel into `catchup_cursor (source='slack', scope='channel:<ID>')`. Returns total messages observed.
  - Skips messages already processed by relying on a `cursor::set` call (mirrors github-watcher's per-source `catchup_cursor` use).
  - **Note**: catchup does NOT replay through the answer pipeline (that risks duplicating answers). It only advances the cursor + logs unread counts for observability + persistence.

- [ ] **Step 1: Implement**

`crates/qa-service/src/catchup.rs`:
```rust
//! Slack startup catchup. Reads recent history per channel, advances the
//! per-channel cursor in catchup_cursor (source='slack'), and logs counts so
//! operators can see how much was missed. We deliberately do NOT replay
//! messages into the answer pipeline — that would double-answer questions
//! that were already handled before restart.

use crate::error::QaError;
use crate::slack::SlackClient;
use sqlx::PgPool;

pub async fn run_catchup_once(
    slack: &dyn SlackClient,
    pool: &PgPool,
    channels: &[String],
    default_oldest: Option<String>,
) -> Result<usize, QaError> {
    let mut total = 0usize;
    for channel in channels {
        let scope = format!("channel:{channel}");
        let cursor = get_cursor(pool, &scope).await?;
        let oldest = cursor.or_else(|| default_oldest.clone());
        let msgs = slack.conversation_history(channel, oldest.as_deref(), 100).await?;
        if let Some(max_ts) = msgs.iter().map(|m| m.ts.clone()).max() {
            set_cursor(pool, &scope, &max_ts).await?;
        }
        tracing::info!(channel, observed = msgs.len(), "slack catchup");
        total += msgs.len();
    }
    Ok(total)
}

async fn get_cursor(pool: &PgPool, scope: &str) -> Result<Option<String>, QaError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT cursor FROM catchup_cursor WHERE source = 'slack' AND scope = $1",
    )
    .bind(scope)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

async fn set_cursor(pool: &PgPool, scope: &str, cursor: &str) -> Result<(), QaError> {
    sqlx::query(
        "INSERT INTO catchup_cursor (source, scope, cursor, updated_at)
              VALUES ('slack', $1, $2, now())
              ON CONFLICT (source, scope) DO UPDATE
                SET cursor = EXCLUDED.cursor, updated_at = now()",
    )
    .bind(scope)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}
```

Add `pub mod catchup;` to `lib.rs`.

- [ ] **Step 2: Build + commit**

```bash
cargo check -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): Slack catchup-once on startup (cursor-only, no replay)"
```

---

### Task 23: Lifecycle probes + signals + UDS listener

**Files:**
- Create: `crates/qa-service/src/lifecycle.rs`
- Create: `crates/qa-service/src/listener.rs`
- Modify: `crates/qa-service/src/lib.rs`

**Interfaces:**
- Produces (`lifecycle.rs`):
  - `pub async fn probe_db(pool: &PgPool, health: &HealthState)` — `SELECT 1`
  - `pub async fn probe_adapter(adapter: &dyn AdapterClient, health: &HealthState)` — `list()` for connectivity
  - `pub async fn probe_repo_descriptions(config: &Config, health: &HealthState)` — every `[agent_adapter.repos.*]` MUST have non-empty `description` (spec §8.4); empty → readyz NG.
  - `pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), QaError>` — SIGTERM/SIGINT + 15s grace.
- Produces (`listener.rs`):
  - `pub async fn bind_uds(path: &Path) -> Result<UnixListener, QaError>` + `pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> Result<(), QaError>` + `pub fn resolve_uds_path(raw: &str) -> PathBuf` — mirrors orchestrator's `listener.rs`.

- [ ] **Step 1: lifecycle.rs**

`crates/qa-service/src/lifecycle.rs`:
```rust
use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use sqlx::PgPool;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use totsuka_config::Config;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_)  => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_adapter(adapter: &dyn AdapterClient, health: &HealthState) {
    match adapter.list().await {
        Ok(_)  => health.set_check("adapter", "ok").await,
        Err(e) => health.set_check("adapter", &format!("fail: {e}")).await,
    }
}

pub async fn probe_repo_descriptions(config: &Config, health: &HealthState) {
    let missing: Vec<&String> = config
        .agent_adapter
        .repos
        .iter()
        .filter(|(_, r)| r.description.trim().is_empty())
        .map(|(name, _)| name)
        .collect();
    if missing.is_empty() {
        health.set_check("repo_descriptions", "ok").await;
    } else {
        let msg = format!("fail: empty description for: {:?}", missing);
        health.set_check("repo_descriptions", &msg).await;
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), QaError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| QaError::Internal(format!("install SIGTERM: {e}")))?;
    let mut int = signal(SignalKind::interrupt())
        .map_err(|e| QaError::Internal(format!("install SIGINT: {e}")))?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received; initiating graceful shutdown"),
        _ = int.recv()  => tracing::info!("SIGINT received; initiating graceful shutdown"),
    }
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
```

- [ ] **Step 2: listener.rs (UDS — qa-service uses UDS per spec §7)**

`crates/qa-service/src/listener.rs`:
```rust
use crate::error::QaError;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;
use tower::Service;

pub async fn bind_uds(path: &Path) -> Result<UnixListener, QaError> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| QaError::Internal(format!("remove old uds: {e}")))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| QaError::Internal(format!("create dir: {e}")))?;
    }
    UnixListener::bind(path).map_err(|e| QaError::Internal(format!("bind uds: {e}")))
}

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> Result<(), QaError> {
    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| QaError::Internal(format!("accept: {e}")))?;
        let io = TokioIo::new(stream);
        let tower_service = svc
            .call(())
            .await
            .map_err(|e| QaError::Internal(format!("svc.call: {e}")))?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
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

Add `pub mod lifecycle; pub mod listener;` to `lib.rs`.

- [ ] **Step 3: Build + commit**

```bash
cargo check -p qa-service
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): lifecycle probes + signals + UDS healthz/readyz listener"
```

---

### Task 24: Main wiring (socket loop + dispatch + sweeper + listener + signals)

**Files:**
- Modify: `crates/qa-service/src/main.rs`

**Interfaces:**
- Produces: `main()` that loads config, opens PgPool, runs schema check + repo description probe, builds Classifier via factory, builds RepoSelector, builds GhInboxClient, builds HyperlocalAdapter, builds HttpSlackClient, builds ThreadMapRepo, runs `recovery::reconcile`, spawns the Socket Mode loop, the dispatch worker (mpsc receiver → answer pipeline gated by Semaphore), the sweeper, the UDS listener, and the signals task. Joins on first to exit.

- [ ] **Step 1: Replace main.rs**

`crates/qa-service/src/main.rs`:
```rust
use std::sync::Arc;
use std::time::Duration;

use qa_service::adapter_client::{AdapterClient, HyperlocalAdapter};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::catchup::run_catchup_once;
use qa_service::classifier::{self, ClassifyRequest, RepoCandidate};
use qa_service::gh_inbox::GhInboxClient;
use qa_service::lifecycle::{probe_adapter, probe_db, probe_repo_descriptions, wait_for_signals};
use qa_service::listener::{bind_uds, resolve_uds_path, serve_uds};
use qa_service::mode::AnswerMode;
use qa_service::question_filter::{QuestionFilter, Trigger};
use qa_service::reaction::{handle_reaction, ReactionCtx};
use qa_service::recovery::reconcile;
use qa_service::repo_select::{RepoSelector, SelectOutcome};
use qa_service::schema_check::check_schema_version;
use qa_service::slack::{
    envelope::SlackEvent,
    socket::{run_socket_loop, SocketModeConfig},
    HttpSlackClient, SlackClient,
};
use qa_service::sweeper::run_sweeper;
use qa_service::thread_map::ThreadMapRepo;
use qa_service::QaApp;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + tracing
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);
    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard =
        totsuka_telemetry::init_tracing(&state_dir, "qa-service", &config.totsuka.log_level);

    // 2. DB + schema
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            config.postgres.user,
            config.postgres.password.expose(),
            config.postgres.host,
            config.postgres.port,
            config.postgres.database,
        )
    });
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await?;
    check_schema_version(&pool).await?;

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    // 3. Adapter + Slack + Classifier + Inbox + RepoSelector
    let adapter_path = resolve_uds_path(&config.qa_service.adapter_uds);
    let adapter: Arc<dyn AdapterClient> = Arc::new(HyperlocalAdapter::new(adapter_path));

    let slack: Arc<dyn SlackClient> =
        Arc::new(HttpSlackClient::new(config.qa_service.slack_bot_token.clone(), None));

    let classifier_arc = classifier::build(&config.qa_service.classifier)?;

    let inbox = Arc::new(GhInboxClient::new(
        config.github_watcher.github_token.clone(),
        None,
    ));

    let selector = Arc::new(RepoSelector::from_cfg(
        config.qa_service.classifier.confidence_threshold,
        &config.qa_service.classifier.on_low_confidence,
    )?);

    let default_mode = AnswerMode::parse(&config.qa_service.default_mode)?;

    // 4. Probes + ready
    let health = HealthState::new();
    probe_db(&pool, &health).await;
    probe_adapter(adapter.as_ref(), &health).await;
    probe_repo_descriptions(&config, &health).await;
    health.set_ready(true).await;

    // 5. Recovery
    let _report = reconcile(thread_map.as_ref(), adapter.as_ref()).await?;

    // 6. Catchup (best-effort)
    if !config.qa_service.catchup_channels.is_empty() {
        let _ = run_catchup_once(
            slack.as_ref(),
            &pool,
            &config.qa_service.catchup_channels,
            None,
        )
        .await;
    }

    // 7. Socket Mode loop → mpsc → dispatch worker
    let (event_tx, mut event_rx) = mpsc::channel::<SlackEvent>(128);
    let shutdown = CancellationToken::new();

    let socket_h = {
        let cfg = SocketModeConfig::new(config.qa_service.slack_app_token.clone());
        let http = Arc::new(reqwest::Client::builder()
            .user_agent("totsuka-qa-service")
            .build()?);
        let s = shutdown.clone();
        tokio::spawn(async move {
            run_socket_loop(cfg, http, event_tx, s).await
        })
    };

    let semaphore = Arc::new(Semaphore::new(
        config.qa_service.answer.max_concurrent_answers as usize,
    ));

    let project_node_id = {
        let token = config.github_watcher.github_token.clone();
        let owner = config.github.project_owner.clone();
        let number = config.github.project_number;
        let http = reqwest::Client::builder().user_agent("totsuka-qa-service").build()?;
        let body = serde_json::json!({
            "query": "query($login:String!,$number:Int!){user(login:$login){projectV2(number:$number){id}}}",
            "variables": { "login": owner, "number": number },
        });
        let v: serde_json::Value = http
            .post("https://api.github.com/graphql")
            .bearer_auth(token.expose())
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        v.pointer("/data/user/projectV2/id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("project node id resolve failed"))?
            .to_string()
    };

    let dispatch_h = {
        let adapter = adapter.clone();
        let slack = slack.clone();
        let classifier_arc = classifier_arc.clone();
        let selector = selector.clone();
        let inbox = inbox.clone();
        let thread_map = thread_map.clone();
        let clock = clock.clone();
        let config = config.clone();
        let semaphore = semaphore.clone();
        let project_node_id = project_node_id.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            let filter = QuestionFilter::new(
                config.qa_service.allowed_user_ids.clone(),
                std::env::var("SLACK_BOT_USER_ID").unwrap_or_default(),
            );
            // The `[agent_adapter.repos.HASH_KEY]` map key IS the `owner/repo`
            // string used by both the classifier and the adapter's spawn call —
            // NOT `RepoSection.repo_path` (which is a local filesystem path).
            let candidates: Vec<RepoCandidate> = config
                .agent_adapter
                .repos
                .iter()
                .map(|(owner_repo, r)| RepoCandidate {
                    repo: owner_repo.clone(),
                    description: r.description.clone(),
                })
                .collect();
            let answer_ctx = AnswerCtx {
                adapter: adapter.clone(),
                slack: slack.clone(),
                thread_map: thread_map.clone(),
                clock: clock.clone(),
                answer_cfg: config.qa_service.answer.clone(),
                system_prompt_template:
                    "Answer the user question. Wrap your answer in {open_tag}…{close_tag} and end \
                     with {sentinel}. Use Slack mrkdwn formatting (*bold*, _italic_, ```code```)."
                        .to_string(),
            };
            let reaction_ctx = ReactionCtx {
                slack: slack.clone(),
                inbox: inbox.clone(),
                project_node_id,
                trigger_emoji: config.qa_service.reaction_trigger.clone(),
            };
            loop {
                tokio::select! {
                    _ = s.cancelled() => break,
                    Some(ev) = event_rx.recv() => {
                        match ev {
                            SlackEvent::Message(m) => {
                                let thread_key = m.thread_ts.clone().unwrap_or_else(|| m.ts.clone());
                                let existing = thread_map.get(&thread_key).await.unwrap_or(None).is_some();
                                let trig = filter.evaluate(&m, existing);
                                if trig == Trigger::None { continue; }
                                let req = ClassifyRequest {
                                    question: m.text.clone(),
                                    thread_context: None,
                                    candidates: candidates.clone(),
                                };
                                let resp = match classifier_arc.classify(req).await {
                                    Ok(r) => r,
                                    Err(e) => { tracing::warn!(error=%e, "classify failed"); continue; }
                                };
                                let outcome = selector.decide(&resp);
                                let (repo, mode) = match outcome {
                                    SelectOutcome::HighConfidence { repo, .. } => (repo, default_mode),
                                    SelectOutcome::LowConfidenceUseTop1 { repo, .. } => (repo, default_mode),
                                    SelectOutcome::LowConfidenceDelegated { .. }
                                    | SelectOutcome::LowConfidenceRefused => {
                                        let _ = slack.post_ephemeral(
                                            &m.channel, &m.user, Some(&thread_key),
                                            "リポジトリを特定できませんでした。明示的に指定してください。",
                                        ).await;
                                        continue;
                                    }
                                };
                                let permit = semaphore.clone().acquire_owned().await.expect("permit");
                                let input = AnswerInput {
                                    channel: m.channel.clone(),
                                    user: m.user.clone(),
                                    thread_ts: thread_key,
                                    question: m.text.clone(),
                                    repo,
                                    mode,
                                };
                                let ctx_cloned = answer_ctx.clone();
                                tokio::spawn(async move {
                                    let _p = permit;
                                    if let Err(e) = handle_answer(&ctx_cloned, input).await {
                                        tracing::warn!(error=%e, "answer pipeline failed");
                                    }
                                });
                            }
                            SlackEvent::ReactionAdded { channel, item_ts, reaction, .. } => {
                                if let Err(e) =
                                    handle_reaction(&reaction_ctx, &channel, &item_ts, &reaction).await
                                {
                                    tracing::warn!(error=%e, "reaction handler failed");
                                }
                            }
                            SlackEvent::Other => {}
                        }
                    }
                }
            }
            Ok::<(), qa_service::error::QaError>(())
        })
    };

    let sweeper_h = {
        let adapter = adapter.clone();
        let thread_map = thread_map.clone();
        let clock = clock.clone();
        let ttl = chrono::Duration::seconds(config.qa_service.answer.pane_idle_ttl_secs as i64);
        let s = shutdown.clone();
        tokio::spawn(async move { run_sweeper(thread_map, adapter, clock, ttl, 60, s).await })
    };

    let listener_h = {
        let uds = resolve_uds_path(&config.qa_service.uds_path);
        let listener = bind_uds(&uds).await?;
        let router = totsuka_telemetry::http::router(health.clone())
            .layer(axum::middleware::from_fn(totsuka_telemetry::request_id::middleware));
        tokio::spawn(async move { serve_uds(listener, router).await })
    };

    let _signals = tokio::spawn(wait_for_signals(shutdown.clone()));
    let _app = QaApp::new(config.clone(), clock.clone());

    tokio::select! {
        r = socket_h   => { let _ = r?; },
        r = dispatch_h => { let _ = r?; },
        r = sweeper_h  => { let _ = r?; },
        r = listener_h => { let _ = r?; },
    }
    Ok(())
}
```

> **Note**: `AnswerCtx` needs `Clone`. Add `#[derive(Clone)]` to `AnswerCtx` in `crates/qa-service/src/answer/pipeline.rs`. Same for `ReactionCtx`. All inner fields are `Arc<...>` / `String` / config struct (already `Clone`).
>
> Also: the spawn block above expects `RepoSection.repo_path: Option<String>` to expose `owner/repo`. If `repo_path` does not match `owner/repo`, callers should provide an explicit `owner_repo` field on `RepoSection` in totsuka-config — but for the initial wiring, using `repo_path` as a stand-in is acceptable since both qa-service classifier and downstream adapter spawn agree on the same string.

- [ ] **Step 2: Build**

```bash
cargo build -p qa-service
```
Expected: succeeds.

- [ ] **Step 3: Commit**

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(qa-service): main wiring — socket loop + dispatch + sweeper + listener + signals"
```

---

### Task 25: e2e — high-confidence answer (classifier mock + adapter mock + slack mock)

**Files:**
- Create: `crates/qa-service/tests/e2e_high_conf_answer.rs`

**Interfaces:**
- Consumes: `MockClassifier`, `MockAdapter`, `MockSlackClient`, `ThreadMapRepo`, `handle_answer`.
- Produces: one e2e test that:
  - sets MockAdapter spawn → `terminal_id = "term_e2e_1"`, read → snapshot containing `<answer>OK</answer><<TOTSUKA_DONE>>`.
  - calls `handle_answer` with a new thread → asserts MockSlackClient `posts` has 1 entry with `text == "OK"` and `thread_ts == input.thread_ts`.
  - asserts `qa_thread_agent` row exists for `thread_ts` mapped to `term_e2e_1`.

- [ ] **Step 1: Implement**

`crates/qa-service/tests/e2e_high_conf_answer.rs`:
```rust
use qa_service::adapter_client::{AdapterClient, AgentSummary, MockAdapter, ReadRes, SpawnRes};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::mode::AnswerMode;
use qa_service::slack::{MockSlackClient, SlackClient};
use qa_service::thread_map::ThreadMapRepo;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_config::schema::AnswerSection;
use totsuka_core::SystemClock;

fn answer_cfg() -> AnswerSection {
    AnswerSection {
        sentinel: "<<TOTSUKA_DONE>>".into(),
        answer_open_tag: "<answer>".into(),
        answer_close_tag: "</answer>".into(),
        poll_interval_ms: 20,
        stable_revision_secs: 1,
        answer_timeout_secs: 5,
        pane_idle_ttl_secs: 1800,
        max_concurrent_answers: 4,
    }
}

#[tokio::test]
async fn high_conf_answer_spawns_polls_extracts_posts() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_1".into(),
        terminal_id: "term_e2e_1".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });
    adapter.set_list_response(vec![AgentSummary {
        agent_id: "agent_e2e_1".into(),
        terminal_id: "term_e2e_1".into(),
        label: "totsuka:qa-1:answer:0".into(),
    }]);

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    // Clean any prior state for this thread.
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }));

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "OK");                            // text
    assert_eq!(posts[0].1.as_deref(), Some(thread_ts.as_str())); // thread_ts

    let mapping = thread_map.get(&thread_ts).await.unwrap().unwrap();
    assert_eq!(mapping.terminal_id, "term_e2e_1");
    assert_eq!(mapping.repo, "acme/api");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();
}
```

> `AnswerCtx` needs `Clone` (see Task 24 note); ensure that derive is in place before this test compiles.

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service --test e2e_high_conf_answer
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(qa-service): e2e — high-confidence answer flow"
```

---

### Task 26: e2e — thread continuation (existing mapping → send, not spawn)

**Files:**
- Create: `crates/qa-service/tests/e2e_thread_continuation.rs`

**Interfaces:**
- Consumes: same as Task 25 but seeds a `qa_thread_agent` row before calling `handle_answer`.
- Produces: e2e test that asserts MockAdapter received `send` (not `spawn`) for the existing terminal_id, then post landed in Slack.

- [ ] **Step 1: Implement**

`crates/qa-service/tests/e2e_thread_continuation.rs`:
```rust
use chrono::Utc;
use qa_service::adapter_client::{AdapterClient, MockAdapter, ReadRes};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::mode::AnswerMode;
use qa_service::slack::{MockSlackClient, SlackClient};
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_config::schema::AnswerSection;
use totsuka_core::SystemClock;

fn answer_cfg() -> AnswerSection {
    AnswerSection {
        sentinel: "<<TOTSUKA_DONE>>".into(),
        answer_open_tag: "<answer>".into(),
        answer_close_tag: "</answer>".into(),
        poll_interval_ms: 20,
        stable_revision_secs: 1,
        answer_timeout_secs: 5,
        pane_idle_ttl_secs: 1800,
        max_concurrent_answers: 4,
    }
}

#[tokio::test]
async fn existing_thread_mapping_sends_no_spawn() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_read_response(ReadRes {
        revision: 5,
        text: "<answer>follow-up</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();

    let now = Utc::now();
    thread_map.upsert(&ThreadMapping {
        thread_ts: thread_ts.clone(),
        terminal_id: "term_existing".into(),
        repo: "acme/api".into(),
        last_activity_at: now,
        created_at: now,
    }).await.unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "follow-up question".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
    };
    let _ = handle_answer(&ctx, input).await.unwrap();

    assert!(adapter.expected_spawns().is_empty(), "must NOT spawn on existing mapping");
    let sends = adapter.expected_sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, "term_existing");
    assert_eq!(sends[0].1, "follow-up question");

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "follow-up");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();
}
```

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service --test e2e_thread_continuation
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(qa-service): e2e — thread continuation reuses agent"
```

---

### Task 27: e2e — restart recovery (reconcile orphans)

**Files:**
- Create: `crates/qa-service/tests/e2e_recovery.rs`

**Interfaces:**
- Consumes: `MockAdapter::set_list_response`, `ThreadMapRepo`, `reconcile`.
- Produces: e2e test that seeds two mappings (one matching a live agent, one orphan), and one live agent without a mapping; asserts that `reconcile` keeps 1, deletes 1 mapping orphan, and closes 1 pane orphan.

- [ ] **Step 1: Implement**

`crates/qa-service/tests/e2e_recovery.rs`:
```rust
use chrono::Utc;
use qa_service::adapter_client::{AdapterClient, AgentSummary, MockAdapter};
use qa_service::recovery::reconcile;
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::SystemClock;

#[tokio::test]
async fn reconcile_keeps_pairs_drops_mapping_orphans_closes_pane_orphans() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock = Arc::new(SystemClock);
    let thread_map = ThreadMapRepo::new(pool.clone(), clock.clone());

    let alive_ts = format!("alive_{}", uuid::Uuid::new_v4().simple());
    let orphan_ts = format!("orphan_{}", uuid::Uuid::new_v4().simple());

    for t in [&alive_ts, &orphan_ts] {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(t).execute(&pool).await.unwrap();
    }

    let now = Utc::now();
    thread_map.upsert(&ThreadMapping {
        thread_ts: alive_ts.clone(),
        terminal_id: "term_alive".into(),
        repo: "acme/api".into(),
        last_activity_at: now,
        created_at: now,
    }).await.unwrap();
    thread_map.upsert(&ThreadMapping {
        thread_ts: orphan_ts.clone(),
        terminal_id: "term_dead".into(),
        repo: "acme/api".into(),
        last_activity_at: now,
        created_at: now,
    }).await.unwrap();

    let adapter = MockAdapter::new();
    adapter.set_list_response(vec![
        AgentSummary {
            agent_id: "agent_alive".into(),
            terminal_id: "term_alive".into(),
            label: "totsuka:qa-1:answer:0".into(),
        },
        AgentSummary {
            agent_id: "agent_pane_orphan".into(),
            terminal_id: "term_pane_orphan".into(),
            label: "totsuka:qa-2:answer:0".into(),
        },
    ]);

    let report = reconcile(&thread_map, &adapter as &dyn AdapterClient).await.unwrap();
    assert_eq!(report.kept, 1);
    assert_eq!(report.mapping_orphans_deleted, 1);
    assert_eq!(report.pane_orphans_closed, 1);

    // Pane orphan should have been stopped.
    let stops = adapter.expected_stops();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].0, "agent_pane_orphan");

    // Mapping orphan should be gone.
    assert!(thread_map.get(&orphan_ts).await.unwrap().is_none());
    // Alive mapping should still exist.
    assert!(thread_map.get(&alive_ts).await.unwrap().is_some());

    for t in [&alive_ts, &orphan_ts] {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(t).execute(&pool).await.unwrap();
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service --test e2e_recovery
```
Expected: 1 passed.

```bash
git add crates/qa-service/
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(qa-service): e2e — restart recovery reconciles orphans"
```

---

### Task 28: CI gate + PR

**Files:** none modified (CI workflow runs fmt / clippy / test / deny / typos for the workspace per the established pipeline).

- [ ] **Step 1: Full local sweep**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test --workspace --locked
```
Expected: all green.

If clippy flags `too_many_arguments` on the dispatch/sweeper/recovery functions, add `#[allow(clippy::too_many_arguments)]` per the pattern established in PR #5.

- [ ] **Step 2: Push + open PR**

```bash
git push -u origin feat/qa-service
gh pr create --title "feat(qa-service): Slack Socket Mode + LLM classifier + answer pipeline" --body "$(cat <<'EOF'
## Summary
- New `crates/qa-service/` (bin + lib) — Slack Socket Mode listener that classifies questions by repo, drives Claude via agent-adapter, posts answers, and creates GitHub Inbox items from reactions
- 5 provider abstraction (anthropic / openai / openrouter / litellm / openai_compatible) via 2 impls + factory dispatch
- Sentinel-bounded answer extraction with tag fallback + UTF-8-safe truncate
- Thread mapping (qa_thread_agent) + restart recovery (reconcile orphans) + idle pane sweeper
- GraphQL injection-safe Inbox draft creation (variables-based, regression test)

## Test plan
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked` (with pgmq + DATABASE_URL)
- [ ] e2e: `--test e2e_high_conf_answer`
- [ ] e2e: `--test e2e_thread_continuation`
- [ ] e2e: `--test e2e_recovery`
- [ ] regression: `--test gh_inbox` (GraphQL injection)
EOF
)"
gh pr checks --watch
```

- [ ] **Step 3: Merge**

```bash
gh pr merge --merge --delete-branch
git checkout main && git pull --ff-only
```

---

## Self-Review Notes (controller checklist before kick-off)

- **Spec coverage** (§8.4): every bullet has a mapped task — Slack Socket Mode (14), Slack Web (12), LLM classifier (6-9), repo selector (10), mode (11), question filter (15), answer extract/pipeline (16-17), reaction → Inbox (18-19), pane sweeper (20), restart recovery (21), Slack catchup (22), lifecycle + listener (23), main wiring (24), 3 e2e (25-27).
- **Schema** (§11.1 / §11.4): handled in Task 3 (MIN/TARGET = 6) and Task 23 (`probe_repo_descriptions` enforces non-empty description).
- **Secret discipline** (§11.7): Tokens (`slack_bot_token`, `slack_app_token`, `classifier.api_key`, `github_token`) all `Secret<String>`. `.expose()` only at outbound HTTP/WS sites (4 places: `slack/web.rs`, `slack/socket.rs`, classifier impls, `gh_inbox.rs`).
- **GraphQL injection**: `gh_inbox.rs` uses const document + variables (Task 18 regression test).
- **Atomicity / determinism**: qa-service emits no bus events itself in this plan — answers are posted via Slack API and Inbox via GitHub REST. The catchup cursor (Task 22) is the only state write outside `qa_thread_agent`.
- **Concurrency**: `max_concurrent_answers` enforced via `Arc<Semaphore>` in the dispatch loop. Slack inbound channel bounded at 128 with drop-oldest semantics.
- **Test quality**: every external integration has either a TCP-stub regression test (`classifier_anthropic`, `classifier_openai_compat`, `slack_web`, `gh_inbox`) or an e2e-with-mocks (`e2e_*`).
- **CI shape**: Task 28 mirrors PR #5's gate exactly.
- **No half-finished bin**: every task ends with a green build. Tasks 14 / 17 / 19 / 20 / 21 are coordinator code without inline tests, but each one builds cleanly and is covered by the e2e tasks (25-27) that follow.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-29-qa-service.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration via `superpowers:subagent-driven-development`.

**2. Inline Execution** — execute tasks in this session with checkpoint review via `superpowers:executing-plans`.

Which approach?
