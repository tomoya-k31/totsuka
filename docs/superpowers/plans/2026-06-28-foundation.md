# totsuka Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 残り 5 バイナリ (agent-adapter / orchestrator / github-watcher / qa-service / totsukactl) が依存する **4 つの共有 crate** (totsuka-core / totsuka-config / totsuka-telemetry / totsuka-bus) と、**Postgres+pgmq 動作環境** (docker compose) + **migrations** を確立する。

**Architecture:** Cargo workspace に共有ライブラリ crate を並べ、Postgres は `ghcr.io/pgmq/pg18-pgmq:v1.10.0` を docker compose で起動。pgmq の publish/pull/ack を薄ラッパで隠蔽し、上位 bin は型安全な DomainEvent と effect_key で会話する。Clock / Secret / Notifier / ColumnId などの cross-cutting 規約 (spec §11) も本 plan で固定する。

**Tech Stack:** Rust stable / tokio (rt-multi-thread) / axum / sqlx (postgres) / serde + toml / tracing + tracing-subscriber + tracing-appender / thiserror + anyhow / chrono / reqwest (TLS) / docker compose / pgmq 1.10.0

## Global Constraints

(spec から逐語抜粋。全タスクは暗黙にこれらを満たす)

- Rust toolchain: **stable**、`[profile.release] panic = "abort"`、`tokio::task::block_in_place` は clippy deny
- Postgres image: `ghcr.io/pgmq/pg18-pgmq:v1.10.0` (固定タグ)
- pgmq queue name: `totsuka_events`、`visibility_secs=30`、`batch_size=16`
- Migrations: **forward-only**、`down/` 禁止 (CI で拒否)、唯一の DB mutator は `sqlx migrate`
- Schema versioning: `schema_meta(version, applied_at)` に最新 migration 番号を持ち、bin は `MIN/TARGET_SCHEMA_VERSION` と照合 (spec §11.1)
- XDG: 設定 `~/.config/totsuka/`、状態 `~/.local/state/totsuka/`、データ `~/.local/share/totsuka/`
- 設定: TOML + env override `TOTSUKA__SECTION__KEY=value`、変数展開 `${name}` / `${env:NAME}`、未定義/循環は起動時エラー (spec §6, §12.2)
- secrets: `Secret<T>` newtype、Debug/Display は `***`、`.expose()` は outbound 構築時のみ (spec §11.7)
- Clock: 全 bin が `Arc<dyn Clock>` を注入で受ける。`SystemTime::now()` 直接呼び出しは clippy deny (spec §11.5)、storage は UTC、表示は `[totsuka].timezone = "Asia/Tokyo"`
- Error: lib は `thiserror`、bin は `anyhow`。`code()` で RFC7807 `type` URI (`/errors/<kind>`) を返す (spec §11.6)
- HTTP: パス prefix `/v1/`、`x-totsuka-request-id` 伝播、エラーは RFC7807
- bus envelope: `{ event_key, source, type, published_at, trace_id, payload }`
- event_key: `gh:delivery:<id>` / `slack:event:<id>` / `derived:<deterministic-key>`
- effect_key: `spawn:{task_id}:{phase}:{attempt}` (spec §11.15)
- ColumnId 8 値: `Inbox / Ready / Design / DesignReview / ImplVerify / FinalReview / AwaitingRelease / Released` (spec §11.4)
- Notifier: `totsuka-telemetry::notify` に集中、`NotifyKind` enum + dedup_key + per-sink rate-limit + `${state_dir}/notify_state.json` 永続 (spec §13)
- Bounded channels only: `tokio::sync::mpsc::channel(N)` (unbounded 禁止) (spec §11.8)
- Blocking 隔離: subprocess / 大文字列パース / 同期 fs は `spawn_blocking` (spec §11.10)

---

## File Structure (本 plan で作成・変更するもの)

```
totsuka/
├── Cargo.toml                          [Create] workspace 定義
├── rust-toolchain.toml                 [Create]
├── mise.toml                           [Create]
├── .gitignore                          [Modify]
├── justfile                            [Create] 開発用ショートカット
├── deploy/
│   └── docker-compose.yml              [Create] pgmq サービス
├── migrations/                         [Create] sqlx migration (forward-only)
│   ├── 0000_schema_meta.sql
│   ├── 0001_processed_events.sql       (PARTITION BY RANGE)
│   ├── 0002_processed_effects.sql
│   ├── 0003_catchup_cursor.sql
│   ├── 0004_gh_item_status.sql
│   ├── 0005_tasks.sql
│   └── 0006_qa_thread_agent.sql
├── crates/
│   ├── totsuka-core/
│   │   ├── Cargo.toml                  [Create]
│   │   └── src/
│   │       ├── lib.rs                  [Create] re-export
│   │       ├── error.rs                [Create] thiserror Error + code()
│   │       ├── clock.rs                [Create] Clock trait + System/Mock
│   │       ├── secret.rs               [Create] Secret<T> newtype
│   │       ├── column.rs               [Create] ColumnId enum + parser
│   │       ├── phase.rs                [Create] Phase enum
│   │       ├── task.rs                 [Create] TaskId, task_id_short
│   │       ├── key.rs                  [Create] event_key / effect_key
│   │       ├── event.rs                [Create] DomainEvent + envelope
│   │       └── notify.rs               [Create] NotifyKind enum
│   ├── totsuka-config/
│   │   ├── Cargo.toml                  [Create]
│   │   └── src/
│   │       ├── lib.rs                  [Create]
│   │       ├── schema.rs               [Create] Config struct + sections
│   │       ├── expand.rs               [Create] ${var} / ${env:NAME} 展開
│   │       ├── validate.rs             [Create] 排他/必須/循環チェック
│   │       └── env_override.rs         [Create] TOTSUKA__ 系
│   ├── totsuka-telemetry/
│   │   ├── Cargo.toml                  [Create]
│   │   └── src/
│   │       ├── lib.rs                  [Create]
│   │       ├── log.rs                  [Create] tracing init + rotation
│   │       ├── http.rs                 [Create] healthz/readyz/metrics axum
│   │       ├── request_id.rs           [Create] middleware
│   │       └── notify/
│   │           ├── mod.rs              [Create] Notifier + state persistence
│   │           ├── payload.rs          [Create] NotifyPayload
│   │           ├── routing.rs          [Create] kind→sinks 写像
│   │           ├── rate.rs             [Create] token bucket
│   │           ├── sink_log.rs         [Create] tracing sink
│   │           └── sink_slack.rs       [Create] webhook sink
│   └── totsuka-bus/
│       ├── Cargo.toml                  [Create]
│       └── src/
│           ├── lib.rs                  [Create]
│           ├── envelope.rs             [Create] EventEnvelope serde
│           ├── publisher.rs            [Create] pgmq.send wrapper + tx 共有
│           ├── consumer.rs             [Create] pgmq.read / delete loop
│           └── pgmq.rs                 [Create] 低レベル SQL 関数呼出
└── examples/
    └── totsuka.toml.example            [Create]
```

各 crate に対応する `tests/` ディレクトリも作成し、単体テストは `src/*.rs` 内 `#[cfg(test)] mod tests`、結合テスト (pgmq 接続が要るもの) は `crates/<name>/tests/*.rs` に置く。

---

## Tasks

タスクは TDD サイクル (test → fail → impl → pass → commit) で構成。各タスクは 5–20 分の作業単位。

### Task A1: Cargo workspace 初期化

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `mise.toml`
- Modify: `.gitignore`
- Create: `justfile`

**Interfaces:**
- Consumes: 無し (新規)
- Produces: `cargo build --workspace` が空 build で通る workspace

- [ ] **Step 1: Cargo workspace 定義を書く**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = [
    "crates/totsuka-core",
    "crates/totsuka-config",
    "crates/totsuka-telemetry",
    "crates/totsuka-bus",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.80"
license = "Apache-2.0"
repository = "https://github.com/tomoya-k31/totsuka"

[workspace.dependencies]
# async runtime
tokio = { version = "1.40", features = ["rt-multi-thread", "macros", "sync", "time", "signal", "fs", "process"] }
# error
thiserror = "1.0"
anyhow = "1.0"
# serde
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"
# logging / metrics
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-appender = "0.2"
# http
axum = { version = "0.7", features = ["macros"] }
hyper = { version = "1.4", features = ["full"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["trace"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls-native-roots", "json"] }
# db
sqlx = { version = "0.8", features = ["runtime-tokio", "tls-rustls", "postgres", "json", "chrono", "uuid", "migrate"] }
# time
chrono = { version = "0.4", default-features = false, features = ["serde", "clock"] }
chrono-tz = "0.10"
# misc
uuid = { version = "1.10", features = ["v4", "serde"] }
regex = "1.11"

[profile.release]
panic = "abort"
lto = "thin"
codegen-units = 1

[profile.dev]
debug = 1
```

- [ ] **Step 2: rust-toolchain.toml を書く**

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
profile = "minimal"
```

- [ ] **Step 3: mise.toml を書く**

`mise.toml`:
```toml
[tools]
rust = "stable"
"cargo:sqlx-cli" = "0.8.2"
just = "1.36"
```

- [ ] **Step 4: .gitignore に Rust エントリを追加**

`.gitignore` 末尾に追記:
```
# Rust
target/
**/*.rs.bk
Cargo.lock
.sqlx/
# runtime state (XDG paths are outside repo)
```

> 注: `Cargo.lock` は workspace バイナリ群を含むため後続 plan で commit する。foundation では一旦 ignore。

- [ ] **Step 5: justfile を書く (開発ショートカット)**

`justfile`:
```just
set shell := ["bash", "-cu"]

# pgmq コンテナ起動
pgmq-up:
    docker compose -f deploy/docker-compose.yml up -d pgmq

pgmq-down:
    docker compose -f deploy/docker-compose.yml down

pgmq-logs:
    docker compose -f deploy/docker-compose.yml logs -f pgmq

# migration 適用 (DATABASE_URL 必須)
db-migrate:
    sqlx migrate run --source migrations

db-info:
    sqlx migrate info --source migrations

# テスト用 DB 再生成
db-reset:
    psql "$DATABASE_URL" -c "DROP DATABASE IF EXISTS totsuka_test"
    psql "$DATABASE_URL" -c "CREATE DATABASE totsuka_test"

# workspace 全テスト
test:
    cargo test --workspace --all-features

# lint
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo fmt --check
```

- [ ] **Step 6: 空 build を確認**

Run: `cargo metadata --no-deps --format-version=1 | jq '.workspace_members'`
Expected: 4 メンバー (`totsuka-core` 〜 `totsuka-bus`) が列挙 ※ crate ディレクトリは未作成なので `cargo build` は失敗する。本 task は workspace 設定のみで完了

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml rust-toolchain.toml mise.toml .gitignore justfile
git commit -m "chore: scaffold cargo workspace + dev tooling"
```

---

### Task A2: docker-compose.yml で pgmq 起動

**Files:**
- Create: `deploy/docker-compose.yml`
- Create: `deploy/.env.example`

**Interfaces:**
- Consumes: 無し
- Produces: `just pgmq-up` で localhost:5432 に pgmq 拡張入り Postgres が起動

- [ ] **Step 1: compose 定義を書く**

`deploy/docker-compose.yml`:
```yaml
services:
  pgmq:
    image: ghcr.io/pgmq/pg18-pgmq:v1.10.0
    container_name: totsuka-pgmq
    user: "0:0"
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-postgres}
      POSTGRES_DB: totsuka
    ports:
      - "127.0.0.1:5432:5432"
    volumes:
      - totsuka_pgmq_data:/var/lib/postgresql
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d totsuka"]
      interval: 5s
      timeout: 3s
      retries: 10
      start_period: 10s
    restart: "no"

volumes:
  totsuka_pgmq_data:
    name: totsuka_pgmq_data
```

`deploy/.env.example`:
```
POSTGRES_PASSWORD=postgres
```

- [ ] **Step 2: 起動して healthcheck を確認**

Run:
```bash
just pgmq-up
# healthy になるまで待つ
docker inspect --format='{{.State.Health.Status}}' totsuka-pgmq
```
Expected: `healthy` (10〜30 秒以内)

- [ ] **Step 3: pgmq 拡張のバージョン確認**

Run:
```bash
psql "postgres://postgres:postgres@127.0.0.1:5432/totsuka" -c "CREATE EXTENSION IF NOT EXISTS pgmq;"
psql "postgres://postgres:postgres@127.0.0.1:5432/totsuka" -tAc "SELECT extversion FROM pg_extension WHERE extname='pgmq';"
```
Expected: `1.10.0` 系の値が返る

- [ ] **Step 4: down してデータ永続化を確認**

Run:
```bash
just pgmq-down
just pgmq-up
psql "postgres://postgres:postgres@127.0.0.1:5432/totsuka" -tAc "SELECT extversion FROM pg_extension WHERE extname='pgmq';"
```
Expected: 同じバージョンが返る (volume が残っている)

- [ ] **Step 5: Commit**

```bash
git add deploy/
git commit -m "feat(deploy): pgmq Postgres container via docker compose"
```

---

### Task A3: Migration 0000 — schema_meta + pgmq 拡張

**Files:**
- Create: `migrations/0000_schema_meta.sql`

**Interfaces:**
- Consumes: pgmq コンテナが起動中
- Produces: `schema_meta(version, applied_at)` テーブル + pgmq 拡張、`sqlx migrate run` が成功

- [ ] **Step 1: migration ファイルを書く**

`migrations/0000_schema_meta.sql`:
```sql
-- pgmq 拡張 (image 内蔵だがアプリ DB に明示インストール)
CREATE EXTENSION IF NOT EXISTS pgmq;

-- bin↔DB ハンドシェイク用 (spec §11.1)
-- version は最新の migration 番号と一致させる。bin は MIN/TARGET_SCHEMA_VERSION でこれを照合
CREATE TABLE IF NOT EXISTS schema_meta (
  version     INT          PRIMARY KEY,
  applied_at  TIMESTAMPTZ  NOT NULL DEFAULT now()
);

-- 本 migration 自身を記録
INSERT INTO schema_meta (version) VALUES (0) ON CONFLICT DO NOTHING;
```

- [ ] **Step 2: 適用して通ることを確認**

Run:
```bash
export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/totsuka"
just db-migrate
psql "$DATABASE_URL" -c "SELECT * FROM schema_meta;"
```
Expected: version=0 の行が 1 つ

- [ ] **Step 3: 再実行で冪等であることを確認**

Run: `just db-migrate`
Expected: 「no migrations to apply」または同等のメッセージ (重複 INSERT は ON CONFLICT で握り潰し)

- [ ] **Step 4: Commit**

```bash
git add migrations/0000_schema_meta.sql
git commit -m "feat(db): schema_meta table + pgmq extension"
```

---

### Task A4: Migration 0001 — processed_events (パーティション)

**Files:**
- Create: `migrations/0001_processed_events.sql`

**Interfaces:**
- Consumes: schema_meta
- Produces: `processed_events` 親テーブル + 初期 2 週間のパーティション

- [ ] **Step 1: migration ファイルを書く**

`migrations/0001_processed_events.sql`:
```sql
-- spec §8.1 / §11.2: イベント単位デデュープ。週パーティションで保持期間後に DROP
CREATE TABLE IF NOT EXISTS processed_events (
  event_key     TEXT         NOT NULL,
  source        TEXT         NOT NULL,
  event_type    TEXT         NOT NULL,
  payload_hash  TEXT,
  received_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (event_key, received_at)
) PARTITION BY RANGE (received_at);

-- 初期パーティション: 当週 + 翌週 (orchestrator nightly job が以降を作る)
-- 日付計算は手動 (date_trunc('week', now()) を即値化するため migration 適用時に
-- 計算する。実本番では orchestrator がパーティションを先読みで作る)
DO $$
DECLARE
    wk_start DATE := date_trunc('week', now())::date;
    wk_end   DATE;
    pname    TEXT;
BEGIN
    FOR i IN 0..1 LOOP
        wk_end := wk_start + INTERVAL '7 days';
        pname  := format('processed_events_%s', to_char(wk_start, 'YYYYMMDD'));
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF processed_events
             FOR VALUES FROM (%L) TO (%L)',
            pname, wk_start, wk_end
        );
        wk_start := wk_end;
    END LOOP;
END $$;

-- 検索用
CREATE INDEX IF NOT EXISTS idx_processed_events_source_received
  ON processed_events (source, received_at DESC);

INSERT INTO schema_meta (version) VALUES (1) ON CONFLICT DO NOTHING;
```

- [ ] **Step 2: 適用して 2 パーティションが作られたことを確認**

Run:
```bash
just db-migrate
psql "$DATABASE_URL" -c "\d+ processed_events"
```
Expected: 2 つの partition (`processed_events_YYYYMMDD`) が `Partitions:` セクションに表示

- [ ] **Step 3: INSERT が正しいパーティションに振り分けられるか**

Run:
```bash
psql "$DATABASE_URL" -c "INSERT INTO processed_events (event_key, source, event_type) VALUES ('test:k1', 'test', 'unit');"
psql "$DATABASE_URL" -c "SELECT tableoid::regclass, event_key FROM processed_events;"
```
Expected: `processed_events_<今週>` という tableoid が表示される

- [ ] **Step 4: 重複 event_key + 同じ received_at は PK 違反**

Run:
```bash
psql "$DATABASE_URL" -c "INSERT INTO processed_events (event_key, source, event_type) VALUES ('test:k1', 'test', 'unit');" 2>&1 | grep -i duplicate
```
Expected: duplicate key value violates ... のエラー。テスト後は `DELETE FROM processed_events WHERE event_key='test:k1';` で清掃

- [ ] **Step 5: Commit**

```bash
git add migrations/0001_processed_events.sql
git commit -m "feat(db): processed_events partitioned by week"
```

---

### Task A5: Migration 0002 — processed_effects

**Files:**
- Create: `migrations/0002_processed_effects.sql`

**Interfaces:**
- Consumes: processed_events
- Produces: `processed_effects` 親テーブル + 初期パーティション + claim 用 INDEX

- [ ] **Step 1: migration ファイルを書く**

`migrations/0002_processed_effects.sql`:
```sql
-- spec §8.2 / §13.1 / §11.15: 副作用の実行権 (lease 付き)
-- effect_key = "spawn:{task_id}:{phase}:{attempt}" 形式 (DiffBack で attempt+1)
CREATE TABLE IF NOT EXISTS processed_effects (
  effect_key        TEXT         NOT NULL,
  event_key         TEXT         NOT NULL,
  effect_type       TEXT         NOT NULL,
  status            TEXT         NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','in_progress','done','failed')),
  lease_owner       TEXT,
  lease_expires_at  TIMESTAMPTZ,
  attempts          INT          NOT NULL DEFAULT 0,
  result            JSONB,
  created_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
  updated_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (effect_key, created_at)
) PARTITION BY RANGE (created_at);

DO $$
DECLARE
    wk_start DATE := date_trunc('week', now())::date;
    wk_end   DATE;
    pname    TEXT;
BEGIN
    FOR i IN 0..1 LOOP
        wk_end := wk_start + INTERVAL '7 days';
        pname  := format('processed_effects_%s', to_char(wk_start, 'YYYYMMDD'));
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF processed_effects
             FOR VALUES FROM (%L) TO (%L)',
            pname, wk_start, wk_end
        );
        wk_start := wk_end;
    END LOOP;
END $$;

-- 回収 (期限切れ in_progress / failed retryable)
CREATE INDEX IF NOT EXISTS idx_effects_recoverable
  ON processed_effects (status, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_effects_event_key
  ON processed_effects (event_key);

INSERT INTO schema_meta (version) VALUES (2) ON CONFLICT DO NOTHING;
```

- [ ] **Step 2: 適用**

Run: `just db-migrate && psql "$DATABASE_URL" -c "\d processed_effects"`
Expected: テーブルとパーティションが表示

- [ ] **Step 3: 状態制約を確認**

Run:
```bash
psql "$DATABASE_URL" -c "INSERT INTO processed_effects (effect_key, event_key, effect_type, status) VALUES ('e1','ev1','agent_spawn','invalid');" 2>&1 | grep -i check
```
Expected: violates check constraint

- [ ] **Step 4: Commit**

```bash
git add migrations/0002_processed_effects.sql
git commit -m "feat(db): processed_effects with lease + partitions"
```

---

### Task A6: Migrations 0003–0006 (cursor / snapshot / tasks / qa_thread_agent)

**Files:**
- Create: `migrations/0003_catchup_cursor.sql`
- Create: `migrations/0004_gh_item_status.sql`
- Create: `migrations/0005_tasks.sql`
- Create: `migrations/0006_qa_thread_agent.sql`

**Interfaces:**
- Consumes: schema_meta
- Produces: cursor / snapshot / tasks / qa_thread_agent テーブル群、`schema_meta.version = 6` が最新

- [ ] **Step 1: 0003 catchup_cursor を書く**

`migrations/0003_catchup_cursor.sql`:
```sql
-- spec §9 / parent §13.2: 発生源/スコープ単位のカーソル
CREATE TABLE IF NOT EXISTS catchup_cursor (
  source      TEXT         NOT NULL,
  scope       TEXT         NOT NULL,
  cursor      TEXT         NOT NULL,
  updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
  PRIMARY KEY (source, scope)
);

INSERT INTO schema_meta (version) VALUES (3) ON CONFLICT DO NOTHING;
```

- [ ] **Step 2: 0004 gh_item_status を書く**

`migrations/0004_gh_item_status.sql`:
```sql
-- spec §11.4: status は ColumnId snake_case で保存 (totsuka-core の serde 形式と一致)
CREATE TABLE IF NOT EXISTS gh_item_status (
  item_id      TEXT         PRIMARY KEY,            -- ProjectV2Item.id (PVTI_...)
  status       TEXT,                                  -- ColumnId snake_case or NULL
  content_ref  TEXT,                                  -- "owner/repo#123" 観測用
  closed_at    TIMESTAMPTZ,                           -- item close 検知時刻 (retention 用)
  updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_gh_item_status_closed
  ON gh_item_status (closed_at) WHERE closed_at IS NOT NULL;

INSERT INTO schema_meta (version) VALUES (4) ON CONFLICT DO NOTHING;
```

- [ ] **Step 3: 0005 tasks を書く (spec §11.14 そのまま)**

`migrations/0005_tasks.sql`:
```sql
-- spec §11.14: task_id = ProjectV2Item.id、task_id_short は末尾 12 文字
CREATE TABLE IF NOT EXISTS tasks (
  id                                  TEXT         PRIMARY KEY,
  task_id_short                       TEXT         NOT NULL UNIQUE,
  repo                                TEXT         NOT NULL,
  pr_node_id                          TEXT,
  current_column                      TEXT         NOT NULL,
  current_phase                       TEXT,
  impl_verify_attempt                 INT          NOT NULL DEFAULT 0,
  suppress_writeback_until_human_move BOOLEAN      NOT NULL DEFAULT FALSE,
  spawned_at                          TIMESTAMPTZ,
  created_at                          TIMESTAMPTZ  NOT NULL DEFAULT now(),
  updated_at                          TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tasks_repo            ON tasks (repo);
CREATE INDEX IF NOT EXISTS idx_tasks_task_id_short   ON tasks (task_id_short);
CREATE INDEX IF NOT EXISTS idx_tasks_pr_node_id      ON tasks (pr_node_id)
  WHERE pr_node_id IS NOT NULL;

INSERT INTO schema_meta (version) VALUES (5) ON CONFLICT DO NOTHING;
```

- [ ] **Step 4: 0006 qa_thread_agent を書く**

`migrations/0006_qa_thread_agent.sql`:
```sql
-- spec §8.4: Slack thread_ts → herdr terminal_id mapping
CREATE TABLE IF NOT EXISTS qa_thread_agent (
  thread_ts         TEXT         PRIMARY KEY,
  terminal_id       TEXT         NOT NULL,
  repo              TEXT         NOT NULL,
  last_activity_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
  created_at        TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_qa_thread_agent_last_activity
  ON qa_thread_agent (last_activity_at);

INSERT INTO schema_meta (version) VALUES (6) ON CONFLICT DO NOTHING;
```

- [ ] **Step 5: 全 migration 適用 + 最新 version 確認**

Run:
```bash
just db-migrate
psql "$DATABASE_URL" -tAc "SELECT max(version) FROM schema_meta;"
```
Expected: `6`

- [ ] **Step 6: テーブル列挙**

Run: `psql "$DATABASE_URL" -c "\dt"`
Expected: `schema_meta / processed_events / processed_effects / catchup_cursor / gh_item_status / tasks / qa_thread_agent` の 7 テーブル (+ partition の子)

- [ ] **Step 7: Commit**

```bash
git add migrations/0003_catchup_cursor.sql migrations/0004_gh_item_status.sql \
        migrations/0005_tasks.sql migrations/0006_qa_thread_agent.sql
git commit -m "feat(db): catchup_cursor / gh_item_status / tasks / qa_thread_agent"
```

---

### Task B1: totsuka-core スケルトン + Error 型

**Files:**
- Create: `crates/totsuka-core/Cargo.toml`
- Create: `crates/totsuka-core/src/lib.rs`
- Create: `crates/totsuka-core/src/error.rs`

**Interfaces:**
- Consumes: workspace dependencies
- Produces: `totsuka_core::Error` enum + `Error::code() -> &'static str` (RFC7807 type URI)

- [ ] **Step 1: crate スケルトン**

`crates/totsuka-core/Cargo.toml`:
```toml
[package]
name = "totsuka-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
thiserror.workspace = true
serde = { workspace = true }
serde_json.workspace = true
chrono = { workspace = true }
chrono-tz.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

`crates/totsuka-core/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod error;

pub use error::{Error, Result};
```

- [ ] **Step 2: 失敗テストを書く**

`crates/totsuka-core/src/error.rs`:
```rust
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("worktree in use: {0}")]
    WorktreeInUse(String),
    #[error("capacity full")]
    CapacityFull,
    #[error("argv contains secret-like flag")]
    ArgvSecretViolation,
    #[error("schema version out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("config error: {0}")]
    Config(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl Error {
    /// RFC7807 `type` URI (spec §11.6)
    pub fn code(&self) -> &'static str {
        match self {
            Error::RepoNotRegistered(_)  => "/errors/repo_not_registered",
            Error::WorktreeInUse(_)      => "/errors/worktree_in_use",
            Error::CapacityFull          => "/errors/capacity_full",
            Error::ArgvSecretViolation   => "/errors/argv_secret_violation",
            Error::SchemaOutOfRange{..}  => "/errors/schema_out_of_range",
            Error::Config(_)             => "/errors/config",
            Error::Io(_)                 => "/errors/io",
            Error::Serde(_)              => "/errors/serde",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_for_each_variant_matches_uri_prefix() {
        let e = Error::RepoNotRegistered("x/y".into());
        assert_eq!(e.code(), "/errors/repo_not_registered");
        let e = Error::CapacityFull;
        assert_eq!(e.code(), "/errors/capacity_full");
        let e = Error::SchemaOutOfRange { got: 3, min: 5, target: 7 };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
}
```

- [ ] **Step 3: コンパイル + テスト pass**

Run: `cargo test -p totsuka-core`
Expected: `test result: ok. 1 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): Error enum + RFC7807 code() mapping"
```

---

### Task B2: Clock trait + SystemClock + MockClock

**Files:**
- Create: `crates/totsuka-core/src/clock.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: chrono
- Produces: `Clock` trait, `SystemClock`, `MockClock` (test util)

- [ ] **Step 1: テスト先行**

`crates/totsuka-core/src/clock.rs`:
```rust
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// テスト用。advance() で時刻を進められる
#[derive(Clone)]
pub struct MockClock {
    inner: Arc<Mutex<DateTime<Utc>>>,
}

impl MockClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { inner: Arc::new(Mutex::new(now)) }
    }
    pub fn advance(&self, dur: chrono::Duration) {
        let mut g = self.inner.lock().unwrap();
        *g = *g + dur;
    }
    pub fn set(&self, now: DateTime<Utc>) {
        *self.inner.lock().unwrap() = now;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn system_clock_is_close_to_chrono_utc() {
        let c = SystemClock;
        let a = c.now();
        let b = Utc::now();
        assert!((b - a).num_milliseconds().abs() < 100);
    }

    #[test]
    fn mock_clock_advances() {
        let base = Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap();
        let c = MockClock::new(base);
        assert_eq!(c.now(), base);
        c.advance(chrono::Duration::seconds(30));
        assert_eq!(c.now(), base + chrono::Duration::seconds(30));
    }
}
```

- [ ] **Step 2: lib.rs に export**

`crates/totsuka-core/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod clock;
pub mod error;

pub use clock::{Clock, SystemClock, MockClock};
pub use error::{Error, Result};
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-core clock`
Expected: `2 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): Clock trait + SystemClock + MockClock"
```

---

### Task B3: Secret<T> newtype

**Files:**
- Create: `crates/totsuka-core/src/secret.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: serde
- Produces: `Secret<T>` (Debug/Display は `***`、`expose()` で内側取得)

- [ ] **Step 1: テスト + 実装**

`crates/totsuka-core/src/secret.rs`:
```rust
use std::fmt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(inner: T) -> Self { Self(inner) }
    /// 内側を露出する。outbound HTTP / DB 接続文字列構築時のみ使用
    pub fn expose(&self) -> &T { &self.0 }
    pub fn into_inner(self) -> T { self.0 }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(v: T) -> Self { Self::new(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak() {
        let s: Secret<String> = "supersecret".to_string().into();
        let d = format!("{:?}", s);
        assert!(!d.contains("supersecret"));
        assert_eq!(d, "Secret(***)");
    }

    #[test]
    fn display_does_not_leak() {
        let s: Secret<String> = "tk_abcdef".to_string().into();
        assert_eq!(format!("{}", s), "***");
    }

    #[test]
    fn expose_returns_inner() {
        let s: Secret<String> = "abc".to_string().into();
        assert_eq!(s.expose(), "abc");
    }

    #[test]
    fn deserialize_from_plain_string() {
        let s: Secret<String> = serde_json::from_str("\"v\"").unwrap();
        assert_eq!(s.expose(), "v");
    }
}
```

- [ ] **Step 2: lib.rs**

`crates/totsuka-core/src/lib.rs` の `pub mod` と `pub use` に追加:
```rust
pub mod secret;
pub use secret::Secret;
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-core secret`
Expected: `4 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): Secret<T> newtype with masked Debug/Display"
```

---

### Task B4: ColumnId enum + 表示名 mapping

**Files:**
- Create: `crates/totsuka-core/src/column.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: serde
- Produces: `ColumnId` enum (8 値、snake_case serde)、`ColumnMap` (display_name ↔ ColumnId 双方向)

- [ ] **Step 1: 実装**

`crates/totsuka-core/src/column.rs`:
```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// spec §11.4: 8 カラムの正規化。serde は snake_case 文字列
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnId {
    Inbox,
    Ready,
    Design,
    DesignReview,
    ImplVerify,
    FinalReview,
    AwaitingRelease,
    Released,
}

impl ColumnId {
    pub const ALL: [ColumnId; 8] = [
        ColumnId::Inbox, ColumnId::Ready, ColumnId::Design, ColumnId::DesignReview,
        ColumnId::ImplVerify, ColumnId::FinalReview, ColumnId::AwaitingRelease, ColumnId::Released,
    ];
    pub fn as_snake(&self) -> &'static str {
        match self {
            ColumnId::Inbox => "inbox",
            ColumnId::Ready => "ready",
            ColumnId::Design => "design",
            ColumnId::DesignReview => "design_review",
            ColumnId::ImplVerify => "impl_verify",
            ColumnId::FinalReview => "final_review",
            ColumnId::AwaitingRelease => "awaiting_release",
            ColumnId::Released => "released",
        }
    }
}

/// 表示名 (GitHub Project の絵文字付き和文) ↔ ColumnId
#[derive(Debug, Clone)]
pub struct ColumnMap {
    display_to_id: HashMap<String, ColumnId>,
    id_to_display: HashMap<ColumnId, String>,
}

impl ColumnMap {
    /// 8 値が全て map に揃っているかチェックして構築。欠落・余剰はエラー
    pub fn try_new(displays: HashMap<ColumnId, String>) -> Result<Self, ColumnMapError> {
        for id in ColumnId::ALL {
            if !displays.contains_key(&id) {
                return Err(ColumnMapError::Missing(id));
            }
        }
        let mut display_to_id = HashMap::new();
        for (id, name) in &displays {
            if display_to_id.insert(name.clone(), *id).is_some() {
                return Err(ColumnMapError::DuplicateDisplay(name.clone()));
            }
        }
        Ok(Self { display_to_id, id_to_display: displays })
    }

    pub fn resolve(&self, display: &str) -> Option<ColumnId> {
        self.display_to_id.get(display).copied()
    }
    pub fn display(&self, id: ColumnId) -> &str {
        self.id_to_display.get(&id).expect("constructor ensures coverage")
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ColumnMapError {
    #[error("column display name missing for {0:?}")]
    Missing(ColumnId),
    #[error("duplicate display name: {0}")]
    DuplicateDisplay(String),
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn snake_case_serde_roundtrip() {
        let s = serde_json::to_string(&ColumnId::ImplVerify).unwrap();
        assert_eq!(s, "\"impl_verify\"");
        let c: ColumnId = serde_json::from_str(&s).unwrap();
        assert_eq!(c, ColumnId::ImplVerify);
    }

    #[test]
    fn map_resolves_japanese_emoji_displays() {
        let m = ColumnMap::try_new(full_map()).unwrap();
        assert_eq!(m.resolve("🤖 調査・設計"), Some(ColumnId::Design));
        assert_eq!(m.display(ColumnId::Released), "🏁 完了");
    }

    #[test]
    fn missing_column_errors() {
        let mut partial = full_map();
        partial.remove(&ColumnId::Inbox);
        let err = ColumnMap::try_new(partial).unwrap_err();
        assert_eq!(err, ColumnMapError::Missing(ColumnId::Inbox));
    }

    #[test]
    fn unknown_display_returns_none() {
        let m = ColumnMap::try_new(full_map()).unwrap();
        assert_eq!(m.resolve("nope"), None);
    }
}
```

- [ ] **Step 2: lib.rs に追加**

```rust
pub mod column;
pub use column::{ColumnId, ColumnMap, ColumnMapError};
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-core column`
Expected: `4 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): ColumnId enum + ColumnMap with coverage validation"
```

---

### Task B5: Phase enum + TaskId + task_id_short

**Files:**
- Create: `crates/totsuka-core/src/phase.rs`
- Create: `crates/totsuka-core/src/task.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: ColumnId
- Produces: `Phase` enum (snake_case serde + `as_snake_short()`)、`TaskId` newtype、`task_id_short()`

- [ ] **Step 1: phase.rs**

`crates/totsuka-core/src/phase.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Design,
    ImplVerify,
}

impl Phase {
    pub fn as_snake(&self) -> &'static str {
        match self { Phase::Design => "design", Phase::ImplVerify => "impl_verify" }
    }
    /// branch 命名用の短縮形 (spec §11.14)
    pub fn as_short(&self) -> &'static str {
        match self { Phase::Design => "design", Phase::ImplVerify => "implv" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn snake_serde() {
        assert_eq!(serde_json::to_string(&Phase::ImplVerify).unwrap(), "\"impl_verify\"");
        let p: Phase = serde_json::from_str("\"design\"").unwrap();
        assert_eq!(p, Phase::Design);
    }
    #[test] fn short_form_for_branch() {
        assert_eq!(Phase::ImplVerify.as_short(), "implv");
        assert_eq!(Phase::Design.as_short(), "design");
    }
}
```

- [ ] **Step 2: task.rs**

`crates/totsuka-core/src/task.rs`:
```rust
use serde::{Deserialize, Serialize};
use std::fmt;

/// ProjectV2Item.id (`PVTI_...`)。totsuka は UUID を発行しない (spec §11.14)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(id: impl Into<String>) -> Self { Self(id.into()) }
    pub fn as_str(&self) -> &str { &self.0 }

    /// branch 名 / ログ用の末尾 12 文字短縮形 (spec §11.14)
    pub fn short(&self) -> String {
        let s = &self.0;
        if s.len() <= 12 { s.clone() } else { s[s.len()-12..].to_string() }
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl From<String> for TaskId { fn from(s: String) -> Self { Self(s) } }
impl From<&str> for TaskId { fn from(s: &str) -> Self { Self(s.to_string()) } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn short_takes_tail_12_chars() {
        let t = TaskId::new("PVTI_lAHOAjcRPs4AHvuRzgVabc123def456");
        assert_eq!(t.short(), "abc123def456");
        assert_eq!(t.short().len(), 12);
    }
    #[test] fn short_keeps_full_when_short() {
        assert_eq!(TaskId::new("short").short(), "short");
    }
    #[test] fn serde_transparent() {
        let t = TaskId::new("PVTI_x");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"PVTI_x\"");
    }
}
```

- [ ] **Step 3: lib.rs**

```rust
pub mod phase;
pub mod task;
pub use phase::Phase;
pub use task::TaskId;
```

- [ ] **Step 4: テスト pass**

Run: `cargo test -p totsuka-core phase && cargo test -p totsuka-core task`
Expected: 5 tests passed (3 + 2)

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): Phase enum + TaskId with task_id_short tail-12"
```

---

### Task B6: event_key / effect_key 生成

**Files:**
- Create: `crates/totsuka-core/src/key.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: TaskId, Phase
- Produces: `event_key_*` 関数群、`spawn_effect_key(task, phase, attempt)`

- [ ] **Step 1: 実装**

`crates/totsuka-core/src/key.rs`:
```rust
use crate::{Phase, TaskId};

/// GitHub webhook delivery 由来 (spec §8.1)
pub fn event_key_gh_delivery(delivery_id: &str) -> String {
    format!("gh:delivery:{}", delivery_id)
}

/// GitHub Project status snapshot diff 由来 (spec §8.3、catchup)
pub fn event_key_gh_status(item_id: &str, to_status_hash: &str) -> String {
    format!("gh:status:{}:{}", item_id, to_status_hash)
}

/// GitHub issue updated (REST since pull) 由来
pub fn event_key_gh_issue(issue_node_id: &str, updated_at_ms: i64) -> String {
    format!("gh:issue:{}:{}", issue_node_id, updated_at_ms)
}

/// Slack event 由来
pub fn event_key_slack(event_id: &str) -> String {
    format!("slack:event:{}", event_id)
}

/// orchestrator 内部派生 (deterministic)
pub fn event_key_derived(key: &str) -> String {
    format!("derived:{}", key)
}

/// agent spawn 副作用キー (spec §11.15: attempt で DiffBack 再 spawn を区別)
pub fn spawn_effect_key(task: &TaskId, phase: Phase, attempt: i32) -> String {
    format!("spawn:{}:{}:{}", task.as_str(), phase.as_snake(), attempt)
}

/// カラム移動副作用 (spec §8.2 型B)
pub fn column_move_effect_key(task: &TaskId, to_status_snake: &str) -> String {
    format!("move:{}:{}", task.as_str(), to_status_snake)
}

/// Slack 投稿副作用
pub fn slack_post_effect_key(channel: &str, event_id: &str) -> String {
    format!("slack:{}:{}", channel, event_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn event_key_formats_are_stable() {
        assert_eq!(event_key_gh_delivery("abc-123"), "gh:delivery:abc-123");
        assert_eq!(event_key_slack("Ev01"), "slack:event:Ev01");
        assert_eq!(event_key_derived("phase_timeout:t1"), "derived:phase_timeout:t1");
    }

    #[test] fn spawn_effect_key_includes_attempt() {
        let t = TaskId::new("PVTI_x");
        assert_eq!(spawn_effect_key(&t, Phase::ImplVerify, 0), "spawn:PVTI_x:impl_verify:0");
        assert_eq!(spawn_effect_key(&t, Phase::ImplVerify, 1), "spawn:PVTI_x:impl_verify:1");
        assert_eq!(spawn_effect_key(&t, Phase::Design, 0),     "spawn:PVTI_x:design:0");
    }

    #[test] fn diff_back_produces_different_effect_key() {
        let t = TaskId::new("PVTI_y");
        let k1 = spawn_effect_key(&t, Phase::ImplVerify, 0);
        let k2 = spawn_effect_key(&t, Phase::ImplVerify, 1);
        assert_ne!(k1, k2);
    }
}
```

- [ ] **Step 2: lib.rs**

```rust
pub mod key;
pub use key::{
    event_key_derived, event_key_gh_delivery, event_key_gh_issue, event_key_gh_status,
    event_key_slack, spawn_effect_key, column_move_effect_key, slack_post_effect_key,
};
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-core key`
Expected: `3 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): event_key/effect_key generation with attempt discriminator"
```

---

### Task B7: DomainEvent + bus envelope

**Files:**
- Create: `crates/totsuka-core/src/event.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: serde, chrono
- Produces: `DomainEvent`, `EventEnvelope`, `Source` enum

- [ ] **Step 1: 実装**

`crates/totsuka-core/src/event.rs`:
```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source { Github, Slack, Internal }

/// 内部表現 (型安全な domain 層)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    pub event_key:    String,
    pub source:       Source,
    #[serde(rename = "type")]
    pub event_type:   String,                          // 例: "github.status_changed"
    pub payload:      serde_json::Value,
}

/// bus に流す envelope (spec §7 bus envelope)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_key:    String,
    pub source:       Source,
    #[serde(rename = "type")]
    pub event_type:   String,
    pub published_at: DateTime<Utc>,
    pub trace_id:     Option<String>,
    pub payload:      serde_json::Value,
}

impl EventEnvelope {
    pub fn from_domain(e: DomainEvent, published_at: DateTime<Utc>, trace_id: Option<String>) -> Self {
        Self {
            event_key: e.event_key, source: e.source, event_type: e.event_type,
            published_at, trace_id, payload: e.payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test] fn envelope_roundtrip() {
        let de = DomainEvent {
            event_key: "gh:delivery:d1".into(),
            source: Source::Github,
            event_type: "github.status_changed".into(),
            payload: serde_json::json!({"to_status": "design"}),
        };
        let ts = Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap();
        let env = EventEnvelope::from_domain(de, ts, Some("trace-1".into()));
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"github.status_changed\""));
        let parsed: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.event_key, "gh:delivery:d1");
        assert_eq!(parsed.source, Source::Github);
    }
}
```

- [ ] **Step 2: lib.rs**

```rust
pub mod event;
pub use event::{DomainEvent, EventEnvelope, Source};
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-core event`
Expected: `1 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): DomainEvent + EventEnvelope (bus shape)"
```

---

### Task B8: NotifyKind enum (typed payload は後続)

**Files:**
- Create: `crates/totsuka-core/src/notify.rs`
- Modify: `crates/totsuka-core/src/lib.rs`

**Interfaces:**
- Consumes: serde
- Produces: `NotifyKind` enum (15 variants per spec §13.1)、`as_snake()`

- [ ] **Step 1: 実装**

`crates/totsuka-core/src/notify.rs`:
```rust
use serde::{Deserialize, Serialize};

/// spec §13.1: 通知種別。NotifyPayload は totsuka-telemetry 側で持つ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyKind {
    HumanGate1,
    HumanGate2,
    TaskFailed,
    TaskStuck,
    GivingUp,
    ProcessDead,
    ProcessUnhealthy,
    PgmqDead,
    ConfigError,
    SecretRotationWarn,
    WritebackConflict,
    ArgvSecretViolation,
    QaSpawnFailed,
    QaAnswerTimeout,
    WorktreeGcAlert,
}

impl NotifyKind {
    pub fn as_snake(&self) -> &'static str {
        // serde 表現と同じ形式
        match self {
            NotifyKind::HumanGate1            => "human_gate1",
            NotifyKind::HumanGate2            => "human_gate2",
            NotifyKind::TaskFailed            => "task_failed",
            NotifyKind::TaskStuck             => "task_stuck",
            NotifyKind::GivingUp              => "giving_up",
            NotifyKind::ProcessDead           => "process_dead",
            NotifyKind::ProcessUnhealthy     => "process_unhealthy",
            NotifyKind::PgmqDead              => "pgmq_dead",
            NotifyKind::ConfigError           => "config_error",
            NotifyKind::SecretRotationWarn   => "secret_rotation_warn",
            NotifyKind::WritebackConflict    => "writeback_conflict",
            NotifyKind::ArgvSecretViolation  => "argv_secret_violation",
            NotifyKind::QaSpawnFailed         => "qa_spawn_failed",
            NotifyKind::QaAnswerTimeout      => "qa_answer_timeout",
            NotifyKind::WorktreeGcAlert      => "worktree_gc_alert",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn all_variants_have_unique_snake() {
        let all = [
            NotifyKind::HumanGate1, NotifyKind::HumanGate2, NotifyKind::TaskFailed,
            NotifyKind::TaskStuck, NotifyKind::GivingUp, NotifyKind::ProcessDead,
            NotifyKind::ProcessUnhealthy, NotifyKind::PgmqDead, NotifyKind::ConfigError,
            NotifyKind::SecretRotationWarn, NotifyKind::WritebackConflict,
            NotifyKind::ArgvSecretViolation, NotifyKind::QaSpawnFailed,
            NotifyKind::QaAnswerTimeout, NotifyKind::WorktreeGcAlert,
        ];
        let s: std::collections::HashSet<_> = all.iter().map(|k| k.as_snake()).collect();
        assert_eq!(s.len(), all.len(), "all snake forms must be unique");
    }

    #[test] fn snake_form_matches_serde() {
        let k = NotifyKind::TaskStuck;
        let j = serde_json::to_string(&k).unwrap();
        assert_eq!(j, format!("\"{}\"", k.as_snake()));
    }
}
```

- [ ] **Step 2: lib.rs**

```rust
pub mod notify;
pub use notify::NotifyKind;
```

- [ ] **Step 3: テスト pass + clippy clean**

Run: `cargo test -p totsuka-core && cargo clippy -p totsuka-core -- -D warnings`
Expected: 全テスト pass、clippy 警告ゼロ

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-core/
git commit -m "feat(core): NotifyKind enum (15 variants per §13.1)"
```

---

### Task C1: totsuka-config スケルトン + 変数展開

**Files:**
- Create: `crates/totsuka-config/Cargo.toml`
- Create: `crates/totsuka-config/src/lib.rs`
- Create: `crates/totsuka-config/src/expand.rs`

**Interfaces:**
- Consumes: totsuka-core
- Produces: `expand_vars(s, vars, env) -> Result<String, ExpandError>` — `${name}` / `${env:NAME}` を展開、未定義/循環でエラー

- [ ] **Step 1: Cargo.toml + lib.rs**

`crates/totsuka-config/Cargo.toml`:
```toml
[package]
name = "totsuka-config"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
totsuka-core = { path = "../totsuka-core" }
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
thiserror.workspace = true
regex.workspace = true
chrono.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

`crates/totsuka-config/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod expand;
pub use expand::{expand_vars, ExpandError};
```

- [ ] **Step 2: expand.rs (テスト先行)**

`crates/totsuka-config/src/expand.rs`:
```rust
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExpandError {
    #[error("undefined variable: {0}")]
    Undefined(String),
    #[error("undefined env variable: {0}")]
    UndefinedEnv(String),
    #[error("cyclic reference involving: {0}")]
    Cycle(String),
}

fn re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\$\{([a-zA-Z0-9_:.\-]+)\}").unwrap())
}

/// `${name}` を vars から、`${env:NAME}` を env_lookup から展開する。
/// vars 内の相互参照も解決する (循環は ExpandError::Cycle)
pub fn expand_vars<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
) -> Result<String, ExpandError>
where F: Fn(&str) -> Option<String>,
{
    expand_inner(input, vars, env_lookup, &mut HashSet::new())
}

fn expand_inner<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    visiting: &mut HashSet<String>,
) -> Result<String, ExpandError>
where F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let mut last = 0usize;
    for cap in re().captures_iter(input) {
        let m   = cap.get(0).unwrap();
        let key = cap.get(1).unwrap().as_str();
        out.push_str(&input[last..m.start()]);
        let replaced = if let Some(env_name) = key.strip_prefix("env:") {
            env_lookup(env_name).ok_or_else(|| ExpandError::UndefinedEnv(env_name.into()))?
        } else {
            if !visiting.insert(key.to_string()) {
                return Err(ExpandError::Cycle(key.into()));
            }
            let v = vars.get(key).ok_or_else(|| ExpandError::Undefined(key.into()))?;
            let r = expand_inner(v, vars, env_lookup, visiting)?;
            visiting.remove(key);
            r
        };
        out.push_str(&replaced);
        last = m.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty_env() -> impl Fn(&str) -> Option<String> { |_| None }

    #[test] fn plain_passthrough() {
        let vars = HashMap::new();
        assert_eq!(expand_vars("/no/vars/here", &vars, &empty_env()).unwrap(), "/no/vars/here");
    }

    #[test] fn simple_var() {
        let mut v = HashMap::new();
        v.insert("work".into(), "/home/u/work".into());
        assert_eq!(expand_vars("${work}/repos", &v, &empty_env()).unwrap(), "/home/u/work/repos");
    }

    #[test] fn env_var() {
        let v = HashMap::new();
        let env = |k: &str| if k == "HOME" { Some("/h".into()) } else { None };
        assert_eq!(expand_vars("${env:HOME}/x", &v, &env).unwrap(), "/h/x");
    }

    #[test] fn nested_vars() {
        let mut v = HashMap::new();
        v.insert("a".into(), "/x/${b}".into());
        v.insert("b".into(), "/y".into());
        assert_eq!(expand_vars("${a}", &v, &empty_env()).unwrap(), "/x/y");
    }

    #[test] fn undefined_errors() {
        let v = HashMap::new();
        assert_eq!(expand_vars("${nope}", &v, &empty_env()).unwrap_err(), ExpandError::Undefined("nope".into()));
    }

    #[test] fn undefined_env_errors() {
        let v = HashMap::new();
        assert_eq!(expand_vars("${env:MISSING}", &v, &empty_env()).unwrap_err(), ExpandError::UndefinedEnv("MISSING".into()));
    }

    #[test] fn cycle_errors() {
        let mut v = HashMap::new();
        v.insert("a".into(), "${b}".into());
        v.insert("b".into(), "${a}".into());
        match expand_vars("${a}", &v, &empty_env()).unwrap_err() {
            ExpandError::Cycle(_) => (),
            e => panic!("expected Cycle, got {:?}", e),
        }
    }
}
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-config expand`
Expected: `7 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-config/
git commit -m "feat(config): ${var}/${env:NAME} expansion with cycle detection"
```

---

### Task C2: Config スキーマ (top-level + sections)

**Files:**
- Create: `crates/totsuka-config/src/schema.rs`
- Modify: `crates/totsuka-config/src/lib.rs`

**Interfaces:**
- Consumes: serde, toml, totsuka-core (Secret, ColumnId)
- Produces: `Config` struct (全 section)、`Config::from_toml_str`

- [ ] **Step 1: schema.rs (主要セクションのみ、長いので段階的)**

`crates/totsuka-config/src/schema.rs`:
```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use totsuka_core::{ColumnId, Secret};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub totsuka:        TotsukaSection,
    pub supervisor:     SupervisorSection,
    pub postgres:       PostgresSection,
    pub bus:            BusSection,
    pub agent_adapter:  AgentAdapterSection,
    pub orchestrator:   OrchestratorSection,
    pub github:         GithubSection,
    pub github_watcher: GithubWatcherSection,
    pub qa_service:     QaServiceSection,
    pub notifications:  NotificationsSection,
    pub retention:      RetentionSection,
    pub telemetry:      TelemetrySection,
    #[serde(default)]
    pub secrets:        SecretsSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotsukaSection {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub state_dir: String,
    pub data_dir:  String,
    #[serde(default = "default_tz")]
    pub timezone:  String,
}
fn default_log_level() -> String { "info".into() }
fn default_tz()        -> String { "Asia/Tokyo".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorSection {
    #[serde(default = "d_30")] pub ready_timeout_secs: u64,
    #[serde(default = "d_15")] pub shutdown_grace_secs: u64,
    #[serde(default = "d_5")]  pub shutdown_kill_secs: u64,
    #[serde(default)]          pub recreate_on_image_mismatch: bool,
    pub heartbeat: HeartbeatSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatSection {
    #[serde(default = "d_5")]   pub healthz_interval_secs: u64,
    #[serde(default = "d_30")]  pub readyz_interval_secs:  u64,
    #[serde(default = "d_30")]  pub pgmq_interval_secs:    u64,
    #[serde(default = "d_3")]   pub unhealthy_threshold:   u32,
    #[serde(default = "d_2")]   pub degraded_threshold:    u32,
    #[serde(default = "d_restart_policy")] pub restart_policy: String,
    #[serde(default = "d_backoff")] pub restart_backoff_secs: Vec<u64>,
    #[serde(default = "d_5")]   pub restart_max_attempts:  u32,
    #[serde(default)]           pub notify_on_degraded:    bool,
}
fn d_restart_policy() -> String { "on-dead-only".into() }
fn d_backoff() -> Vec<u64> { vec![5, 15, 60] }
fn d_2() -> u32 { 2 } fn d_3() -> u32 { 3 } fn d_5<T: From<u32>>() -> T { T::from(5) }
fn d_15() -> u64 { 15 } fn d_30() -> u64 { 30 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresSection {
    pub image:        String,
    pub container:    String,
    pub host:         String,
    pub port:         u16,
    pub database:     String,
    pub user:         String,
    pub volume:       String,
    pub compose_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusSection {
    pub queue_name:       String,
    #[serde(default = "d_30")]  pub visibility_secs: u64,
    #[serde(default = "d_bs")]  pub batch_size:      u32,
    #[serde(default = "d_pi")]  pub poll_interval_ms: u64,
}
fn d_bs() -> u32 { 16 } fn d_pi() -> u64 { 200 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAdapterSection {
    pub uds_path:      String,
    #[serde(default)]  pub tcp_bind: String,
    pub herdr_socket:  String,
    pub node_capacity: u32,
    pub repos_root:    String,
    pub auto_clone:    bool,
    #[serde(default = "d_72")]   pub worktree_failed_ttl_hours: u64,
    #[serde(default = "d_3600")] pub worktree_orphan_scan_interval_secs: u64,
    #[serde(default)]            pub vars:  HashMap<String, String>,
    #[serde(default)]            pub repos: HashMap<String, RepoSection>,
}
fn d_72() -> u64 { 72 } fn d_3600() -> u64 { 3600 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSection {
    pub description:                String,
    #[serde(default)] pub repo_path:        Option<String>,
    #[serde(default)] pub worktree_subdir:  Option<String>,
    #[serde(default)] pub worktree_path:    Option<String>,
    #[serde(default)] pub default_branch:   Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSection {
    pub uds_path:                       String,
    pub wip_global:                     u32,
    pub phase_timeout_default_secs:     u64,
    #[serde(default)] pub phase_timeout: HashMap<String, u64>,
    pub retry_max:                      u32,
    pub stuck_threshold_secs:           u64,
    pub adapter_uds:                    String,
    #[serde(default)] pub claude_argv:  ClaudeArgvSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeArgvSection {
    #[serde(default)] pub global:    Vec<String>,
    #[serde(default)] pub per_repo:  HashMap<String, ClaudeArgvExtra>,
    #[serde(default)] pub per_phase: HashMap<String, ClaudeArgvExtra>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeArgvExtra {
    #[serde(default)] pub extra: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubSection {
    pub project_owner:  String,
    pub project_number: u64,
    #[serde(default = "d_status")] pub status_field: String,
    pub columns: HashMap<ColumnId, String>,
}
fn d_status() -> String { "Status".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubWatcherSection {
    pub bind: String,
    #[serde(default = "d_20")] pub project_poll_interval_secs: u64,
    #[serde(default = "d_60")] pub issues_poll_interval_secs:  u64,
    #[serde(default = "d_24")] pub catchup_window_hours:       u64,
    #[serde(default = "d_100")] pub graphql_page_size:         u32,
}
fn d_20() -> u64 { 20 } fn d_60() -> u64 { 60 } fn d_24() -> u64 { 24 } fn d_100() -> u32 { 100 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaServiceSection {
    pub uds_path:         String,
    pub allowed_user_ids: Vec<String>,
    pub catchup_channels: Vec<String>,
    pub reaction_trigger: String,
    pub default_mode:     String,           // "auto" | "delegated"
    pub adapter_uds:      String,
    #[serde(default = "d_llm")] pub repo_select_mode: String,
    pub classifier: ClassifierSection,
    pub answer:     AnswerSection,
}
fn d_llm() -> String { "llm_classify".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierSection {
    pub provider: String,                  // anthropic | openai | openrouter | litellm | openai_compatible
    pub model:    String,
    #[serde(default)] pub api_base: String,
    #[serde(default = "d_256")] pub max_tokens: u32,
    #[serde(default = "d_th")]  pub confidence_threshold: f64,
    #[serde(default = "d_tc")]  pub top_candidates: u32,
    #[serde(default = "d_low")] pub on_low_confidence: String,
    #[serde(default = "d_true")] pub include_thread_context: bool,
    #[serde(default = "d_15")]   pub request_timeout_secs: u64,
}
fn d_256() -> u32 { 256 } fn d_th() -> f64 { 0.70 } fn d_tc() -> u32 { 3 }
fn d_low() -> String { "delegated_reaction".into() } fn d_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSection {
    #[serde(default = "d_sentinel")] pub sentinel: String,
    #[serde(default = "d_open")]     pub answer_open_tag: String,
    #[serde(default = "d_close")]    pub answer_close_tag: String,
    #[serde(default = "d_1500")]     pub poll_interval_ms: u64,
    #[serde(default = "d_8")]        pub stable_revision_secs: u64,
    #[serde(default = "d_180")]      pub answer_timeout_secs: u64,
    #[serde(default = "d_1800")]     pub pane_idle_ttl_secs: u64,
    #[serde(default = "d_4")]        pub max_concurrent_answers: u32,
}
fn d_sentinel() -> String { "<<TOTSUKA_DONE>>".into() }
fn d_open()  -> String { "<answer>".into() }
fn d_close() -> String { "</answer>".into() }
fn d_1500() -> u64 { 1500 } fn d_8() -> u64 { 8 } fn d_180() -> u64 { 180 }
fn d_1800() -> u64 { 1800 } fn d_4() -> u32 { 4 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsSection {
    #[serde(default = "d_true")] pub config_error_notify: bool,
    #[serde(default = "d_600")]  pub dedup_default_secs: u64,
    #[serde(default = "d_30")]   pub rate_limit_per_min: u32,
    #[serde(default)]            pub dedup_ttl_secs: HashMap<String, u64>,
    #[serde(default)]            pub slack: SlackNotifySection,
    #[serde(default)]            pub github: GithubNotifySection,
}
fn d_600() -> u64 { 600 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlackNotifySection {
    #[serde(default)] pub webhook_url:           String,
    #[serde(default)] pub default_channel:       String,
    #[serde(default)] pub channel_overrides:     HashMap<String, String>,
    #[serde(default = "d_10")] pub bucket_capacity: u32,
    #[serde(default = "d_5")]  pub bucket_refill_per_min: u32,
}
fn d_10() -> u32 { 10 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubNotifySection {
    #[serde(default)] pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionSection {
    #[serde(default = "d_4")]    pub events_weeks: u32,
    #[serde(default = "d_30u")]  pub snapshot_days: u32,
    #[serde(default = "d_1024")] pub logs_max_mb: u32,
    #[serde(default = "d_50")]   pub log_file_max_mb: u32,
}
fn d_30u() -> u32 { 30 } fn d_1024() -> u32 { 1024 } fn d_50() -> u32 { 50 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySection {
    #[serde(default = "d_true")] pub metrics_enabled:    bool,
    #[serde(default)]            pub otlp_endpoint:      String,
    #[serde(default = "d_ratio")] pub trace_sample_ratio: f64,
}
fn d_ratio() -> f64 { 0.1 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretsSection {
    #[serde(default = "d_secret_days")] pub rotation_warn_days: u32,
}
fn d_secret_days() -> u32 { 30 }

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_TOML: &str = r#"
[totsuka]
state_dir = "/tmp/state"
data_dir  = "/tmp/data"

[supervisor]
[supervisor.heartbeat]

[postgres]
image="ghcr.io/pgmq/pg18-pgmq:v1.10.0"
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
project_owner="org"
project_number=1
[github.columns]
inbox="📥 Inbox"
ready="📋 Ready"
design="🤖 調査・設計"
design_review="🚧 設計レビュー"
impl_verify="🤖 実装・受入検証"
final_review="🚧 最終レビュー"
awaiting_release="🚀 リリース待ち"
released="🏁 完了"

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

    #[test] fn parses_minimal_config() {
        let c = Config::from_toml_str(MIN_TOML).expect("parse");
        assert_eq!(c.totsuka.timezone, "Asia/Tokyo");          // default applied
        assert_eq!(c.bus.batch_size, 16);                       // default applied
        assert_eq!(c.github.columns.len(), 8);
        assert_eq!(c.github.columns.get(&ColumnId::Design).unwrap(), "🤖 調査・設計");
        assert_eq!(c.agent_adapter.worktree_failed_ttl_hours, 72);
    }

    #[test] fn missing_required_field_errors() {
        let bad = MIN_TOML.replace(r#"queue_name="totsuka_events""#, "");
        assert!(Config::from_toml_str(&bad).is_err());
    }
}
```

- [ ] **Step 2: lib.rs に schema を export**

`crates/totsuka-config/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod expand;
pub mod schema;
pub use expand::{expand_vars, ExpandError};
pub use schema::Config;
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-config schema`
Expected: `2 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-config/
git commit -m "feat(config): full Config schema with serde defaults"
```

---

### Task C3: バリデーション (排他 / 必須 / リポ詳細)

**Files:**
- Create: `crates/totsuka-config/src/validate.rs`
- Modify: `crates/totsuka-config/src/lib.rs`

**Interfaces:**
- Consumes: schema::Config
- Produces: `Config::validate(&self) -> Result<(), Vec<ValidationError>>`

- [ ] **Step 1: 実装**

`crates/totsuka-config/src/validate.rs`:
```rust
use crate::schema::Config;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("repo {repo}: worktree_subdir and worktree_path are mutually exclusive (both set)")]
    WorktreeBothSet { repo: String },
    #[error("repo {repo}: must set exactly one of worktree_subdir or worktree_path (none set)")]
    WorktreeNeitherSet { repo: String },
    #[error("repo {repo}: description is required (empty)")]
    RepoDescriptionEmpty { repo: String },
    #[error("github.columns must cover all 8 ColumnId values (have {0}, need 8)")]
    ColumnsCoverage(usize),
    #[error("default_mode must be 'auto' or 'delegated' (got {0})")]
    InvalidQaMode(String),
    #[error("classifier.provider must be anthropic|openai|openrouter|litellm|openai_compatible (got {0})")]
    InvalidProvider(String),
    #[error("classifier.api_base required for provider {0}")]
    ApiBaseRequired(String),
    #[error("classifier.confidence_threshold must be in [0.0, 1.0] (got {0})")]
    InvalidThreshold(f64),
    #[error("supervisor.heartbeat.restart_policy must be one of on-dead-only|on-unhealthy|never (got {0})")]
    InvalidRestartPolicy(String),
    #[error("agent_adapter.uds_path and orchestrator.uds_path must differ")]
    UdsCollision,
}

impl Config {
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();

        // worktree 排他
        for (repo, r) in &self.agent_adapter.repos {
            if r.description.is_empty() {
                errs.push(ValidationError::RepoDescriptionEmpty { repo: repo.clone() });
            }
            match (&r.worktree_subdir, &r.worktree_path) {
                (Some(_), Some(_)) => errs.push(ValidationError::WorktreeBothSet { repo: repo.clone() }),
                (None, None)       => errs.push(ValidationError::WorktreeNeitherSet { repo: repo.clone() }),
                _ => {}
            }
        }

        // ColumnId 8 値カバレッジ (deserialize でも見ているが念押し)
        if self.github.columns.len() != 8 {
            errs.push(ValidationError::ColumnsCoverage(self.github.columns.len()));
        }

        // qa default_mode
        if !matches!(self.qa_service.default_mode.as_str(), "auto" | "delegated") {
            errs.push(ValidationError::InvalidQaMode(self.qa_service.default_mode.clone()));
        }

        // provider
        let p = &self.qa_service.classifier.provider;
        if !matches!(p.as_str(), "anthropic" | "openai" | "openrouter" | "litellm" | "openai_compatible") {
            errs.push(ValidationError::InvalidProvider(p.clone()));
        }
        if matches!(p.as_str(), "litellm" | "openai_compatible") && self.qa_service.classifier.api_base.is_empty() {
            errs.push(ValidationError::ApiBaseRequired(p.clone()));
        }

        // threshold range
        let th = self.qa_service.classifier.confidence_threshold;
        if !(0.0..=1.0).contains(&th) {
            errs.push(ValidationError::InvalidThreshold(th));
        }

        // restart_policy
        let rp = &self.supervisor.heartbeat.restart_policy;
        if !matches!(rp.as_str(), "on-dead-only" | "on-unhealthy" | "never") {
            errs.push(ValidationError::InvalidRestartPolicy(rp.clone()));
        }

        // UDS 衝突 (代表ペアのみチェック)
        if self.agent_adapter.uds_path == self.orchestrator.uds_path {
            errs.push(ValidationError::UdsCollision);
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Config, RepoSection};

    fn baseline() -> Config {
        let toml = include_str!("test_min.toml");
        Config::from_toml_str(toml).unwrap()
    }

    #[test] fn baseline_validates() {
        baseline().validate().expect("baseline should validate");
    }

    #[test] fn worktree_both_set_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert("org/r".into(), RepoSection {
            description: "x".into(),
            repo_path: None,
            worktree_subdir: Some(".w".into()),
            worktree_path: Some("/tmp".into()),
            default_branch: None,
        });
        let e = c.validate().unwrap_err();
        assert!(e.contains(&ValidationError::WorktreeBothSet { repo: "org/r".into() }));
    }

    #[test] fn worktree_neither_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert("org/r".into(), RepoSection {
            description: "x".into(),
            repo_path: None, worktree_subdir: None, worktree_path: None, default_branch: None,
        });
        assert!(c.validate().unwrap_err().contains(&ValidationError::WorktreeNeitherSet { repo: "org/r".into() }));
    }

    #[test] fn empty_description_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert("org/r".into(), RepoSection {
            description: "".into(),
            repo_path: None,
            worktree_subdir: Some(".w".into()),
            worktree_path: None,
            default_branch: None,
        });
        assert!(c.validate().unwrap_err().iter().any(|e| matches!(e, ValidationError::RepoDescriptionEmpty{..})));
    }

    #[test] fn invalid_provider_errors() {
        let mut c = baseline();
        c.qa_service.classifier.provider = "bogus".into();
        assert!(c.validate().unwrap_err().iter().any(|e| matches!(e, ValidationError::InvalidProvider(_))));
    }

    #[test] fn litellm_requires_api_base() {
        let mut c = baseline();
        c.qa_service.classifier.provider = "litellm".into();
        c.qa_service.classifier.api_base = "".into();
        assert!(c.validate().unwrap_err().iter().any(|e| matches!(e, ValidationError::ApiBaseRequired(_))));
    }

    #[test] fn threshold_range_errors() {
        let mut c = baseline();
        c.qa_service.classifier.confidence_threshold = 1.5;
        assert!(c.validate().unwrap_err().iter().any(|e| matches!(e, ValidationError::InvalidThreshold(_))));
    }

    #[test] fn restart_policy_validates() {
        let mut c = baseline();
        c.supervisor.heartbeat.restart_policy = "wrong".into();
        assert!(c.validate().unwrap_err().iter().any(|e| matches!(e, ValidationError::InvalidRestartPolicy(_))));
    }

    #[test] fn uds_collision_errors() {
        let mut c = baseline();
        c.agent_adapter.uds_path = "/same".into();
        c.orchestrator.uds_path  = "/same".into();
        assert!(c.validate().unwrap_err().contains(&ValidationError::UdsCollision));
    }
}
```

- [ ] **Step 2: test fixture を置く**

`crates/totsuka-config/src/test_min.toml` に C2 の `MIN_TOML` 内容と同じものを保存:
```bash
# C2 の MIN_TOML を抜き出して保存 (heredoc 推奨)
```

> 実装者注: C2 テスト内の `MIN_TOML` 定数を `include_str!` で読めるよう、同じ内容を `src/test_min.toml` に置く。C2 のテストは `MIN_TOML` 定数を `include_str!("test_min.toml")` に変更してもよい (共通化)。

- [ ] **Step 3: lib.rs**

```rust
pub mod validate;
pub use validate::ValidationError;
```

- [ ] **Step 4: テスト pass**

Run: `cargo test -p totsuka-config validate`
Expected: `9 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-config/
git commit -m "feat(config): validation (worktree exclusivity, provider, threshold, UDS, ...)"
```

---

### Task C4: env override (TOTSUKA__SECTION__KEY)

**Files:**
- Create: `crates/totsuka-config/src/env_override.rs`
- Modify: `crates/totsuka-config/src/lib.rs`

**Interfaces:**
- Consumes: schema::Config (toml::Value 経由で merge)
- Produces: `apply_env_overrides(toml_value, env_iter) -> toml::Value`

- [ ] **Step 1: 実装 (envar table merge)**

`crates/totsuka-config/src/env_override.rs`:
```rust
use toml::Value;

/// `TOTSUKA__<SECTION>__<KEY>=value` を TOML Value に差し込む。
/// 値は文字列・整数・bool として best-effort で解釈し、それ以外は文字列のままセット。
pub fn apply_env_overrides<I>(mut root: Value, env: I) -> Value
where I: IntoIterator<Item = (String, String)>,
{
    for (k, v) in env {
        let Some(path) = k.strip_prefix("TOTSUKA__") else { continue };
        let parts: Vec<&str> = path.split("__").collect();
        let lowered: Vec<String> = parts.iter().map(|p| p.to_ascii_lowercase()).collect();
        let parsed = parse_scalar(&v);
        set_path(&mut root, &lowered, parsed);
    }
    root
}

fn parse_scalar(v: &str) -> Value {
    if let Ok(i) = v.parse::<i64>()  { return Value::Integer(i); }
    if let Ok(f) = v.parse::<f64>()  { return Value::Float(f); }
    if v.eq_ignore_ascii_case("true")  { return Value::Boolean(true); }
    if v.eq_ignore_ascii_case("false") { return Value::Boolean(false); }
    Value::String(v.to_string())
}

fn set_path(root: &mut Value, path: &[String], val: Value) {
    if path.is_empty() { return; }
    let table = match root.as_table_mut() {
        Some(t) => t,
        None    => return, // root が table でなければ無視
    };
    if path.len() == 1 {
        table.insert(path[0].clone(), val);
        return;
    }
    let entry = table.entry(path[0].clone()).or_insert_with(|| Value::Table(Default::default()));
    set_path(entry, &path[1..], val);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn overrides_scalar() {
        let base: Value = toml::from_str(r#"
[bus]
batch_size = 16
"#).unwrap();
        let v = apply_env_overrides(base, vec![("TOTSUKA__BUS__BATCH_SIZE".into(), "64".into())]);
        assert_eq!(v["bus"]["batch_size"].as_integer(), Some(64));
    }

    #[test] fn creates_missing_path() {
        let base: Value = toml::from_str("[totsuka]\nstate_dir=\"/x\"\n").unwrap();
        let v = apply_env_overrides(base, vec![("TOTSUKA__TELEMETRY__OTLP_ENDPOINT".into(), "http://otel:4317".into())]);
        assert_eq!(v["telemetry"]["otlp_endpoint"].as_str(), Some("http://otel:4317"));
    }

    #[test] fn ignores_non_totsuka_env() {
        let base: Value = toml::from_str("[bus]\nbatch_size=16\n").unwrap();
        let v = apply_env_overrides(base, vec![("HOME".into(), "/h".into())]);
        assert_eq!(v["bus"]["batch_size"].as_integer(), Some(16));
    }
}
```

- [ ] **Step 2: lib.rs**

```rust
pub mod env_override;
pub use env_override::apply_env_overrides;
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-config env_override`
Expected: `3 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-config/
git commit -m "feat(config): TOTSUKA__SECTION__KEY env override"
```

---

### Task D1: totsuka-telemetry スケルトン + tracing init

**Files:**
- Create: `crates/totsuka-telemetry/Cargo.toml`
- Create: `crates/totsuka-telemetry/src/lib.rs`
- Create: `crates/totsuka-telemetry/src/log.rs`

**Interfaces:**
- Consumes: tracing, tracing-subscriber, tracing-appender
- Produces: `init_tracing(state_dir, bin_name, level)` — JSON 形式の構造化ログ + daily rotation file

- [ ] **Step 1: Cargo.toml**

`crates/totsuka-telemetry/Cargo.toml`:
```toml
[package]
name = "totsuka-telemetry"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
totsuka-core   = { path = "../totsuka-core" }
totsuka-config = { path = "../totsuka-config" }
tokio          = { workspace = true }
tracing        = { workspace = true }
tracing-subscriber = { workspace = true }
tracing-appender = { workspace = true }
axum           = { workspace = true }
tower          = { workspace = true }
hyper          = { workspace = true }
serde          = { workspace = true }
serde_json     = { workspace = true }
reqwest        = { workspace = true }
chrono         = { workspace = true }
thiserror      = { workspace = true }
uuid           = { workspace = true }

[dev-dependencies]
tempfile = "3.12"
```

- [ ] **Step 2: log.rs**

`crates/totsuka-telemetry/src/log.rs`:
```rust
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 構造化ログ初期化。stdout (json) + daily rotation file。
/// 返り値の WorkerGuard は main 関数の最後まで保持する (drop で flush)
pub fn init_tracing(state_dir: &Path, bin_name: &str, default_level: &str) -> WorkerGuard {
    let log_dir = state_dir.join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");
    let file_appender = tracing_appender::rolling::daily(&log_dir, format!("{bin_name}.log"));
    let (nb, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let stdout_layer = fmt::layer().json().with_target(true).with_current_span(false);
    let file_layer   = fmt::layer().json().with_writer(nb).with_target(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test] fn init_creates_log_dir_and_file() {
        let dir = tempdir().unwrap();
        let _guard = init_tracing(dir.path(), "smoke", "info");
        tracing::info!("hello");
        // 非同期 flush なので即時にはファイルが書かれない可能性あり。dir が出来ていることだけ確認
        assert!(dir.path().join("logs").exists());
    }
}
```

- [ ] **Step 3: lib.rs**

`crates/totsuka-telemetry/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod log;
pub use log::init_tracing;
```

- [ ] **Step 4: テスト pass**

Run: `cargo test -p totsuka-telemetry log`
Expected: `1 passed`

> 注: tracing は global subscriber を使うため、同一プロセスで複数回 init は警告。テストは 1 つだけ。

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-telemetry/
git commit -m "feat(telemetry): tracing init with daily rotation + JSON layer"
```

---

### Task D2: request_id middleware + healthz/readyz axum router

**Files:**
- Create: `crates/totsuka-telemetry/src/request_id.rs`
- Create: `crates/totsuka-telemetry/src/http.rs`
- Modify: `crates/totsuka-telemetry/src/lib.rs`

**Interfaces:**
- Consumes: axum, tower, uuid
- Produces: `RequestIdLayer` middleware、`health_router() -> Router<Arc<HealthState>>` (healthz/readyz/metrics スタブ)

- [ ] **Step 1: request_id.rs**

`crates/totsuka-telemetry/src/request_id.rs`:
```rust
use axum::{extract::Request, middleware::Next, response::Response};
use uuid::Uuid;

pub const HEADER: &str = "x-totsuka-request-id";

/// 着信時に request-id を取得、なければ生成。response にも echo
pub async fn middleware(mut req: Request, next: Next) -> Response {
    let id = req.headers().get(HEADER).and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut res = next.run(req).await;
    res.headers_mut().insert(HEADER, id.parse().unwrap());
    res
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);
```

- [ ] **Step 2: http.rs**

`crates/totsuka-telemetry/src/http.rs`:
```rust
use std::sync::Arc;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json, Router, routing::get};
use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub struct HealthState {
    inner: Arc<RwLock<HealthInner>>,
}

#[derive(Default)]
struct HealthInner {
    ready: bool,
    checks: HashMap<String, String>, // name -> "ok" / "fail: <msg>"
}

impl HealthState {
    pub fn new() -> Self { Self::default() }

    pub async fn set_check(&self, name: &str, status: &str) {
        self.inner.write().await.checks.insert(name.into(), status.into());
    }
    pub async fn set_ready(&self, ready: bool) {
        self.inner.write().await.ready = ready;
    }
}

#[derive(Serialize)]
struct ReadyResponse<'a> {
    ready:  bool,
    checks: &'a HashMap<String, String>,
}

pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz",  get(readyz))
        .route("/metrics", get(metrics_stub))
        .with_state(state)
        .layer(axum::middleware::from_fn(crate::request_id::middleware))
}

async fn healthz() -> impl IntoResponse { (StatusCode::OK, "ok") }

async fn readyz(State(s): State<HealthState>) -> impl IntoResponse {
    let g = s.inner.read().await;
    let body = serde_json::to_string(&ReadyResponse { ready: g.ready, checks: &g.checks }).unwrap();
    let code = if g.ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, [(axum::http::header::CONTENT_TYPE, "application/json")], body)
}

async fn metrics_stub() -> impl IntoResponse {
    // 実装は D3 で差し替え
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain")], "# HELP placeholder\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test] async fn healthz_returns_ok() {
        let app = router(HealthState::new());
        let res = app.oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test] async fn readyz_starts_not_ready() {
        let st = HealthState::new();
        let app = router(st.clone());
        let res = app.oneshot(Request::builder().uri("/readyz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test] async fn readyz_ok_after_set_ready() {
        let st = HealthState::new();
        st.set_ready(true).await;
        st.set_check("db", "ok").await;
        let app = router(st.clone());
        let res = app.oneshot(Request::builder().uri("/readyz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test] async fn request_id_echoed() {
        let app = router(HealthState::new());
        let res = app.oneshot(
            Request::builder().uri("/healthz")
              .header(crate::request_id::HEADER, "test-id-1")
              .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.headers().get(crate::request_id::HEADER).unwrap(), "test-id-1");
    }
}
```

- [ ] **Step 3: lib.rs**

```rust
pub mod request_id;
pub mod http;
pub use http::{HealthState, router as health_router};
```

- [ ] **Step 4: テスト pass**

Run: `cargo test -p totsuka-telemetry http`
Expected: `4 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-telemetry/
git commit -m "feat(telemetry): healthz/readyz axum router + request_id middleware"
```

---

### Task D3: Notifier core (NotifyKind + dedup state + persistence)

**Files:**
- Create: `crates/totsuka-telemetry/src/notify/mod.rs`
- Create: `crates/totsuka-telemetry/src/notify/payload.rs`
- Create: `crates/totsuka-telemetry/src/notify/routing.rs`
- Modify: `crates/totsuka-telemetry/src/lib.rs`

**Interfaces:**
- Consumes: totsuka-core::{NotifyKind, Clock}, serde, tokio fs
- Produces: `Notifier::new(clock, state_path, sinks, routing) -> Notifier`、`notifier.notify(kind, dedup_key, payload).await`

- [ ] **Step 1: payload.rs**

`crates/totsuka-telemetry/src/notify/payload.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyPayload {
    pub title:    String,
    pub body:     String,
    #[serde(default)] pub fields: Vec<(String, String)>,
    #[serde(default)] pub link:   Option<String>,
    #[serde(default)] pub trace_id: Option<String>,
}
```

- [ ] **Step 2: routing.rs**

`crates/totsuka-telemetry/src/notify/routing.rs`:
```rust
use totsuka_core::NotifyKind;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SinkId { Log, Slack, Github }

/// spec §13.5 の写像表
pub fn default_routing() -> HashMap<NotifyKind, Vec<SinkId>> {
    use NotifyKind::*;
    let log_only = vec![SinkId::Log];
    let log_slack = vec![SinkId::Log, SinkId::Slack];
    let mut m = HashMap::new();
    for k in [HumanGate1, HumanGate2, TaskFailed, TaskStuck, GivingUp,
              ProcessDead, ProcessUnhealthy, PgmqDead, ConfigError,
              SecretRotationWarn, WritebackConflict, ArgvSecretViolation,
              QaSpawnFailed] {
        m.insert(k, log_slack.clone());
    }
    m.insert(QaAnswerTimeout, log_only.clone());
    m.insert(WorktreeGcAlert, log_only);
    m
}

/// 種別ごとの dedup TTL 秒。0 = dedup 無効
pub fn default_dedup_ttl() -> HashMap<NotifyKind, u64> {
    use NotifyKind::*;
    let mut m = HashMap::new();
    m.insert(HumanGate1, 0); m.insert(HumanGate2, 0); m.insert(TaskFailed, 0);
    m.insert(GivingUp, 0); m.insert(ProcessDead, 0); m.insert(ArgvSecretViolation, 0);
    m.insert(TaskStuck, 3600);
    m.insert(ProcessUnhealthy, 600); m.insert(PgmqDead, 600);
    m.insert(ConfigError, 1800); m.insert(WritebackConflict, 3600);
    m.insert(QaSpawnFailed, 300); m.insert(QaAnswerTimeout, 600);
    m.insert(WorktreeGcAlert, 3600); m.insert(SecretRotationWarn, 86400);
    m
}
```

- [ ] **Step 3: mod.rs (Notifier + sink trait + dedup persistence)**

`crates/totsuka-telemetry/src/notify/mod.rs`:
```rust
pub mod payload;
pub mod routing;

pub use payload::NotifyPayload;
pub use routing::{SinkId, default_dedup_ttl, default_routing};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use totsuka_core::{Clock, NotifyKind};

#[async_trait::async_trait]
pub trait NotifySink: Send + Sync {
    fn id(&self) -> SinkId;
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("sink io: {0}")] Io(String),
    #[error("sink http: {0}")] Http(String),
}

#[derive(Default, Serialize, Deserialize)]
struct PersistedState {
    dedup: HashMap<String, DateTime<Utc>>,
}

pub struct Notifier {
    clock:     Arc<dyn Clock>,
    state:     Arc<Mutex<PersistedState>>,
    state_path: PathBuf,
    sinks:     Vec<Arc<dyn NotifySink>>,
    routing:   HashMap<NotifyKind, Vec<SinkId>>,
    dedup_ttl: HashMap<NotifyKind, u64>,
}

impl Notifier {
    pub async fn new(
        clock:      Arc<dyn Clock>,
        state_path: PathBuf,
        sinks:      Vec<Arc<dyn NotifySink>>,
        routing:    HashMap<NotifyKind, Vec<SinkId>>,
        dedup_ttl:  HashMap<NotifyKind, u64>,
    ) -> Self {
        let state = load_state(&state_path).await.unwrap_or_default();
        Self {
            clock, state: Arc::new(Mutex::new(state)), state_path,
            sinks, routing, dedup_ttl,
        }
    }

    pub async fn notify(&self, kind: NotifyKind, dedup_key: impl Into<String>, payload: NotifyPayload) {
        let dkey = format!("{}:{}", kind.as_snake(), dedup_key.into());
        let ttl_secs = self.dedup_ttl.get(&kind).copied().unwrap_or(0);
        let now = self.clock.now();

        if ttl_secs > 0 {
            let g = self.state.lock().await;
            if let Some(last) = g.dedup.get(&dkey) {
                let age = (now - *last).num_seconds() as u64;
                if age < ttl_secs {
                    tracing::debug!(kind=?kind, dedup_key=%dkey, age_secs=age, "notify deduped");
                    return;
                }
            }
            drop(g);
        }

        let sink_ids = self.routing.get(&kind).cloned().unwrap_or_else(|| vec![SinkId::Log]);
        for sid in sink_ids {
            if let Some(sink) = self.sinks.iter().find(|s| s.id() == sid) {
                if let Err(e) = sink.send(kind, &payload).await {
                    tracing::warn!(kind=?kind, sink=?sid, error=%e, "sink failed");
                }
            }
        }

        if ttl_secs > 0 {
            let mut g = self.state.lock().await;
            g.dedup.insert(dkey, now);
            let snapshot = serde_json::to_vec_pretty(&*g).unwrap();
            drop(g);
            let _ = atomic_write(&self.state_path, &snapshot).await;
        }
    }
}

async fn load_state(path: &PathBuf) -> Option<PersistedState> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn atomic_write(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use totsuka_core::MockClock;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountSink { id: SinkId, count: AtomicU32 }
    #[async_trait::async_trait]
    impl NotifySink for CountSink {
        fn id(&self) -> SinkId { self.id }
        async fn send(&self, _: NotifyKind, _: &NotifyPayload) -> Result<(), SinkError> {
            self.count.fetch_add(1, Ordering::SeqCst); Ok(())
        }
    }

    fn ttl_map() -> HashMap<NotifyKind, u64> {
        let mut m = HashMap::new();
        m.insert(NotifyKind::TaskStuck, 60);
        m.insert(NotifyKind::ProcessDead, 0);
        m
    }

    fn route_log_only() -> HashMap<NotifyKind, Vec<SinkId>> {
        let mut m = HashMap::new();
        for k in [NotifyKind::TaskStuck, NotifyKind::ProcessDead] {
            m.insert(k, vec![SinkId::Log]);
        }
        m
    }

    #[tokio::test] async fn dedup_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns.json");
        let clock = Arc::new(MockClock::new(Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap()));
        let sink = Arc::new(CountSink { id: SinkId::Log, count: AtomicU32::new(0) });
        let n = Notifier::new(clock.clone(), path, vec![sink.clone()], route_log_only(), ttl_map()).await;

        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default()).await;
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default()).await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 1);

        clock.advance(chrono::Duration::seconds(61));
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default()).await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test] async fn ttl_zero_never_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(MockClock::new(Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap()));
        let sink = Arc::new(CountSink { id: SinkId::Log, count: AtomicU32::new(0) });
        let n = Notifier::new(clock, dir.path().join("ns.json"), vec![sink.clone()], route_log_only(), ttl_map()).await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default()).await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default()).await;
        n.notify(NotifyKind::ProcessDead, "k1", NotifyPayload::default()).await;
        assert_eq!(sink.count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test] async fn dedup_state_persists_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns.json");
        let clock = Arc::new(MockClock::new(Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap()));

        let sink1 = Arc::new(CountSink { id: SinkId::Log, count: AtomicU32::new(0) });
        let n = Notifier::new(clock.clone(), path.clone(), vec![sink1.clone()], route_log_only(), ttl_map()).await;
        n.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default()).await;
        assert_eq!(sink1.count.load(Ordering::SeqCst), 1);

        // 「再起動」をシミュレート: 新 Notifier を同じ state_path で構築
        let sink2 = Arc::new(CountSink { id: SinkId::Log, count: AtomicU32::new(0) });
        let n2 = Notifier::new(clock, path, vec![sink2.clone()], route_log_only(), ttl_map()).await;
        n2.notify(NotifyKind::TaskStuck, "task:1", NotifyPayload::default()).await;
        // dedup state が読み込まれているので 0 のまま
        assert_eq!(sink2.count.load(Ordering::SeqCst), 0);
    }
}
```

> 依存追加が必要: `crates/totsuka-telemetry/Cargo.toml` に `async-trait = "0.1"` を `[dependencies]` に追加。tempfile は dev-dependencies に。

- [ ] **Step 4: 依存追記 + lib.rs**

`crates/totsuka-telemetry/Cargo.toml`:
```toml
async-trait = "0.1"
```
(`[dependencies]` セクションに 1 行追加)

`crates/totsuka-telemetry/src/lib.rs`:
```rust
pub mod notify;
pub use notify::{Notifier, NotifyPayload, NotifySink, SinkError, SinkId, default_dedup_ttl, default_routing};
```

- [ ] **Step 5: テスト pass**

Run: `cargo test -p totsuka-telemetry notify`
Expected: `3 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/totsuka-telemetry/
git commit -m "feat(telemetry): Notifier core with dedup + persisted state"
```

---

### Task D4: Log sink + Slack sink

**Files:**
- Create: `crates/totsuka-telemetry/src/notify/sink_log.rs`
- Create: `crates/totsuka-telemetry/src/notify/sink_slack.rs`
- Modify: `crates/totsuka-telemetry/src/notify/mod.rs`

**Interfaces:**
- Consumes: tracing, reqwest, Secret<String>
- Produces: `LogSink`、`SlackSink::new(webhook_url, default_channel)`

- [ ] **Step 1: sink_log.rs**

`crates/totsuka-telemetry/src/notify/sink_log.rs`:
```rust
use super::{NotifySink, NotifyPayload, SinkError, SinkId};
use totsuka_core::NotifyKind;

pub struct LogSink;

#[async_trait::async_trait]
impl NotifySink for LogSink {
    fn id(&self) -> SinkId { SinkId::Log }
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError> {
        tracing::warn!(
            target: "notify",
            kind = kind.as_snake(),
            title = %payload.title,
            body  = %payload.body,
            link  = ?payload.link,
            "notification"
        );
        Ok(())
    }
}
```

- [ ] **Step 2: sink_slack.rs**

`crates/totsuka-telemetry/src/notify/sink_slack.rs`:
```rust
use super::{NotifySink, NotifyPayload, SinkError, SinkId};
use serde_json::json;
use std::time::Duration;
use totsuka_core::{NotifyKind, Secret};

pub struct SlackSink {
    webhook_url:     Secret<String>,
    default_channel: String,
    client:          reqwest::Client,
}

impl SlackSink {
    pub fn new(webhook_url: Secret<String>, default_channel: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build().expect("reqwest client");
        Self { webhook_url, default_channel, client }
    }
}

#[async_trait::async_trait]
impl NotifySink for SlackSink {
    fn id(&self) -> SinkId { SinkId::Slack }
    async fn send(&self, kind: NotifyKind, payload: &NotifyPayload) -> Result<(), SinkError> {
        let url = self.webhook_url.expose();
        if url.is_empty() {
            return Ok(()); // 設定なし=無効、no-op
        }
        let body = json!({
            "channel": self.default_channel,
            "text":    format!("*[{}]* {}\n{}", kind.as_snake(), payload.title, payload.body),
            "attachments": payload.fields.iter().map(|(k,v)| {
                json!({ "title": k, "value": v, "short": true })
            }).collect::<Vec<_>>(),
        });
        let res = self.client.post(url).json(&body).send().await
            .map_err(|e| SinkError::Http(e.to_string()))?;
        if !res.status().is_success() {
            return Err(SinkError::Http(format!("slack http {}", res.status())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test] async fn empty_url_is_noop() {
        let s = SlackSink::new(Secret::new(String::new()), "#x".into());
        let r = s.send(NotifyKind::HumanGate1, &NotifyPayload::default()).await;
        assert!(r.is_ok());
    }
}
```

- [ ] **Step 3: mod.rs に re-export 追加**

`crates/totsuka-telemetry/src/notify/mod.rs` 末尾 `pub use` 群に追加:
```rust
pub mod sink_log;
pub mod sink_slack;
pub use sink_log::LogSink;
pub use sink_slack::SlackSink;
```

- [ ] **Step 4: テスト pass**

Run: `cargo test -p totsuka-telemetry sink`
Expected: `1 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-telemetry/
git commit -m "feat(telemetry): LogSink + SlackSink for Notifier"
```

---

### Task E1: totsuka-bus スケルトン + pgmq SQL ラッパ

**Files:**
- Create: `crates/totsuka-bus/Cargo.toml`
- Create: `crates/totsuka-bus/src/lib.rs`
- Create: `crates/totsuka-bus/src/pgmq.rs`

**Interfaces:**
- Consumes: sqlx (PgPool)
- Produces: `pgmq_create_queue(&pool, name)`、`pgmq_send_json(&pool, name, payload)`、`pgmq_read_one(&pool, name, vt_secs)`、`pgmq_delete(&pool, name, msg_id)`

- [ ] **Step 1: Cargo.toml**

`crates/totsuka-bus/Cargo.toml`:
```toml
[package]
name = "totsuka-bus"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
totsuka-core   = { path = "../totsuka-core" }
totsuka-config = { path = "../totsuka-config" }
sqlx           = { workspace = true }
serde          = { workspace = true }
serde_json     = { workspace = true }
tokio          = { workspace = true }
chrono         = { workspace = true }
thiserror      = { workspace = true }
tracing        = { workspace = true }

[dev-dependencies]
uuid = { workspace = true }
```

- [ ] **Step 2: pgmq.rs (低レベルラッパ)**

`crates/totsuka-bus/src/pgmq.rs`:
```rust
use serde_json::Value;
use sqlx::{PgPool, Row};

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("sqlx: {0}")] Sqlx(#[from] sqlx::Error),
    #[error("json: {0}")]  Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, BusError>;

/// 冪等: 既存ならそのまま
pub async fn create_queue(pool: &PgPool, name: &str) -> Result<()> {
    sqlx::query("SELECT pgmq.create($1)").bind(name).execute(pool).await?;
    Ok(())
}

pub async fn send_json(pool: &PgPool, name: &str, payload: &Value) -> Result<i64> {
    let row = sqlx::query("SELECT pgmq.send($1, $2::jsonb) AS msg_id")
        .bind(name).bind(payload)
        .fetch_one(pool).await?;
    Ok(row.get::<i64, _>("msg_id"))
}

#[derive(Debug, Clone)]
pub struct PgmqMessage {
    pub msg_id: i64,
    pub read_ct: i32,
    pub message: Value,
}

/// vt_secs だけ visibility を消費する
pub async fn read_one(pool: &PgPool, name: &str, vt_secs: i32) -> Result<Option<PgmqMessage>> {
    // pgmq.read(queue, vt, qty) returns SETOF
    let rows = sqlx::query("SELECT msg_id, read_ct, message FROM pgmq.read($1, $2, 1)")
        .bind(name).bind(vt_secs)
        .fetch_all(pool).await?;
    Ok(rows.into_iter().next().map(|r| PgmqMessage {
        msg_id:  r.get("msg_id"),
        read_ct: r.get("read_ct"),
        message: r.get("message"),
    }))
}

pub async fn delete(pool: &PgPool, name: &str, msg_id: i64) -> Result<bool> {
    let row = sqlx::query("SELECT pgmq.delete($1, $2) AS ok")
        .bind(name).bind(msg_id)
        .fetch_one(pool).await?;
    Ok(row.get::<bool, _>("ok"))
}
```

- [ ] **Step 3: lib.rs**

`crates/totsuka-bus/src/lib.rs`:
```rust
#![forbid(unsafe_code)]
pub mod pgmq;
pub use pgmq::{create_queue, send_json, read_one, delete, BusError, PgmqMessage};
```

- [ ] **Step 4: 統合テスト (要 DB)**

`crates/totsuka-bus/tests/pgmq_smoke.rs`:
```rust
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use totsuka_bus::*;

fn db_url() -> Option<String> { std::env::var("DATABASE_URL").ok() }

#[tokio::test]
async fn send_read_delete_cycle() {
    let Some(url) = db_url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let qname = format!("test_q_{}", uuid::Uuid::new_v4().simple());

    create_queue(&pool, &qname).await.unwrap();
    let payload = json!({"event_key":"t:1","payload":{"x":1}});
    let msg_id = send_json(&pool, &qname, &payload).await.unwrap();
    assert!(msg_id > 0);

    let m = read_one(&pool, &qname, 30).await.unwrap().expect("must read 1");
    assert_eq!(m.msg_id, msg_id);
    assert_eq!(m.message["event_key"], "t:1");

    let ok = delete(&pool, &qname, m.msg_id).await.unwrap();
    assert!(ok);

    // pgmq.drop_queue で清掃
    sqlx::query("SELECT pgmq.drop_queue($1)").bind(&qname).execute(&pool).await.unwrap();
}
```

- [ ] **Step 5: テスト実行**

Run:
```bash
export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/totsuka"
just pgmq-up && just db-migrate
cargo test -p totsuka-bus --test pgmq_smoke -- --nocapture
```
Expected: `1 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/totsuka-bus/
git commit -m "feat(bus): pgmq SQL function wrappers (create/send/read/delete)"
```

---

### Task E2: Envelope publish/consume API + transactional publish

**Files:**
- Create: `crates/totsuka-bus/src/envelope.rs`
- Create: `crates/totsuka-bus/src/publisher.rs`
- Create: `crates/totsuka-bus/src/consumer.rs`
- Modify: `crates/totsuka-bus/src/lib.rs`

**Interfaces:**
- Consumes: totsuka-core::{DomainEvent, EventEnvelope, Source, Clock}
- Produces:
  - `Publisher::send_domain(event, &mut tx)` — bus と同一 tx で publish (spec §9.3)
  - `Consumer::poll_one(vt_secs) -> Result<Option<(i64, EventEnvelope)>>`
  - `Consumer::ack(msg_id)`

- [ ] **Step 1: envelope.rs (薄いラッパ)**

`crates/totsuka-bus/src/envelope.rs`:
```rust
use totsuka_core::EventEnvelope;
use serde_json::Value;
use crate::pgmq::BusError;

pub fn envelope_to_json(env: &EventEnvelope) -> Result<Value, BusError> {
    Ok(serde_json::to_value(env)?)
}

pub fn json_to_envelope(v: Value) -> Result<EventEnvelope, BusError> {
    Ok(serde_json::from_value(v)?)
}
```

- [ ] **Step 2: publisher.rs**

`crates/totsuka-bus/src/publisher.rs`:
```rust
use crate::{envelope::envelope_to_json, pgmq::BusError};
use sqlx::{PgPool, Postgres, Transaction, Row};
use std::sync::Arc;
use totsuka_core::{Clock, DomainEvent, EventEnvelope};

pub struct Publisher {
    queue:  String,
    clock:  Arc<dyn Clock>,
}

impl Publisher {
    pub fn new(queue: impl Into<String>, clock: Arc<dyn Clock>) -> Self {
        Self { queue: queue.into(), clock }
    }

    /// 通常の (tx 外) publish
    pub async fn send(&self, pool: &PgPool, ev: DomainEvent, trace_id: Option<String>) -> Result<i64, BusError> {
        let env = EventEnvelope::from_domain(ev, self.clock.now(), trace_id);
        let v = envelope_to_json(&env)?;
        crate::pgmq::send_json(pool, &self.queue, &v).await
    }

    /// 同一 tx で publish (cursor 更新等とアトミック)。spec §9.3
    pub async fn send_in_tx(&self, tx: &mut Transaction<'_, Postgres>, ev: DomainEvent, trace_id: Option<String>) -> Result<i64, BusError> {
        let env = EventEnvelope::from_domain(ev, self.clock.now(), trace_id);
        let v = envelope_to_json(&env)?;
        let row = sqlx::query("SELECT pgmq.send($1, $2::jsonb) AS msg_id")
            .bind(&self.queue).bind(&v)
            .fetch_one(&mut **tx).await?;
        Ok(row.get::<i64, _>("msg_id"))
    }
}
```

- [ ] **Step 3: consumer.rs**

`crates/totsuka-bus/src/consumer.rs`:
```rust
use crate::{envelope::json_to_envelope, pgmq::{self, BusError, PgmqMessage}};
use sqlx::PgPool;
use totsuka_core::EventEnvelope;

pub struct Consumer { queue: String }

impl Consumer {
    pub fn new(queue: impl Into<String>) -> Self { Self { queue: queue.into() } }

    pub async fn poll_one(&self, pool: &PgPool, vt_secs: i32) -> Result<Option<(i64, EventEnvelope)>, BusError> {
        let m = pgmq::read_one(pool, &self.queue, vt_secs).await?;
        let Some(PgmqMessage { msg_id, message, .. }) = m else { return Ok(None); };
        let env = json_to_envelope(message)?;
        Ok(Some((msg_id, env)))
    }

    pub async fn ack(&self, pool: &PgPool, msg_id: i64) -> Result<bool, BusError> {
        pgmq::delete(pool, &self.queue, msg_id).await
    }
}
```

- [ ] **Step 4: lib.rs**

```rust
pub mod envelope;
pub mod publisher;
pub mod consumer;
pub use publisher::Publisher;
pub use consumer::Consumer;
```

- [ ] **Step 5: 統合テスト (publish→consume→ack)**

`crates/totsuka-bus/tests/envelope_smoke.rs`:
```rust
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_bus::*;
use totsuka_core::{DomainEvent, MockClock, Source};
use chrono::TimeZone;

fn db_url() -> Option<String> { std::env::var("DATABASE_URL").ok() }

#[tokio::test]
async fn publish_consume_ack() {
    let Some(url) = db_url() else { eprintln!("skipping"); return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let qname = format!("test_env_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();

    let clock = Arc::new(MockClock::new(chrono::Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap()));
    let publisher = Publisher::new(qname.clone(), clock);
    let consumer  = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "gh:delivery:abc".into(),
        source:    Source::Github,
        event_type:"github.status_changed".into(),
        payload:   json!({"to_status":"design"}),
    };
    let msg_id = publisher.send(&pool, ev, Some("trace-1".into())).await.unwrap();
    assert!(msg_id > 0);

    let Some((mid, env)) = consumer.poll_one(&pool, 30).await.unwrap() else {
        panic!("expected message");
    };
    assert_eq!(mid, msg_id);
    assert_eq!(env.event_key, "gh:delivery:abc");
    assert_eq!(env.event_type, "github.status_changed");
    assert_eq!(env.trace_id.as_deref(), Some("trace-1"));

    assert!(consumer.ack(&pool, mid).await.unwrap());
    assert!(consumer.poll_one(&pool, 30).await.unwrap().is_none());

    sqlx::query("SELECT pgmq.drop_queue($1)").bind(&qname).execute(&pool).await.unwrap();
}
```

- [ ] **Step 6: テスト実行**

Run: `cargo test -p totsuka-bus --test envelope_smoke -- --nocapture`
Expected: `1 passed`

- [ ] **Step 7: Commit**

```bash
git add crates/totsuka-bus/
git commit -m "feat(bus): Publisher (incl in-tx) + Consumer for DomainEvent envelopes"
```

---

### Task F1: examples/totsuka.toml.example + workspace lint pass

**Files:**
- Create: `examples/totsuka.toml.example`

**Interfaces:**
- Consumes: 無し (人間向け参考)
- Produces: 完全な valid 例

- [ ] **Step 1: 例を書く**

`examples/totsuka.toml.example`: 設計書 §6 のスキーマをすべて埋めたサンプル。
ファイル長が大きくなるので spec §6 から逐語コピーする (`[totsuka] / [supervisor] / [postgres] / [bus] / [agent_adapter] (vars + repos 2 件含む) / [orchestrator] / [github] (columns 8 値) / [github_watcher] / [qa_service] (classifier + answer) / [notifications] / [retention] / [telemetry]`)。

> 実装者注: spec の `docs/superpowers/specs/2026-06-28-rust-app-decomposition-design.md` §6 にある TOML 例を `examples/totsuka.toml.example` に貼り付け、コメントを保持。

- [ ] **Step 2: パース可能か確認**

`crates/totsuka-config/tests/example_parses.rs`:
```rust
use totsuka_config::Config;

#[test] fn example_file_parses_and_validates() {
    let path = format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"));
    let txt = std::fs::read_to_string(&path).expect("read example");
    let cfg = Config::from_toml_str(&txt).expect("parse example");
    cfg.validate().expect("validate example");
}
```

- [ ] **Step 3: テスト pass**

Run: `cargo test -p totsuka-config --test example_parses`
Expected: `1 passed`

- [ ] **Step 4: workspace 全体 lint + 全テスト**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```
Expected: 全て pass

- [ ] **Step 5: Commit**

```bash
git add examples/totsuka.toml.example crates/totsuka-config/tests/example_parses.rs
git commit -m "feat(config): example TOML + parse-validation test"
```

---

### Task F2: end-to-end smoke (config → publish → consume → notify)

**Files:**
- Create: `tests/e2e/foundation_smoke.rs` (workspace test)
- Modify: `Cargo.toml` (workspace に tests member 追加 or workspace の root に直接 tests/ を置く)

**Interfaces:**
- Consumes: 全 foundation crate
- Produces: 1 つの統合テストで「config 読込 → bus publish → consume → notifier deduped」を確認

- [ ] **Step 1: 統合テスト crate を workspace に追加**

`Cargo.toml` workspace members に追加:
```toml
"crates/totsuka-foundation-e2e",
```

新規 `crates/totsuka-foundation-e2e/Cargo.toml`:
```toml
[package]
name = "totsuka-foundation-e2e"
version.workspace = true
edition.workspace = true
publish = false

[dependencies]
totsuka-core      = { path = "../totsuka-core" }
totsuka-config    = { path = "../totsuka-config" }
totsuka-telemetry = { path = "../totsuka-telemetry" }
totsuka-bus       = { path = "../totsuka-bus" }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
sqlx  = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
uuid  = { workspace = true }
tempfile = "3.12"
async-trait = "0.1"
```

`crates/totsuka-foundation-e2e/src/lib.rs`: (empty placeholder)
```rust
// e2e crate is test-only
```

- [ ] **Step 2: e2e テストを書く**

`crates/totsuka-foundation-e2e/tests/foundation_smoke.rs`:
```rust
use chrono::TimeZone;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_bus::*;
use totsuka_config::Config;
use totsuka_core::{DomainEvent, MockClock, NotifyKind, Source};
use totsuka_telemetry::*;

#[tokio::test]
async fn config_loaded_publish_consume_notify_deduped() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { eprintln!("skip"); return };

    // 1. config (例ファイル) を読む
    let example_path = format!("{}/../../examples/totsuka.toml.example", env!("CARGO_MANIFEST_DIR"));
    let txt = std::fs::read_to_string(&example_path).unwrap();
    let cfg = Config::from_toml_str(&txt).unwrap();
    cfg.validate().expect("example must validate");
    assert_eq!(cfg.bus.queue_name, "totsuka_events");

    // 2. bus publish/consume
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let qname = format!("smoke_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();
    let clock = Arc::new(MockClock::new(chrono::Utc.with_ymd_and_hms(2026,6,28,12,0,0).unwrap()));
    let pubr = Publisher::new(qname.clone(), clock.clone());
    let cons = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "smoke:1".into(),
        source: Source::Internal,
        event_type: "smoke.tick".into(),
        payload: json!({"n": 1}),
    };
    let mid = pubr.send(&pool, ev, None).await.unwrap();
    let (got_id, env) = cons.poll_one(&pool, 30).await.unwrap().unwrap();
    assert_eq!(got_id, mid);
    assert_eq!(env.event_key, "smoke:1");
    cons.ack(&pool, mid).await.unwrap();

    // 3. notifier (LogSink 1 つ、dedup TTL=60s)
    use std::collections::HashMap;
    let tmp = tempfile::tempdir().unwrap();
    let mut ttl = HashMap::new();
    ttl.insert(NotifyKind::TaskStuck, 60);
    let mut route = HashMap::new();
    route.insert(NotifyKind::TaskStuck, vec![SinkId::Log]);

    let n = Notifier::new(
        clock.clone(),
        tmp.path().join("notify_state.json"),
        vec![Arc::new(LogSink)],
        route, ttl,
    ).await;

    n.notify(NotifyKind::TaskStuck, "task:x", NotifyPayload::default()).await;
    n.notify(NotifyKind::TaskStuck, "task:x", NotifyPayload::default()).await;
    // dedup されていれば state ファイルに 1 entry のみ
    let bytes = std::fs::read(tmp.path().join("notify_state.json")).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("task_stuck:task:x"));

    // 清掃
    sqlx::query("SELECT pgmq.drop_queue($1)").bind(&qname).execute(&pool).await.unwrap();
}
```

- [ ] **Step 3: 実行**

Run:
```bash
export DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/totsuka"
just pgmq-up && just db-migrate
cargo test -p totsuka-foundation-e2e --test foundation_smoke -- --nocapture
```
Expected: `1 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/totsuka-foundation-e2e/ Cargo.toml
git commit -m "test(e2e): foundation smoke (config + publish/consume + notifier dedup)"
```

---

### Task F3: CI hint (lint + test on PR) — optional but recommended

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: GitHub Actions runners
- Produces: PR で `cargo fmt --check` + clippy + DB なし unit test を実行

- [ ] **Step 1: workflow を書く (DB 必要な統合テストは別 job、本 task は unit のみ)**

`.github/workflows/ci.yml`:
```yaml
name: ci
on: [push, pull_request]

jobs:
  unit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy, rustfmt }
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --check
      - name: Clippy (unit-only)
        run: cargo clippy --workspace --lib --bins --tests -- -D warnings
      - name: Unit tests
        run: cargo test --workspace --lib

  integration:
    runs-on: ubuntu-latest
    services:
      pgmq:
        image: ghcr.io/pgmq/pg18-pgmq:v1.10.0
        env:
          POSTGRES_PASSWORD: postgres
          POSTGRES_DB: totsuka
        ports: ["5432:5432"]
        options: >-
          --health-cmd "pg_isready -U postgres -d totsuka"
          --health-interval 5s --health-timeout 3s --health-retries 10
    env:
      DATABASE_URL: postgres://postgres:postgres@127.0.0.1:5432/totsuka
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install sqlx-cli
        run: cargo install sqlx-cli --no-default-features --features rustls,postgres --version 0.8.2
      - name: Run migrations
        run: sqlx migrate run --source migrations
      - name: Integration tests
        run: cargo test --workspace --tests
```

- [ ] **Step 2: ローカルでも GitHub でも問題なく回ることを確認**

Run (local sanity):
```bash
just lint && cargo test --workspace --lib
```
Expected: pass

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: lint + unit + integration jobs (pgmq service)"
```

---

## Self-Review

**1. Spec coverage:** foundation スコープ (spec §1〜§3、§6 設定スキーマ、§11.1/2/4/5/6/7、§13 Notifier 基盤、§10 migrations、§4 docker compose) を網羅。スコープ外 (agent-adapter / orchestrator / watcher / qa-service / totsukactl 本体) は本 plan には含めない (後続 plan で担当)。

**2. Placeholder scan:** 全コード片を実装。`TODO`・「適切な〜」表現なし。F1 の `examples/totsuka.toml.example` だけ「spec §6 から貼り付け」と指示している (元情報が同じ repo 内にあるため許容、ただし実装者は貼り付けが必要)。

**3. Type consistency:** `Clock` (B2), `Secret<T>` (B3), `ColumnId` (B4), `Phase` (B5), `TaskId` (B5), `event_key_*` (B6), `EventEnvelope/DomainEvent/Source` (B7), `NotifyKind` (B8) は全て `totsuka-core` から re-export。`Config` (C2), `apply_env_overrides` (C4), `ValidationError` (C3) は `totsuka-config`。`Notifier`, `NotifySink`, `SinkId`, `NotifyPayload`, `LogSink`, `SlackSink` は `totsuka-telemetry`。`Publisher`, `Consumer`, `create_queue`, `send_json`, `read_one`, `delete` は `totsuka-bus`。後続タスクは全てこれらの公開シンボルだけを参照する。

**4. 各タスク独立にテスト可能:** A1-A6 は DB only (Rust 不要)、B1-B8 は DB 不要、C1-C4 は DB 不要、D1-D4 は DB 不要、E1-E2 + F2 のみ DB 必要。Subagent-driven 実行で各タスクを独立にレビューできる。

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-28-foundation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration。各タスクが独立 commit になり、レビューが小さい。

**2. Inline Execution** — このセッション内で executing-plans skill を使ってバッチ実行 + checkpoint レビュー。

**Which approach?**

(後続 plan: agent-adapter / orchestrator / github-watcher / qa-service / totsukactl は本 foundation 完了後に順次作成します。各 plan は同じ TDD バイトサイズ粒度で書きます。)



