# totsuka — Rust アプリ分割と起動・停止ライフサイクル設計

- Date: 2026-06-28
- Status: Draft (user review pending)
- Parent: `.plan/init-plan/requirements-design.md` (システム全体要件)

## 1. スコープ

親設計書で確定したマルチリポ・エージェントオーケストレーション基盤を、**Rust 製のローカル PC アプリ群** として実装するための、

- アプリ分割 (どの責務をどの binary にするか)
- 起動順序と起動確認 (まとめて立ち上げるときの規律)
- 安全な shutdown 手順
- 共通設定 / IPC / heartbeat / 状態機械

を確定する。親設計の責務・冪等性・状態書き戻し等は本書では再記しない (差分・具体化のみ)。

### 制約 (与件)
- `herdr` と `Claude Code` は **既存ツール**。本プロジェクトでは触らず、API を呼び出す側に回る。
- `orchestrator` / `agent-adapter` / `qa-service` は **永続的にローカル PC 上で動作する** 前提。
- `github-watcher` (とその先の bus / state store) は **将来クラウドに移行可能** とする。
- ローカル前提のため常時稼働ではない。再起動・停止を挟んでも親設計の §9 catch-up と §8 冪等で取りこぼさない。

---

## 2. アプリ構成 (5 binary + 4 共有 crate)

### 5 binary
| binary | 役割 | DB | herdr | listen | 親設計の対応 |
|---|---|---|---|---|---|
| `totsukactl` | 起動・停止・status の supervisor CLI (+ 常駐 daemon) | × | × | UDS (status API) | (本書で新設) |
| `agent-adapter` | HTTP → herdr 変換、worktree 管理 | × | ○ (UDS) | UDS (主) + TCP loopback (dev 任意) | §11 |
| `orchestrator` | 状態機械、bus pull、adapter 駆動 | ○ (rw) | × | UDS (healthz/readyz) | §10 |
| `github-watcher` | ProjectsV2 polling (snapshot diff) + issues since-pull、bus publish | ○ (rw cursors) | × | TCP (healthz/readyz のみ) | §3.1, §9 |
| `qa-service` | Slack Socket Mode + reaction→GitHub 起票 + adapter spawn | ○ (rw cursors) | × | UDS (healthz/readyz) | §6 |

### 4 共有 crate
| crate | 内容 |
|---|---|
| `totsuka-core` | `DomainEvent` / `Task` / `Phase` / `Column` 等のドメイン型、`event_key` / `effect_key` 生成 |
| `totsuka-bus` | pgmq の publish/pull/ack 薄ラッパ。トランザクション共有 API |
| `totsuka-config` | 共通 TOML スキーマ、`${...}` 展開、起動時バリデーション |
| `totsuka-telemetry` | `tracing` 初期化、構造化ログ、共通 healthz/readyz、request_id 伝播 |

### 外部前提 (totsuka が管理しない)
- Postgres (pgmq 拡張入り) → 後述のとおり `totsukactl` が docker compose 経由で起動
- herdr デーモン (Unix socket)
- Claude Code CLI (herdr が起動)
- docker daemon + compose plugin

---

## 3. Cargo workspace レイアウト (要約)

詳細は §11。

```
crates/
  totsuka-core, totsuka-bus, totsuka-config, totsuka-telemetry   # lib
  totsukactl, agent-adapter, orchestrator, github-watcher, qa-service   # bin
```

主要依存 (デフォルト想定):
- `tokio` (rt-multi-thread), `axum` (HTTP, UDS/TCP 両対応), `hyper`/`hyperlocal` (UDS クライアント), `reqwest` (TCP/HTTPS クライアント), `sqlx` (Postgres + migrations), `serde` + `toml`, `tracing` + `tracing-subscriber`, `slack-morphism`, `octocrab`, `clap`, `tokio::signal`, `nix`。

---

## 4. 起動順序と probe (`totsukactl up`)

### 前提と支配領域
- **totsukactl が面倒見る**: Postgres コンテナ (docker compose で管理) と 4 つの Rust 子プロセス
- **外部前提 (人/OS が用意)**: docker daemon + compose plugin、herdr デーモン、Claude Code CLI、git/gh の credential

### 起動シーケンス
```
totsukactl up
 │
 ├─[phase -1: postgres]   docker compose 経由で pgmq コンテナ
 │   ├─ docker daemon probe (docker info)             ── NG: 案内して exit 1
 │   ├─ compose plugin probe (docker compose version) ── NG: 案内して exit 1
 │   ├─ container 状態確認 (docker compose ps pgmq)
 │   │     未起動 → docker compose up -d pgmq
 │   │     起動済 → そのまま次へ
 │   ├─ image タグ検証
 │   │     running image が ghcr.io/pgmq/pg18-pgmq:v1.10.0 か inspect で照合
 │   │     不一致 → 期待タグと現タグ・差分を表示し、
 │   │              `docker compose pull && totsukactl up --recreate` を案内
 │   └─ healthy 待ち (compose healthcheck + 自前 SELECT 1)
 │         30 秒超過 → `docker compose logs --tail=50 pgmq` を吐いて exit 1
 │
 ├─[phase 0: preflight]
 │   ├─ totsuka.toml 読込・バリデーション (totsuka-config)
 │   ├─ pgmq 拡張バージョン確認 (select extversion from pg_extension where extname='pgmq')
 │   │     v1.10.0 互換 (semver) でなければ案内して exit 1
 │   ├─ migration 差分チェック (sqlx migrate info)
 │   │     差分あり → `totsukactl migrate` を案内して exit 1 (自動適用しない)
 │   └─ herdr unix socket 疎通 probe (ping)
 │         NG → herdr 起動方法を案内して exit 1
 │
 ├─[phase 1: execution plane]    spawn → readyz 200 待ち
 │   └─ agent-adapter             (UDS listen + herdr 接続 + 設定リポ .git 検証)
 │
 ├─[phase 2: control plane]       spawn → readyz 200 待ち
 │   └─ orchestrator              (DB 接続 + adapter UDS probe + bus consumer start)
 │
 └─[phase 3: ingestion]           並列 spawn → 両方 readyz 200 待ち
     ├─ github-watcher            (healthz TCP listen + GitHub API probe + 起動時 ProjectsV2 一巡完了)
     └─ qa-service                (Slack Socket Mode 接続 + catch-up 完了)
```

### compose.yml の規約 (`deploy/docker-compose.yml`)
- service 名: `pgmq`
- image: `ghcr.io/pgmq/pg18-pgmq:v1.10.0` (`totsuka-config` で集中管理、`totsukactl` はそれを参照)
- volume: named volume `totsuka_pgmq_data` を `/var/lib/postgresql` に mount (永続)。PG18 はメジャーバージョン別 subdirectory (例: `/var/lib/postgresql/18/docker`) を使うため、`/var/lib/postgresql/data` に mount すると起動を拒否する。
- port: `127.0.0.1:5432:5432` (totsuka.toml で変更可)
- healthcheck: `pg_isready -U postgres -d totsuka` (`POSTGRES_DB` と一致させる)
- restart policy: なし (supervisor 管理下)
- container user: `"0:0"` (root) — Docker Desktop for macOS が named volume を非 root プロセスで初期化させないため。Linux 環境でも互換。

### readiness の定義
| bin | ready 条件 |
|---|---|
| `agent-adapter` | UDS listen + herdr socket 接続 + 設定済みリポの `.git` 検証 |
| `orchestrator`  | DB 接続 + agent-adapter UDS probe 200 + bus consumer loop 開始 |
| `github-watcher`| healthz/readyz TCP listen + GitHub API token probe 200 + 起動時 ProjectsV2 一巡完了 |
| `qa-service`    | Slack Socket Mode 接続 + catch-up 一巡完了 |

起動時 catch-up を readiness に含める根拠: 親設計 §9 のとおり catch-up はライブ経路の前に「穴埋め」を終えるべきで、それで初めて正常稼働中といえる。

### probe のタイムアウト・リトライ
- supervisor → 子プロセスの readyz probe: **30 秒 × 0.5 秒間隔**。超過で全体 abort → 起動済みプロセスを逆順 SIGTERM → exit 1
- 各 Rust アプリ内部の外部依存 retry (DB / adapter): exp backoff、上限 60 秒で fail-fast

### 失敗時のユーザ案内 (共通)
1. どこで失敗したか (phase + サブステップ)
2. 直近のエラーメッセージ (構造化ログから抜粋)
3. 直近ログ末尾 (該当コンポーネントの最後 50 行)
4. 想定原因と対処の short hint (case 別)
5. 詳細ログの場所 (`~/.local/state/totsuka/logs/<bin>.log` と `docker compose logs pgmq`)

### `totsukactl` サブコマンド
- `init` — first-run bootstrap (詳細 §11.11)
- `up [--recreate] [--bootstrap]` — 全体起動。`--recreate` で pgmq コンテナ作り直し、`--bootstrap` で config 欠落時に暗黙 init
- `down [--force] [--postgres]` — 安全 shutdown / 強制終了 / Postgres も停止
- `status` — 5 プロセス + pgmq コンテナの pid・readyz・直近ログ
- `migrate` — sqlx migration 適用 (preflight が案内したとき)
- `backup` — pg_dump で `${data_dir}/backups/` に保存、直近 7 を保持 (詳細 §11.3)
- `restore <dump>` — 確認プロンプトの上で復元 (down --postgres 後にのみ可)
- `logs <bin>` — 該当ログを tail -f
- `restart <bin>` — 個別再起動 (依存順を尊重)
- `reload <bin>` — SIGHUP 中継 (現状は agent-adapter のみ意味あり)

---

## 5. 安全な shutdown (`totsukactl down`)

### 設計原則
- **新規流入を先に止め、奥から閉じる** (逆依存順)
- **at-least-once + 冪等 (親設計 §8)** が前提なので、in-flight イベント取りこぼし・重複は実害なし
- **herdr 上の Claude エージェントは生かしたまま**。agent-adapter は「追跡を手放して exit」するだけ
- **Postgres コンテナは止めない** (既定)。データ保護優先。`down --postgres` で明示停止のみ

### shutdown シーケンス
```
totsukactl down
 │
 ├─[stage 1: ingestion 遮断]    並列 SIGTERM → exit 待ち
 │   ├─ github-watcher          (polling loop 停止 + 進行中 tx flush)
 │   └─ qa-service              (Slack Socket Mode 切断 + 進行中 tx flush)
 │
 ├─[stage 2: control plane 排水]  SIGTERM → drain 完了 → exit 待ち
 │   └─ orchestrator            (bus pull 停止 / in-flight effect の lease 完了待ち / DB close)
 │
 ├─[stage 3: execution plane 切り離し]  SIGTERM → exit 待ち
 │   └─ agent-adapter           (UDS listen 停止 / 進行中 HTTP 応答完了 /
 │                                 既存 worktree は破棄せず TTL 保持 /
 │                                 herdr 上の Claude pane は kill しない)
 │
 ├─[stage 4 (任意): postgres 停止]   `--postgres` 指定時のみ
 │   └─ docker compose stop pgmq
 │
 └─ pid file クリーンアップ
```

### 各 bin の共通ハンドラ
```
SIGTERM/SIGINT を待ち受け
 → shutdown_initiated フラグ
 → 新規受付停止 (listener.close / consumer.pause / WS.disconnect)
 → in-flight タスクを drain (deadline 付き)
 → リソース close (DB pool, HTTP clients, files)
 → ログ flush → exit 0
```

### deadline と escalation
| signal | 動作 | timeout |
|---|---|---|
| 1st SIGTERM | graceful drain 開始 | **15 秒** |
| timeout 超過 | SIGTERM 再送 | 5 秒 |
| なお生存 | SIGKILL | 即 |

15 秒の根拠: orchestrator の effect lease は秒単位、watcher の tx は数百 ms、adapter の HTTP 応答も即返り。長時間ブロックする処理は shutdown 経路に置かない。

### `totsukactl down --force`
- 順序無視で全プロセスに即 SIGTERM、3 秒で SIGKILL
- in-flight effect は親設計 §8 の冪等台帳で次回起動時に再駆動
- Claude pane は生存 (herdr が動いていれば継続)

### 再起動時の状態リカバリ
- **agent-adapter**: 起動時に `agent.list` で生存 pane を取得 → `effects.result` に保存した `terminal_id` から task と再紐付け
- **orchestrator**: `processed_effects.in_progress` の lease 期限切れを sweeper が再 claim → 再駆動
- **watcher / qa-service**: 親設計 §9 catch-up で穴埋め

---

## 6. 共通設定 `totsuka.toml`

### 配置
- 既定: `~/.config/totsuka/config.toml` (XDG)
- secrets は `~/.config/totsuka/secrets.toml` (chmod 600)、または env で上書き
- 各 bin は `--config <path>` で上書き可能。supervisor も同じ config を読む

### スキーマ
```toml
# ---- 全体 ----
[totsuka]
log_level   = "info"
state_dir   = "~/.local/state/totsuka"
data_dir    = "~/.local/share/totsuka"
timezone    = "Asia/Tokyo"           # 表示・通知用 (storage は UTC 統一、§11.5)

# ---- データ保持 (詳細 §11.2) ----
[retention]
events_weeks    = 4
snapshot_days   = 30
logs_max_mb     = 1024
log_file_max_mb = 50

# ---- telemetry (詳細 §11.9) ----
[telemetry]
metrics_enabled    = true
otlp_endpoint      = ""              # 空文字で trace export 無効
trace_sample_ratio = 0.1

# ---- secrets メタ (詳細 §11.7) ----
[secrets]
rotation_warn_days = 30

# ---- supervisor ----
[supervisor]
ready_timeout_secs    = 30
shutdown_grace_secs   = 15
shutdown_kill_secs    = 5
recreate_on_image_mismatch = false

[supervisor.heartbeat]
healthz_interval_secs   = 5
readyz_interval_secs    = 30
pgmq_interval_secs      = 30
unhealthy_threshold     = 3
degraded_threshold      = 2
restart_policy          = "on-dead-only"   # on-dead-only | on-unhealthy | never
restart_backoff_secs    = [5, 15, 60]
restart_max_attempts    = 5
notify_on_degraded      = false

# ---- postgres コンテナ ----
[postgres]
image       = "ghcr.io/pgmq/pg18-pgmq:v1.10.0"
container   = "totsuka-pgmq"
host        = "127.0.0.1"
port        = 5432
database    = "totsuka"
user        = "postgres"
volume      = "totsuka_pgmq_data"
compose_file = "deploy/docker-compose.yml"

# ---- bus (pgmq) ----
[bus]
queue_name        = "totsuka_events"
visibility_secs   = 30
batch_size        = 16
poll_interval_ms  = 200

# ---- agent-adapter ----
[agent_adapter]
uds_path          = "${totsuka.state_dir}/sock/adapter.sock"
tcp_bind          = ""                 # 空文字で TCP 無効 (本番)。dev は "127.0.0.1:7801" 等
herdr_socket      = "~/.config/herdr/herdr.sock"
node_capacity     = 8
repos_root        = "${env:HOME}/work/repos"
auto_clone        = true

# worktree GC (詳細 §11.16)
worktree_failed_ttl_hours          = 72
worktree_orphan_scan_interval_secs = 3600

[agent_adapter.vars]
work = "${env:HOME}/work"
fast = "/Volumes/fast-ssd"

[agent_adapter.repos."gmo-media/hakoniwa"]
description     = "Web UI (Next.js / TypeScript) for product X"   # qa-service LLM 分類用
worktree_subdir = ".worktree"

[agent_adapter.repos."gmo-media/vanduit"]
description     = "Backend API (Rust / axum) for product X"
repo_path       = "${work}/dev/vanduit"
worktree_path   = "${fast}/worktrees/vanduit"

# ---- orchestrator ----
[orchestrator]
uds_path                  = "${totsuka.state_dir}/sock/orchestrator.sock"
wip_global                = 3
phase_timeout_default_secs = 1800
phase_timeout             = { impl_verify = 7200 }
retry_max                 = 1
stuck_threshold_secs      = 600
adapter_uds               = "${agent_adapter.uds_path}"

[orchestrator.claude_argv]
global = ["--dangerously-skip-permissions"]
[orchestrator.claude_argv.per_repo."gmo-media/hakoniwa"]
extra  = ["--model", "sonnet"]
[orchestrator.claude_argv.per_phase.impl_verify]
extra  = ["--model", "opus"]

# ---- GitHub (watcher / orchestrator / qa-service が共通参照、単一 Project 前提) ----
[github]
project_owner   = "gmo-media"             # ProjectsV2 オーナー (org or user)
project_number  = 42                      # ProjectsV2 の番号
status_field    = "Status"                # ProjectsV2 single-select フィールド名

[github.columns]                          # 必須。totsuka-core::ColumnId 全 8 値を網羅 (詳細 §11.4)
inbox             = "📥 Inbox"
ready             = "📋 Ready"
design            = "🤖 調査・設計"
design_review     = "🚧 設計レビュー"
impl_verify       = "🤖 実装・受入検証"
final_review      = "🚧 最終レビュー"
awaiting_release  = "🚀 リリース待ち"
released          = "🏁 完了"

# ---- github-watcher (polling-only。Project 参照は [github]) ----
[github_watcher]
bind                       = "127.0.0.1:7802"   # healthz/readyz 用のみ
project_poll_interval_secs = 20                  # ProjectsV2 status snapshot diff の周期
issues_poll_interval_secs  = 60                  # issues since-pull の周期
catchup_window_hours       = 24                  # 起動時 catch-up が遡る上限
graphql_page_size          = 100                 # ProjectsV2 items の 1 ページ取得数

# ---- qa-service ----
[qa_service]
uds_path              = "${totsuka.state_dir}/sock/qa-service.sock"
allowed_user_ids      = ["U12345", "U67890"]
catchup_channels      = ["C111", "C222"]
reaction_trigger      = "memo"
default_mode          = "delegated"      # auto | delegated
adapter_uds           = "${agent_adapter.uds_path}"
repo_select_mode      = "llm_classify"   # llm_classify (既定) | channel_map (将来)

[qa_service.classifier]                              # 詳細 §8.4
# provider: anthropic | openai | openrouter | litellm | openai_compatible
# 必須対応 4 つ: anthropic / openai / openrouter / litellm
# openai_compatible は将来用 catch-all (Azure OpenAI / Groq / Together / Ollama 等)
provider              = "anthropic"
model                 = "claude-haiku-4-5-20251001"

# api_base: 空文字 ("") なら provider 既定を使用。litellm / openai_compatible は必須指定
#   anthropic   既定 → "https://api.anthropic.com"
#   openai      既定 → "https://api.openai.com/v1"
#   openrouter  既定 → "https://openrouter.ai/api/v1"
#   litellm     既定 → なし (必須指定、例: "http://localhost:4000")
#   openai_compatible 既定 → なし (必須指定)
api_base              = ""

max_tokens            = 256
confidence_threshold  = 0.70                          # top-1 がこの未満なら fallback
top_candidates        = 3                             # 候補上位 N
on_low_confidence     = "delegated_reaction"          # delegated_reaction | refuse | use_top1
include_thread_context = true                         # スレッド継続発言は親メッセージも分類入力にする
request_timeout_secs  = 15

# api key は secrets.toml の [qa_service.classifier].api_key を最優先、
# 次に provider 別 env (下記) を fallback
#   anthropic   → ANTHROPIC_API_KEY
#   openai      → OPENAI_API_KEY
#   openrouter  → OPENROUTER_API_KEY
#   litellm     → LITELLM_API_KEY
#   openai_compatible → 設定ファイル必須 (env fallback なし)

[qa_service.answer]                                  # 詳細 §8.4 Slack 回答フロー
sentinel               = "<<TOTSUKA_DONE>>"          # Claude が回答末尾に出力するマーカー
answer_open_tag        = "<answer>"
answer_close_tag       = "</answer>"
poll_interval_ms       = 1500                         # pane.read のポーリング間隔
stable_revision_secs   = 8                            # sentinel が来なくても revision がこの秒数停滞したら done 扱い
answer_timeout_secs    = 180                          # 全体打ち切り (この時刻までに sentinel/停滞検知できなければ truncate して送信)
pane_idle_ttl_secs     = 1800                         # スレッド最終活動からこの秒数経過で pane.close
max_concurrent_answers = 4                            # 同時並行で動かす回答 task の上限

# ---- notifications (§13 詳細、O17 の写像はコード、宛先のみ設定) ----
[notifications]
config_error_notify   = true             # ConfigError 種別を発火するか
dedup_default_secs    = 600
rate_limit_per_min    = 30               # 全 sink 合計の安全上限

[notifications.dedup_ttl_secs]           # 種別→TTL 秒 (0=dedup 無効)。詳細 §13.6
human_gate1            = 0
human_gate2            = 0
task_failed            = 0
task_stuck             = 3600
giving_up              = 0
process_dead           = 0
process_unhealthy      = 600
pgmq_dead              = 600
config_error           = 1800
secret_rotation_warn   = 86400
writeback_conflict     = 3600
argv_secret_violation  = 0
qa_spawn_failed        = 300
qa_answer_timeout      = 600
worktree_gc_alert      = 3600

[notifications.slack]
webhook_url            = ""              # 空文字で sink 無効
default_channel        = "#totsuka"
channel_overrides      = {}              # 例: { human_gate1 = "#review" }
bucket_capacity        = 10
bucket_refill_per_min  = 5

[notifications.github]
enabled                = false           # 将来用
```

### secrets.toml
```toml
[postgres]            password = "..."
[github_watcher]      github_token = "ghp_..."
[qa_service]          slack_app_token = "xapp-..."  slack_bot_token = "xoxb-..."
[qa_service.classifier] api_key = "sk-ant-..."   # provider 別に対応する key:
                                                  #   anthropic   "sk-ant-..."  (or env ANTHROPIC_API_KEY)
                                                  #   openai      "sk-..."      (or env OPENAI_API_KEY)
                                                  #   openrouter  "sk-or-..."   (or env OPENROUTER_API_KEY)
                                                  #   litellm     <litellm proxy key> (or env LITELLM_API_KEY)
[notifications.slack] webhook_url = "https://hooks.slack.com/..."
```

### env での上書き
- `TOTSUKA__<SECTION>__<KEY>=value` で TOML 値を上書き
- secrets は **env を最優先** (`POSTGRES_PASSWORD` 等の慣用名を許容)

### 起動時バリデーション (`totsuka-config`)
1. 必須キー (親設計 §12.5 + 上記スキーマ)
2. 変数展開: `${name}` / `${env:NAME}` の未定義/循環は起動時エラー
3. 排他: `worktree_subdir` と `worktree_path` (親設計 §12.4)
4. リポ未登録の `owner/repo`: orchestrator 受信時に reject
5. port / UDS path 衝突: 同一値ならエラー
6. 通知: `notifications.slack.webhook_url` が空文字 = 通知無効

### ホットリロード (SIGHUP)
- `totsukactl reload agent-adapter` → supervisor 経由で SIGHUP
- 受け取った bin は `totsuka.toml` を再パースし、変更可能項目のみ適用

**hot reload 可**:
- 新規 `[agent_adapter.repos."*"]` 追加 (リポ未登録時の主用途)
- 既存リポの `default_branch` 変更
- 通知設定 (`[notifications.*]`)

**hot reload 不可 (restart 必要、reload はエラー応答)**:
- 既存リポの `repo_path` / `worktree_subdir` / `worktree_path` 変更
- `repos_root` 変更
- `[agent_adapter].uds_path` / `herdr_socket` 変更
- `[postgres]` 系

リロードは「全変更を診断 → 不可変更が混じれば全 rollback、エラー応答」のアトミック方式。

#### リポ未登録時のフロー (例)
1. orchestrator が adapter `/spawn` を叩く → 404 `repo_not_registered`
2. orchestrator は task を同一カラムに留める (retry を消費しない)
3. 通知ディスパッチャが「設定不足」種別で通知
   > Task #123 (`gmo-media/foo`): リポ未登録です。totsuka.toml に追記後 `totsukactl reload agent-adapter` を実行してください。
4. ユーザ追記 → `totsukactl reload agent-adapter` → adapter ログに適用結果
5. orchestrator の次の retry tick で再度 `/spawn` → 成功

---

## 7. プロセス間通信 (IPC) + heartbeat

### 配置の前提
- ローカル固定 (永続的に同一ホスト): orchestrator / agent-adapter / qa-service / supervisor / herdr / Claude
- 将来クラウド移行候補: github-watcher / Postgres (bus + state store)

### IPC マトリクス
| from → to | プロトコル | bind | 理由 |
|---|---|---|---|
| orchestrator → agent-adapter | HTTP over Unix Domain Socket | `${state_dir}/sock/adapter.sock` (0700) | ローカル固定。port 不要・FS ACL |
| qa-service → agent-adapter | HTTP over Unix Domain Socket | 同上 | 同上 |
| supervisor → orchestrator / adapter / qa-service (healthz/readyz) | HTTP over Unix Domain Socket | `${state_dir}/sock/<bin>.sock` (0700) | 同上 |
| supervisor → github-watcher (healthz/readyz) | HTTP over TCP loopback | `127.0.0.1:<port>` | 将来クラウド対応で TCP 統一 |
| github-watcher / qa-service / orchestrator → bus | Postgres TCP | `[postgres].host:port` | クラウド DB 候補 |
| agent-adapter → herdr | Unix domain socket (NDJSON) | `[agent_adapter].herdr_socket` | herdr の既存仕様 |
| supervisor → 子プロセス (shutdown / reload) | OS signal | SIGTERM / SIGKILL / SIGHUP | — |
| watcher / qa-service → GitHub / Slack | HTTPS | — | 外部統合 |
| supervisor → docker | subprocess `docker compose` | — | §4 |

### 開発用補助 (TCP loopback)
- agent-adapter は `[agent_adapter].tcp_bind` 指定時のみ TCP loopback 併聴 (curl / browser でデバッグ)
- 本番設定では空文字 → UDS のみ
- github-watcher は最初から TCP (将来クラウド対応)

### 共通 HTTP 規約 (`totsuka-telemetry`)
- `GET /healthz` (liveness, 200)
- `GET /readyz` (readiness, 200 / 503 + 内訳 JSON)
  ```json
  { "ready": false,
    "checks": { "db": "ok", "adapter": "fail: connect refused", "herdr": "ok" } }
  ```
- `x-totsuka-request-id` ヘッダを全 inter-service HTTP に伝播
- バージョニング: `/v1/...`
- JSON: timestamp は RFC3339/UTC/`Z` 終端、enum は snake_case、ID は文字列
- エラーは RFC7807 Problem Details

### bus envelope (`totsuka-bus`)
```json
{ "event_key":     "gh:delivery:abc-123",
  "source":        "github",
  "type":          "github.status_changed",
  "published_at":  "2026-06-28T03:00:00Z",
  "trace_id":      "...",
  "payload":       { ... type 別 ... } }
```
`event_key` 生成 (`totsuka-core`): `gh:delivery:<id>` / `slack:event:<id>` / `derived:<deterministic-key>`

### リトライ規律
- orchestrator → adapter: idempotency-key=effect_key、5xx は exp backoff (max 30s, 3 回)、4xx は即停止
- adapter → herdr: 1s 内の即時再接続のみ、それ以上は readyz NG
- bus pull: `visibility_secs` で見えない期間を確保、未 ack は再配信 (親設計 §8 で冪等化)

### heartbeat (supervisor 常駐)
```
totsukactl up
  └─ supervisor を fork & detach、pid を ${state_dir}/supervisor.pid に書く
     └─ supervisor は ${state_dir}/sock/supervisor.sock で status API を提供
        ├─ 子プロセス group を waitpid 監視 (即時 dead 検知)
        ├─ healthz tick (既定5秒)   : 各 bin に GET /healthz
        ├─ readyz  tick (既定30秒) : 各 bin に GET /readyz
        ├─ pgmq tick   (既定30秒) : docker compose ps pgmq + SELECT 1
        └─ 失敗時は restart_policy に従う + 通知
```

#### 状態遷移 (子プロセス単位)
```
starting → ready → healthy → degraded → unhealthy → dead → restarting → starting
                                                         ↘ giving_up (max attempts 超過)
* → draining → stopped   (supervisor からの SIGTERM)
```

#### restart_policy
- `on-dead-only` (既定): SIGCHLD で死亡確認時のみ再起動
- `on-unhealthy` : unhealthy 判定でも再起動
- `never`        : 通知のみ、再起動は人手 (`totsukactl restart <bin>`)

#### 連鎖再起動の規律
- `agent-adapter` が dead で再起動 → orchestrator は内部 retry で復旧 (orchestrator 自身は再起動しない)
- `pgmq` 異常時は **連鎖再起動禁止** (データ破損リスク)、強通知のみ。`policy` 上書きで `never` 固定

#### supervisor の status API
```
GET  /v1/processes               → [{name, pid, state, started_at, last_healthz_at,
                                      consecutive_failures, restart_count}, ...]
POST /v1/processes/<name>/restart
POST /v1/processes/<name>/reload
POST /v1/shutdown                 (totsukactl down が叩く)
```

#### 観測
- 構造化ログ: `heartbeat=ok|unhealthy|dead bin=orchestrator latency_ms=12`
- 通知: dead 必発、degraded/unhealthy は閾値到達時に 1 回 (rate-limit)
- `totsukactl status` 表示例:
  ```
  NAME            STATE      PID    UPTIME    HEALTHZ  RESTARTS
  pgmq            running    -      1h23m     ok       -
  agent-adapter   healthy    1234   1h22m     ok(5s)   0
  orchestrator    healthy    1235   1h22m     ok(5s)   0
  github-watcher  degraded   1236   1h22m     readyz!  1
  qa-service      healthy    1237   1h22m     ok(5s)   0
  ```

---

## 8. 各アプリの I/O 契約

transport の最終仕様は §7 のマトリクスを参照。本章は責務記述。

### 8.1 `agent-adapter`
- **入力**:
  - HTTP over UDS (本番) / TCP loopback (dev 任意):
    - `POST /v1/agents` — spawn (`{ task_id, phase, attempt, repo, branch, argv, env }`)
      - `task_id` = ProjectV2Item.id (§11.14)
      - `branch` = `totsuka/{task_id_short}/{phase_short}` (orchestrator が生成、§11.14)
      - `attempt` = `tasks.impl_verify_attempt` 等 (§11.15)
    - `POST /v1/agents/{id}/messages` — send (`{ text }`)
    - `GET  /v1/agents/{id}/output` — フル snapshot (herdr `pane.read` 準拠、revision 同梱)
    - `DELETE /v1/agents/{id}` — stop (= herdr `pane.close`)
    - `POST /v1/repos/reload` — SIGHUP の HTTP 等価 (任意)
  - OS signal: SIGTERM (graceful) / SIGHUP (config reload)
  - 設定: totsuka.toml `[agent_adapter]`
- **出力**:
  - Unix socket → herdr: `agent.start` / `agent.send` / `pane.read` / `pane.close`
  - HTTP レスポンス: spawn 成功時 `{ agent_id, terminal_id, worktree_path }`、失敗時 RFC7807
  - fs: worktree 作成 (親設計 §12.4)、failed 時 TTL 保持
- **状態**: stateless。再起動後は `agent.list` から既存 pane を逆引きして再追跡。in-memory cache は設定済みリポの解決結果のみ
- **障害境界**:
  - herdr 死亡 → readyz NG (`herdr: fail`)、HTTP は 503
  - worktree 衝突 → 409
  - 未登録リポ → 404 `repo_not_registered`
  - 容量超過 → 409 `capacity_full` (back-pressure §11.8 参照)
  - argv に secret-like flag → 400 `argv_secret_violation` (§11.13)
- **note**: herdr の `events.subscribe` は **購読しない** (totsuka は agent 状態を保持しない、§9)。secrets は env 経由で渡す (§11.13)

### 8.2 `orchestrator`
- **入力**:
  - Postgres (bus): `pgmq.read`
    - 主要 type: `github.status_changed`, `github.pr_merged`, `github.release_published`, `phase.timeout` (内部 timer), `human.gate_passed`
  - Postgres (state): `processed_events` / `processed_effects` / task 状態
  - OS signal: SIGTERM
  - 設定: totsuka.toml `[orchestrator]`
- **出力**:
  - HTTP over UDS → agent-adapter (`/v1/agents` 系)
  - HTTPS → GitHub Project (型B 冪等カラム移動 / tasks フィールド書き戻し)
  - HTTPS → Slack (通知、設定があれば)
  - Postgres: `processed_effects` lease 取得 / task state 更新
- **内部コンポーネント**: bus consumer / state machine (8 カラム) / WIP gate / conversation driver / phase timer / sweeper / notifier
- **状態**: stateful (唯一の中枢)。すべて Postgres 永続。in-memory は cache のみ
- **障害境界**:
  - adapter 不能 → readyz NG。bus pull 一時停止。読込済みは visibility timeout で再配信
  - DB 切断 → readyz NG、reconnect 試行
  - GitHub API 5xx → exp backoff、長期失敗は通知
  - **writeback OCC 競合** (§11.12): 人間の同時操作で version mismatch → 中止 + `suppress_writeback_until_human_move` フラグ立て、人手の次回移動で解除

### 8.3 `github-watcher` (polling-only)
- **理由**: ローカル PC は NAT 配下で外部から webhook を直接受けられない。tunnel / forwarder を導入する代わりに、**ProjectsV2 の GraphQL polling + snapshot diff** に一本化する。ステータス遷移は人間操作 (= 低頻度) なので 15〜30 秒の polling 間隔で実用上十分。将来 watcher をクラウドへ移したときに webhook モードを追加できる構造は残す (`[github_watcher].mode` 拡張余地)。
- **入力**:
  - HTTPS → GitHub GraphQL (ProjectsV2 items 全走査、`fieldValueByName(name:"Status")` 取得)
  - HTTPS → GitHub REST (Issues `since`、PR / release イベント)
  - OS signal: SIGTERM
  - 設定: totsuka.toml `[github_watcher]` + `[github]`
- **出力**:
  - Postgres (bus): `pgmq.send` で DomainEvent (`github.status_changed` / `github.pr_merged` / `github.release_published` / `github.issue_updated`)
  - Postgres (state): `catchup_cursor` / `gh_item_status` を **同一 tx で更新** (親設計 §9.3)
- **動作**:
  - **ProjectsV2 status loop** (周期 `project_poll_interval_secs`):
    1. `node($projectId) { items(first: N, after: $cursor) { ... fieldValueByName("Status") } }` を全ページ取得
    2. 取得した `(item_id, status)` を `gh_item_status` snapshot と diff
    3. 差分ごとに `github.status_changed` を publish、snapshot UPSERT、cursor 更新を **同一 tx** で実行
    4. event_key は決定論的 (`gh:status:<item_id>:<to_status_hash>`) — 再 publish は冪等で吸収
  - **Issues / PR / release loop** (周期 `issues_poll_interval_secs`):
    - Issues は REST `since` パラメータで更新分のみ取得
    - PR / release は `events` API or repo-scoped query
    - event_key は `gh:issue:<id>:<updated_at_ms>` などの決定論的合成
  - **PR ↔ task 紐付け** (§11.14): branch 名から `task_id_short` を抽出し `tasks` から逆引き。失敗時は PR 本文末尾の `Totsuka-Task: {full_task_id}` trailer を確認。どちらも欠落していれば task と紐付かない PR として扱い、orchestrator はその PR を進捗 signal に使わない
- **状態**: semi-stateful。cursor (`catchup_cursor`) と snapshot (`gh_item_status`) を DB に保持、in-memory は当該 loop 中のページング状態のみ
- **障害境界**:
  - GitHub rate limit → exp backoff、`X-RateLimit-Reset` まで poll を遅延
  - GraphQL 5xx → そのページからリトライ、loop 自体は継続
  - DB tx 失敗 → 当該 tick を破棄、次 tick で再走査 (snapshot 未更新なら diff は同じ結果になる)
- **将来クラウド移行時**: `[github_watcher].mode = "webhook"` を追加し、webhook listener + HMAC 検証を有効化。catch-up loop は ダウンタイム穴埋め用に低頻度で残す。両モード並走時は event_key の冪等性で重複吸収

### 8.4 `qa-service`
- **入力**:
  - WebSocket ← Slack Socket Mode (`message`, `reaction_added`)
  - HTTPS → Slack web API (`conversations.history` / `replies` / `chat.postMessage` / `chat.postEphemeral`)
  - HTTPS → GitHub Project (reaction 起票)
  - HTTPS → LLM provider (repo 分類用、§8.4 別項)。provider は `[qa_service.classifier].provider` で切替可能: **anthropic / openai / openrouter / litellm / openai_compatible**
  - HTTP over UDS → agent-adapter (回答エージェント spawn)
  - OS signal: SIGTERM
  - 設定: totsuka.toml `[qa_service]` + `[qa_service.classifier]`、secrets は provider 別 (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `OPENROUTER_API_KEY` / `LITELLM_API_KEY`)
- **出力**:
  - Slack 投稿 (auto / ephemeral)
  - GitHub Project Inbox 起票
  - agent-adapter spawn / send / read
  - Postgres (bus / state): catchup イベント publish と cursor 更新
- **モード**: `auto` (直接ポスト) / `delegated` (本人に ephemeral)、`default_mode` で既定、将来 user 別 override
- **状態**: semi-stateful (cursor + thread コンテキスト in-memory)、再起動時は catchup と Slack history で再構築
- **障害境界**:
  - Slack 切断 → WebSocket 再接続 + history 穴埋め
  - adapter 不能 → 回答失敗、ephemeral で「準備中」+ bus に `qa.spawn_failed`
  - GitHub 起票失敗 → reaction 残存 → 起動時 catchup で再走査
  - LLM provider 5xx / rate limit / timeout → exp backoff、3 回失敗で `on_low_confidence` フォールバックに合流。provider 切替時は readyz で疎通 probe を強制

#### Repo 選択フロー (`repo_select_mode = "llm_classify"`)

質問が回答対象になった (allowed_user_ids 発言 + メンション or thread 継続) ときに、以下を実行:

1. **候補リストを totsuka.toml から構築**
   - 既定: `[agent_adapter.repos.*]` の全リポを候補に。各リポの `description` (必須・空文字なら readyz NG) と `owner/repo` を分類入力に渡す
2. **LLM provider で分類** (`[qa_service.classifier]` 設定、provider は anthropic / openai / openrouter / litellm / openai_compatible)
   - request: question 本文 + (`include_thread_context = true` なら親メッセージ) + 候補リポ一覧 (description 付き) を system / user message に展開
   - **構造化出力を provider 別 API で強制**:
     - Anthropic → tool use (`tools = [{name: "classify_repo", input_schema: ...}]` + `tool_choice = {type: "tool", name: "classify_repo"}`)
     - OpenAI / OpenRouter / LiteLLM / openai_compatible → `response_format = {type: "json_schema", json_schema: {...}}` (gpt-4o 系) または function calling (旧モデル fallback)
   - 共通スキーマ: `[{"repo": "owner/foo", "confidence": 0.92, "rationale": "..."}, ...]` を `top_candidates` 個
   - パース失敗・空応答は ProviderError として 3 回 retry → fallback 合流
3. **判定**:
   - top-1 の `confidence >= confidence_threshold` → そのリポで `agent-adapter` に spawn (auto モード) / ephemeral 確認 (delegated モード)
   - 閾値未満 → `on_low_confidence` に従う:
     - `delegated_reaction`: ephemeral で上位 `top_candidates` の絵文字 reaction を出し、本人に選ばせる
     - `refuse`: ephemeral で「リポジトリを特定できませんでした、明示的に指定してください」
     - `use_top1`: 最尤を強制採用 (信頼度をログに記録)
4. **観測**: 全分類結果を構造化ログ (`tracing::info!(question_id, provider, model, top1_repo, top1_confidence, rationale, latency_ms)`) + メトリクス `qa_classify_total{provider, outcome}` (`high_conf` / `low_conf_delegated` / `low_conf_refused` / `error`) + `qa_classify_latency_seconds{provider}`

#### Provider 抽象の実装メモ (qa-service 内 module 構成)

```
crates/qa-service/src/classifier/
├── mod.rs              # Classifier trait + dispatch (provider 名 → impl 選択)
├── prompt.rs           # 共通 prompt 構築 (question + thread + repos)
├── schema.rs           # 共通レスポンス型 (RepoCandidate { repo, confidence, rationale })
├── anthropic.rs        # Anthropic Messages API + tool_use 強制
├── openai_compat.rs    # OpenAI Chat Completions + response_format/json_schema、openai / openrouter / litellm / openai_compatible で共用
└── retry.rs            # exp backoff、429 / 5xx / parse error 共通ハンドラ
```

- 2 実装 (`anthropic.rs` / `openai_compat.rs`) で 4 つの必須 provider を全てカバー
- 拡張点: 別 API shape (例: Google Gemini native) を足すときは新ファイル + `mod.rs` の dispatch に追加
- 将来 orchestrator や notifier で LLM が必要になったら、この module を `crates/totsuka-llm/` に昇格させて共有 crate 化する余地を残す (本書時点では qa-service 内に閉じる)

#### Repo 選択フロー (将来: `repo_select_mode = "channel_map"`)
- 設定 `[qa_service.channel_repos]` で channel → repo 候補配列
- 候補 1 個なら即決、複数なら delegated reaction、0 個なら refuse
- 現状未実装、将来オプションとして追加可能

#### Slack 回答フロー (qa-service が回答 post まで担う)

回答は **qa-service が** agent-adapter 経由で Claude を駆動し、出力を回収して Slack に post する (agent-adapter は中継のみ、回答整形・post は qa-service の責務)。`max_concurrent_answers` で並行数を制限。

1. **Thread mapping を引く**
   - `qa_thread_agent (thread_ts PRIMARY KEY, terminal_id, repo, last_activity_at)` を SELECT
   - **既存あり**: `agent-adapter POST /v1/agents/{terminal_id}/messages` で `text` (= 新規発言本文) を送信
   - **既存なし**: §8.4 repo 選択で決まった repo を使い `agent-adapter POST /v1/agents` で spawn。返却された `terminal_id` を mapping に INSERT
   - spawn 時の argv には **system prompt template** を渡す:
     - 「回答は `<answer>...</answer>` で囲み、末尾に必ず `<<TOTSUKA_DONE>>` を出力すること」
     - 「Slack で表示されるため Markdown は mrkdwn 流儀 (`*bold*` / `_italic_` / ``` ``` ```) で書くこと」
     - tag / sentinel 文字列は `[qa_service.answer]` から流し込む (ハードコードしない)
2. **回答 ready 検知** (`[qa_service.answer]` の値)
   - `poll_interval_ms` ごとに `agent-adapter GET /v1/agents/{terminal_id}/output` を呼び、snapshot + revision を取得
   - **完了条件 (いずれか先)**:
     1. snapshot 文字列に `sentinel` が含まれる → 完了 (主)
     2. revision が `stable_revision_secs` 秒変化しない → 完了 (フォールバック、Claude が sentinel を忘れた場合)
     3. `answer_timeout_secs` 経過 → truncate して post + 警告ログ + メトリクス `qa_answer_timeout_total`
   - **方針との整合性**: herdr の agent status は購読しない (§9 と整合)。pane.read snapshot のみを根拠に判定する
3. **回答テキスト抽出**
   - snapshot から `answer_open_tag` 〜 `answer_close_tag` の間を切り出し
   - tag が見つからない場合のフォールバック: snapshot 末尾 N 行を fallback として送信し、tag 欠落を警告ログ
   - 抽出後文字列はサイズ上限 (例 40000 文字、Slack 上限近辺) で truncate
4. **Slack に post**
   - `default_mode = "auto"`: `chat.postMessage` で channel (+ thread_ts) に投稿
   - `default_mode = "delegated"`: `chat.postEphemeral` で本人にだけ「案」を表示、本人が編集して投稿 (返信は本人責任)
   - mode は質問のチャンネル設定や user 設定で将来 override 可能
5. **mapping 更新**
   - `qa_thread_agent.last_activity_at = now()` を UPDATE
6. **pane ライフサイクル**
   - 定期 sweeper (qa-service 内 tokio task) が `last_activity_at < now() - pane_idle_ttl_secs` の mapping を抽出
   - `agent-adapter DELETE /v1/agents/{terminal_id}` で `pane.close` 相当 → mapping を DELETE
   - スレッドが再開した場合は新 spawn される (履歴コンテキストは失われるため、必要なら system prompt に「過去会話は失われた、リセットされた前提で答えよ」のヒントを追記)

#### 再起動時のリカバリ
- qa-service 起動時に `agent.list` を adapter 経由で取得 → `qa_thread_agent` と突き合わせ:
  - 両方にある: そのまま継続
  - mapping のみあり / pane 喪失: mapping を DELETE (スレッド継続発言は新規 spawn 扱い)
  - pane のみあり / mapping 喪失: orphan として `pane.close` (リーク回避)

#### Slack 回答メトリクス
- `qa_answer_total{mode, outcome}` (`mode = auto|delegated`, `outcome = posted|truncated|spawn_failed|extract_fallback`)
- `qa_answer_latency_seconds` (spawn/send 〜 post 完了)
- `qa_pane_open_gauge` (現在保持中の pane 数)

### 8.5 `totsukactl` (supervisor)
- **入力**:
  - CLI: `up / down [--force] [--postgres] / status / migrate / logs <bin> / restart <bin> / reload <bin>`
  - HTTP over UDS ← サブコマンドからの supervisor.sock 経由問い合わせ
  - OS signal: 自身 SIGTERM で全 stack shutdown
  - 設定: totsuka.toml 全体
- **出力**:
  - subprocess (docker compose / fork+exec)
  - OS signal → 子プロセス
  - HTTP → 子プロセス healthz/readyz
  - fs: pid file / log file
  - stdout/stderr: ユーザ向けメッセージ
- **状態**: デーモン常駐 (heartbeat)、状態は `${state_dir}` 下に pid と process registry
- **障害境界**: 自身死亡 → pid file が stale → `totsukactl status` で `stack not running`

---

## 9. ライフサイクル状態機械

### 9.1 子プロセスの状態 (supervisor 視点)
```
Starting → Ready → Healthy
                 ↘ Degraded (readyz NG ≧ degraded_threshold)
                 ↘ Unhealthy (healthz NG ≧ unhealthy_threshold)
                 ↘ Dead (SIGCHLD / 接続不能)
Dead | Failed | Unhealthy → Restarting (policy 該当) → Starting
Restarting → GivingUp     (restart_max_attempts 超過、強通知 + 放置)
* → Draining → Stopped     (SIGTERM 受信 → drain → exit)
```

### 9.2 totsuka スタック全体
```
Stopped → Starting → Running ⇄ Degraded → ShuttingDown → Stopped
```
`totsukactl status` の STACK 行で表示。

### 9.3 task の状態 (orchestrator)
親設計 §4.1 の 8 カラム + 副状態:
```
Inbox → Ready
     → 調査・設計   (Drafting → Drafted → Awaiting:設計レビュー)
     → 設計レビュー
     → 実装・受入検証 (Implementing → Verifying → DiffBack | PassedVerification)
     → 最終レビュー
     → リリース待ち → 完了
```

#### 進行のトリガはすべて GitHub の真実から得る
- 設計コメント書き戻し / PR 作成 / CI green / merge / release は github-watcher が DomainEvent として publish
- 人ゲート ①② は GitHub Project のカラム移動 (人間操作) を github-watcher が拾う
- orchestrator はこれらを bus 経由で受信し、状態機械を進める

#### agent 状態は使わない (orchestrator conversation driver 詳細)
- **進捗の真実は GitHub のみ**。orchestrator は `pane.read` snapshot を眺めて完了判定はしない (qa-service の sentinel パターンは採用しない)
- **implementer フェーズ完了の signal** = `github.pr_merged_ready` (= PR が作成され CI green になった) を bus 経由で受信
- **verifier フェーズ起動条件**: 上記 signal 到達時、orchestrator は verifier 用に新規 spawn (別 pane、別 effect_key)
- **verifier への入力**: orchestrator が implementer pane の `pane.read` snapshot + PR diff (gh CLI で取得) を結合し、verifier に `agent.send` で 1 回だけ投入する。verifier の判定結果は同じく PR への review コメント or status check 経由で GitHub に返り、watcher が `github.pr_verification_*` イベントとして拾う
- **DiffBack の signal** = verifier から「AC 未達」の状態が GitHub 上 (PR review コメント or label) に立った時点。orchestrator は §11.15 の `impl_verify_attempt + 1` で再 spawn
- **phase wall-clock timeout** (親設計 §10.1, 既定 `impl_verify=2h`) は **安全網**。何らかの理由で GitHub signal が来なかった場合に task を `Blocked` (副状態) にして元カラムに留め、通知
- **`crates/orchestrator/src/conversation.rs` の責務**: 上記の signal-driven な「verifier 起動 + 入力組み立て」のプリミティブを提供する小さなモジュールで、独自の完了検知ロジックは持たない

#### 副状態の terminal
- `Blocked` — stuck / 未登録リポ等、人手必要
- `GivenUp` — retry 超過、運用判断待ち

### 9.4 (削除) agent (Claude) の状態
totsuka は agent 状態を保持・監視しない。herdr に完全委譲。spawn 時刻だけ task に紐付けて phase timeout を計測する。

### 9.5 effect の状態
`pending → in_progress → done | failed` (lease 期限切れは sweeper が回収再駆動)

### 9.6 状態間の関係
```
[supervisor: stack/process state] ── 監督 ──► [process state per bin]

[orchestrator が駆動]
        ├──► [task state]   ◄── GitHub Project / Slack 通知 ──┐
        ├──► [effect state] ◄── sweeper / retry              │
        └──► spawn 時刻 + phase deadline                       │
                                                              │
[agent (Claude) state]   ◄── herdr が一手に管理 ───────────────┘
       totsuka は保持しない、必要な output は pane.read で都度取得
```

- supervisor は process だけ知る
- orchestrator は task / effect / phase timer だけ知る
- adapter は pane handle (`terminal_id`) を effect.result に保存するだけ。再起動後は `agent.list` で再特定可能
- agent 状態は herdr の中だけ

### 9.7 herdr の観測点 (調査済みの API)
本書執筆時点の herdr 0.7.1 で利用可能:

| 機能 | herdr API |
|---|---|
| IPC | Unix socket (NDJSON)、既定 `~/.config/herdr/herdr.sock`、env `HERDR_SOCKET_PATH` で上書き、`0600` |
| spawn | `agent.start { name, cwd, argv, env, ... }` または `pane.split` |
| stop | `pane.close` (agent.stop は無い) |
| listing | `agent.list` (`terminal_id`, status, labels, cwd, ...) |
| output read | `pane.read` フル snapshot + 単調 `revision`。since cursor なし |
| input send | `agent.send` (literal) / `pane.send_keys` (named keys) |
| status (5 値) | `Idle / Working / Blocked / Done / Unknown` (画面スクレイプ) |
| events | `events.subscribe` NDJSON (`pane.agent_status_changed` / `pane.output_changed` / `pane.exited` ...) |
| probe | `ping` (version / protocol / capabilities) |
| 注意 | CPU / RSS / native stuck 検知はなし、process 情報は `pane.process_info` のみ |

本設計では **`agent.start / agent.send / pane.read / pane.close / agent.list / ping` のみ使用**。`events.subscribe` は使わない (agent 状態を購読しない方針)。

---

## 10. ディレクトリレイアウト

### 10.1 リポジトリ (totsuka)
```
totsuka/
├── Cargo.toml                          # [workspace] members
├── rust-toolchain.toml
├── mise.toml
├── README.md
├── .gitignore
├── .plan/                              # 既存: 設計検討メモ
├── docs/
│   └── superpowers/specs/
│       └── 2026-06-28-rust-app-decomposition-design.md   # 本書
├── deploy/
│   └── docker-compose.yml              # pgmq サービス定義
├── migrations/                         # sqlx migration (forward-only、§11.1)
│   ├── 0000_schema_meta.sql            # bin↔DB ハンドシェイク用
│   ├── 0001_processed_events.sql       # PARTITION BY RANGE (received_at) 週単位
│   ├── 0002_processed_effects.sql      # 同上
│   ├── 0003_catchup_cursor.sql
│   ├── 0004_gh_item_status.sql         # status は ColumnId snake_case で保存
│   ├── 0005_tasks.sql                  # task_id (PVTI_...) PK、task_id_short、impl_verify_attempt、suppress_writeback_until_human_move 等 (§11.14/11.15)
│   └── 0006_qa_thread_agent.sql        # Slack thread_ts → terminal_id mapping (§8.4)
├── examples/
│   └── totsuka.toml.example
├── crates/
│   ├── totsuka-core/
│   │   └── src/{lib.rs, event.rs, task.rs, phase.rs, column.rs, key.rs}
│   ├── totsuka-bus/
│   │   └── src/{lib.rs, pgmq.rs, envelope.rs, consumer.rs, publisher.rs}
│   ├── totsuka-config/
│   │   └── src/{lib.rs, schema.rs, expand.rs, validate.rs}
│   ├── totsuka-telemetry/
│   │   └── src/{lib.rs, log.rs, http.rs, ready.rs}
│   ├── totsukactl/
│   │   └── src/{main.rs, cli.rs, supervisor.rs, heartbeat.rs, compose.rs,
│   │             probe.rs, child.rs, sock_api.rs}
│   ├── agent-adapter/
│   │   └── src/{main.rs, http.rs, herdr_client.rs, repos.rs, worktree.rs,
│   │             reload.rs}
│   ├── orchestrator/
│   │   └── src/{main.rs, sm/, wip.rs, conversation.rs, timer.rs,
│   │             sweeper.rs, notifier.rs, adapter_client.rs, gh_writeback.rs}
│   ├── github-watcher/
│   │   └── src/{main.rs, polling/, gh_client.rs, project_diff.rs, issues_pull.rs}
│   └── qa-service/
│       └── src/{main.rs, slack/, mode.rs, reaction.rs, gh_inbox.rs,
│                 adapter_client.rs, catchup.rs,
│                 classifier/{mod.rs, prompt.rs, schema.rs,
│                             anthropic.rs, openai_compat.rs, retry.rs}}
└── tests/
    └── e2e/
```

### 10.2 ランタイム (XDG)
```
${XDG_CONFIG_HOME:-~/.config}/totsuka/
├── config.toml
└── secrets.toml

${XDG_DATA_HOME:-~/.local/share}/totsuka/        # 将来用

${XDG_STATE_HOME:-~/.local/state}/totsuka/
├── supervisor.pid
├── pids/
│   ├── agent-adapter.pid
│   ├── orchestrator.pid
│   ├── github-watcher.pid
│   └── qa-service.pid
├── sock/                                        # 0700
│   ├── supervisor.sock
│   ├── adapter.sock
│   ├── orchestrator.sock
│   └── qa-service.sock
└── logs/
    ├── supervisor.log
    ├── agent-adapter.log
    ├── orchestrator.log
    ├── github-watcher.log
    └── qa-service.log

${XDG_CACHE_HOME:-~/.cache}/totsuka/             # 未使用
```

### 10.3 外部 (totsuka が触らない)
```
~/.config/herdr/herdr.sock                       # herdr socket (HERDR_SOCKET_PATH 上書き可)
${HOME}/work/repos/...                           # repos_root、各リポの worktree もこの下
docker volume: totsuka_pgmq_data                 # Postgres データ
```

---

## 11. Cross-cutting conventions

複数 bin に共通して適用される設計規律。実装段階で retrofit すると全 bin を貫く影響が出るため、本書段階で確定する。

### 11.1 Schema versioning (bin ↔ DB ハンドシェイク)

- migrations 0000 で `schema_meta(version INT PRIMARY KEY, applied_at TIMESTAMPTZ)` を作成
- DB に触る各 bin (orchestrator / github-watcher / qa-service) は以下を const で持つ:
  ```rust
  pub const MIN_SCHEMA_VERSION: i32 = 5;
  pub const TARGET_SCHEMA_VERSION: i32 = 7;
  ```
- 起動時 readyz の `db` チェックは `schema_meta.version` を読み、`MIN ≤ version ≤ TARGET` のときだけ ok を返す。範囲外は readyz NG (`db: schema out of range (got=X, want=[5..7])`)
- migrations は **forward-only**。`down/` ファイルは CI で禁止
- 唯一の DB スキーマ変更経路は `totsukactl migrate` (preflight が差分検知時に案内)

### 11.2 データ保持・パーティション

```toml
[retention]
events_weeks     = 4        # processed_events / processed_effects の保持週数
snapshot_days    = 30       # gh_item_status を item close 後に保持する日数
logs_max_mb      = 1024     # ${state_dir}/logs の合計上限 (rotate で削減)
log_file_max_mb  = 50       # ファイル単位ローテ上限
```

- `processed_events` / `processed_effects` は **PARTITION BY RANGE (received_at)**、週ごとパーティション (初期 migration から)
- orchestrator が nightly tick で `events_weeks` を超える partition を `DETACH + DROP`
- `gh_item_status` は GitHub item が close 後 `snapshot_days` 経過したら DELETE
- ログは `tracing-appender` の daily rotate、合計サイズ `logs_max_mb` を超えたら古いファイルから削除

### 11.3 Disaster recovery

- `totsukactl backup` — `pg_dump --format=custom` を `${data_dir}/backups/<UTC-ts>.dump` に保存、直近 7 を保持
- `totsukactl restore <dump>` — `totsukactl down --postgres` 後にのみ実行可。確認プロンプトあり
- compose.yml の任意設定: `[postgres].wal_archive_dir` を指定すると WAL アーカイブ有効化、PITR 可能
- pgmq Postgres volume を失うと **dedup ledger も全消失** → 復旧後しばらくは catch-up が古いイベントを再発火する可能性があるため、監視強化

### 11.4 ColumnId と表示名マッピング

- 親設計の 8 カラム (絵文字付き和文表示名) は `totsuka-core::ColumnId` enum で正規化:
  ```rust
  pub enum ColumnId { Inbox, Ready, Design, DesignReview,
                      ImplVerify, FinalReview, AwaitingRelease, Released }
  ```
- TOML `[github].columns` は **必須**:
  ```toml
  [github.columns]
  inbox             = "📥 Inbox"
  ready             = "📋 Ready"
  design            = "🤖 調査・設計"
  design_review     = "🚧 設計レビュー"
  impl_verify       = "🤖 実装・受入検証"
  final_review      = "🚧 最終レビュー"
  awaiting_release  = "🚀 リリース待ち"
  released          = "🏁 完了"
  ```
- watcher は GitHub から取得した表示名をこの map で `ColumnId` に変換、未知の表示名は readyz NG (`config_error_notify` で通知)
- `gh_item_status.status` には `ColumnId` の serde 形式 (snake_case) を保存。表示名変更だけならコード変更不要

### 11.5 Clock + timezone

- `totsuka-core::Clock` trait:
  ```rust
  pub trait Clock: Send + Sync + 'static {
      fn now(&self) -> chrono::DateTime<chrono::Utc>;
  }
  pub struct SystemClock;        // 本番
  pub struct MockClock { ... }   // テスト
  ```
- 全 bin はコンストラクタで `Arc<dyn Clock>` を受け取る。`SystemTime::now()` / `chrono::Utc::now()` 直接呼び出しは clippy で deny (例外は明示コメント必須)
- ストレージは **UTC 統一**、表示・通知は `[totsuka].timezone` (既定 `"Asia/Tokyo"`) でローカライズ
- フェーズ deadline / lease expiry / event_key timestamps すべて Clock 経由 → テストで時間制御可能

### 11.6 Error / panic ポリシー

- ライブラリ crate (totsuka-core / -bus / -config / -telemetry) は `thiserror` で error enum を export、`code()` メソッドで RFC7807 `type` URI 文字列 (`/errors/<kind>`) を返す
- bin crate は `anyhow::Result<()>` を主たる戻り型とし、境界 (HTTP ハンドラ / bus consumer / supervisor tick) で error enum を `?` で受け、`tracing::error!` でログしてから外部応答に変換
- `Cargo.toml [profile.release] panic = "abort"`。panic はプロセス死 → supervisor の Dead 経路に乗せる
- ただし HTTP ハンドラの個別リクエストは `catch_unwind` 相当の wrapper で個別捕捉し、リスナーごと落ちないようにする (各 bin の main.rs で 1 箇所のみ)
- RFC7807 `type` URI の写像表は `totsuka-core::error::TYPE_URI_TABLE` に集中、追加は写像追加のみ

### 11.7 Secret<T> wrapper

```rust
pub struct Secret<T>(T);
impl<T> Debug for Secret<T> { /* "***" */ }
impl<T> Display for Secret<T> { /* "***" */ }
impl<T> Secret<T> { pub fn expose(&self) -> &T { &self.0 } }
```

- `totsuka-config` が token / password / webhook url を `Secret<String>` として deserialize
- ログには絶対に出さない (Debug 安全)。`.expose()` は outbound HTTP / DB 接続文字列構築時のみ
- 追加: `[secrets].rotation_warn_days = 30`。各 token の最終更新時刻を `${state_dir}/secrets_meta.json` に保持し、超過したら daily warning を notifier 経由で発火

### 11.8 Back-pressure (channel bounds)

全 in-process channel は **bounded mpsc**。サイズと full 時動作を明示:

| 場所 | bound (既定) | full 時動作 |
|---|---|---|
| orchestrator: bus pull → state machine 投入 | `[bus].batch_size * 2 = 32` | block (consumer が pgmq pull 一時停止 → visibility timeout で再配信) |
| orchestrator: state machine → adapter HTTP request queue | `node_capacity = 8` | block (back-pressure 連鎖) |
| orchestrator: GitHub writeback queue | 64 | block |
| orchestrator: notifier queue | 256 | **drop oldest** (本流を止めない、warn ログ) |
| adapter: HTTP request → herdr request | `node_capacity = 8` | 429 即返却 (HTTP 層 back-pressure) |
| adapter: pane.read result | 1 (per request) | 不要 (request scoped) |
| watcher: ProjectsV2 diff → bus publish | `graphql_page_size = 100` | block (1 page atomically commit) |
| qa-service: Slack event → 処理 | 128 | **drop oldest** (Slack 側にもバッファ、warn ログ) |

メトリクス `channel_full_total{channel}` を発火。

### 11.9 Metrics / trace export

- `GET /metrics` を全 bin で公開 (UDS の場合は UDS、TCP の場合は TCP)。Prometheus pull format
- 必須メトリクス:
  - `events_processed_total{type}` (counter)
  - `effects_in_flight` (gauge)
  - `phase_timeout_total` (counter)
  - `restart_total{bin}` (counter, supervisor 発火)
  - `heartbeat_failure_total{bin, kind}` (counter)
  - `channel_full_total{channel}` (counter)
- OTLP trace 任意: `[telemetry].otlp_endpoint = ""` (空文字で無効)、設定時は `tracing-opentelemetry` 経由で export

追加 config:
```toml
[telemetry]
metrics_enabled    = true
otlp_endpoint      = ""              # 空文字で trace export 無効
trace_sample_ratio = 0.1
```

### 11.10 Blocking task boundaries

tokio rt-multi-thread を前提とし、**blocking 処理は必ず `spawn_blocking`** で隔離:
- subprocess (`docker compose`, `git`, `gh`)
- 大きな文字列パース (`pane.read` の数 MB snapshot 解析)
- `std::fs` 同期 IO
- 同期 DB driver は使わない (`sqlx` async のみ)

`spawn_blocking` 同時実行は **per-bin semaphore** (`max_blocking_concurrency = node_capacity`) で制限。tokio の blocking thread pool は upper bound があるため、無秩序に投げると worker が枯渇する。

clippy lint: `tokio::task::block_in_place` は既定で deny。例外は明示コメント必須。

### 11.11 First-run bootstrap

- `totsukactl init` — 以下を順に実行 (冪等):
  1. XDG ディレクトリ作成 (`~/.config/totsuka/`, `~/.local/state/totsuka/{pids,sock,logs}/`)
  2. `config.toml` を template から書き出し (既存なら skip + 警告)
  3. `secrets.toml` を 0600 で placeholder 付きで書き出し
  4. `docker compose up -d pgmq` (preflight と同じ手順)
  5. `sqlx migrate run`
  6. 次のステップ案内 (token を埋めて `totsukactl up`)
- `totsukactl up --bootstrap` は `config.toml` / `secrets.toml` 両方欠落時のみ暗黙 `init` 実行後 up

### 11.12 GitHub Project writeback 競合 (型B 強化)

- orchestrator はカラム移動の直前に取得した `ProjectV2Item` の version を渡し、**OCC (Optimistic Concurrency Control) で UPDATE**
- 競合 (version mismatch) → writeback 中止、ログ記録。次の watcher poll で `github.status_changed` が来るので state machine が再評価
- **人間優先ルール**: writeback が競合した場合、orchestrator はその task に `suppress_writeback_until_human_move` フラグを立て、人手のカラム移動を 1 回観測するまで自動 writeback を行わない (無限ループ防止)
- フラグはタスク状態に保存し、`human.gate_passed` 受信でクリア

### 11.13 Claude argv 規律

- 3 層 merge は **append-only**: `global ++ per_repo.extra ++ per_phase.extra` の順で結合。重複 flag の解釈は CLI に委ねる (last-wins は Claude CLI 仕様に従う)
- **secrets-in-argv 禁止**: orchestrator は argv element に対し `(?i)(--.*(?:token|secret|password|key).*)` の正規表現マッチを行い、ヒットしたら spawn を中止して `argv_secret_violation` エラー (RFC7807)
- secrets は **env vars 経由** で渡す。adapter は totsuka.toml の `[agent_adapter.repos.*]` から決定論的に env を生成し、herdr `agent.start` の `env` パラメータに乗せる

### 11.14 Task identity & branch / PR linkage

- **task_id** = `ProjectV2Item.id` (GitHub ProjectsV2 が発行する node ID。`PVTI_...` の形)。totsuka 側で UUID を発行しない
  - 理由: GitHub Project 上の item が単一の真実、そこから派生する識別子を使うことで watcher snapshot と effect ledger が同一キーで照合できる
  - DraftIssue (まだ issue 化されていない Inbox の項目) も `PVTI_...` を持つので問題ない
- **branch 命名規約** (orchestrator が決定し spawn payload に乗せて adapter に渡す):
  ```
  totsuka/{task_id_short}/{phase_short}
   - task_id_short = ProjectV2Item.id の末尾 12 文字 (短縮)
   - phase_short   = design | implv (impl_verify 略) など固定
  例: totsuka/abc123def456/implv
  ```
  - branch 衝突時: adapter が `git worktree add` で既存検出 → 409 `worktree_in_use` を返し orchestrator に判断を委ねる (通常は前回 worktree が残っている = 異常状態、通知)
- **PR ↔ task 紐付け**:
  - 主: branch 名から `task_id_short` を抽出 (orchestrator が `tasks.task_id_short → task_id` を逆引きできるよう INDEX を持つ)
  - 副: PR 本文末尾に **`Totsuka-Task: {full_task_id}` trailer** を必ず付ける (Claude への system prompt に instruction を含める)。branch 名が rename されたとしても trailer から復元可能
  - watcher は PR イベント時に両方を確認、不一致なら通知 + 副を優先 (人が renamed した可能性を尊重)
- **`tasks` テーブル列定義** (`migrations/0005_tasks.sql`):
  ```sql
  CREATE TABLE tasks (
    id                                    TEXT PRIMARY KEY,           -- ProjectV2Item.id
    task_id_short                         TEXT NOT NULL UNIQUE,       -- 末尾 12 文字、branch 用
    repo                                  TEXT NOT NULL,              -- owner/name
    pr_node_id                            TEXT,                       -- 紐付いた PR がある場合
    current_column                        TEXT NOT NULL,              -- ColumnId snake_case
    current_phase                         TEXT,                       -- design | impl_verify など
    impl_verify_attempt                   INT NOT NULL DEFAULT 0,     -- §11.15 で +1 される
    suppress_writeback_until_human_move   BOOLEAN NOT NULL DEFAULT FALSE,
    spawned_at                            TIMESTAMPTZ,                -- 現フェーズの spawn 時刻 (phase timeout 計測)
    created_at                            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                            TIMESTAMPTZ NOT NULL DEFAULT now()
  );
  CREATE INDEX idx_tasks_repo            ON tasks (repo);
  CREATE INDEX idx_tasks_task_id_short   ON tasks (task_id_short);
  CREATE INDEX idx_tasks_pr_node_id      ON tasks (pr_node_id) WHERE pr_node_id IS NOT NULL;
  ```

### 11.15 Effect re-entry semantics

親設計 §4.2 が `DiffBack` を許可しているため、同一 `(task_id, phase)` を複数回 spawn することがある。§11.1 の `effect_key = spawn:{task_id}:{phase}` 形式ではデデュープで skip されてしまう。

- **effect_key を `spawn:{task_id}:{phase}:{attempt}` に拡張**
  - `attempt` は `tasks.impl_verify_attempt` (`design_attempt` も同様に追加するなら別列)
  - orchestrator は DiffBack を検出したら `UPDATE tasks SET impl_verify_attempt = impl_verify_attempt + 1` を実行してから新 effect_key で claim
- **retry vs DiffBack の区別**:
  - retry (`retry_max=1`): フェーズ失敗時の自動再試行、attempt は **据え置き**、effect_key は同じ (idempotent skip でもう一度 lease 取得は可)
  - DiffBack: verifier が AC 未達と判断し戻されたケース、attempt **+1**、新 effect_key
  - 区別は `processed_effects.attempts` ではなく orchestrator の状態機械で行う (DiffBack は明示イベントなので衝突しない)
- **totsuka-core::key** 更新:
  ```rust
  pub fn spawn_effect_key(task_id: &str, phase: Phase, attempt: i32) -> String {
      format!("spawn:{}:{}:{}", task_id, phase.as_snake(), attempt)
  }
  ```

### 11.16 Adapter worktree GC

- 追加 totsuka.toml:
  ```toml
  [agent_adapter]
  worktree_failed_ttl_hours        = 72       # failed worktree を保持する時間 (調査用)
  worktree_orphan_scan_interval_secs = 3600   # scanner の周期
  ```
- adapter 内 tokio task が `worktree_orphan_scan_interval_secs` ごとに実行:
  1. 各リポについて `git worktree list --porcelain` を実行 (`spawn_blocking` 経由、§11.10)
  2. 取得した worktree path の集合を、`processed_effects.result` 内の生存 worktree と `qa_thread_agent` の生存 pane の集合と diff
  3. **どこからも参照されない worktree**:
     - 通常 (成功完了済): `git worktree remove --force` + `git branch -D <branch>`
     - failed-flag 付き: `last_modified < now() - worktree_failed_ttl_hours` のときのみ削除
  4. ログ: `worktree_gc total=N removed=M kept=K` (gauge: `worktree_gc_kept`)
- **ディスク逼迫保護** (将来拡張): 利用可能 disk space < 10% で scanner を強制実行 + 通知 (本書では設定だけ予約)
- restart 時: adapter は起動時に同 scanner を一度走らせ、前回 down 時に残った orphan を回収

---

## 12. 確定事項一覧

| 区分 | 確定事項 |
|---|---|
| アプリ分割 | 5 binary (totsukactl / agent-adapter / orchestrator / github-watcher / qa-service) + 4 共有 crate |
| 起動方式 | 自前 supervisor CLI `totsukactl`、daemon として常駐 |
| Postgres | docker compose 経由で `ghcr.io/pgmq/pg18-pgmq:v1.10.0` を起動、totsukactl が probe / 起動 / バージョン検証 |
| 起動順序 | postgres → preflight → agent-adapter → orchestrator → (github-watcher ∥ qa-service) |
| readyz | 全 bin が readyz 200 を返すまで supervisor は次フェーズに進まない (timeout 30s) |
| shutdown | 逆依存順 (ingestion → control → execution)、SIGTERM grace 15s、Claude pane は kill しない |
| 設定ファイル | 共通 `~/.config/totsuka/config.toml`、secrets 分離、env で上書き |
| GitHub Project 参照 | `[github] project_owner` + `project_number` 単一前提。watcher / orchestrator / qa-service が共通参照 |
| GitHub ingestion 方式 | **polling-only** (ProjectsV2 GraphQL snapshot diff、既定 20 秒)。ローカル NAT 配下に webhook を立てない。webhook モードは将来クラウド移行時の拡張余地 |
| ホットリロード | SIGHUP で `[agent_adapter.repos.*]` 追加など差分適用、不可項目混在は全 rollback |
| IPC (ローカル組) | HTTP over Unix Domain Socket (orchestrator / adapter / qa-service / supervisor 間) |
| IPC (cloud 候補) | github-watcher は TCP loopback (将来公開可) |
| heartbeat | supervisor 常駐 + healthz 5s / readyz 30s tick、restart_policy = on-dead-only 既定 |
| agent 状態 | totsuka 側で保持しない。herdr に完全委譲、必要な output だけ `pane.read` snapshot |
| 進行トリガ | GitHub の真実 (PR/CI/merge/release) を github-watcher が DomainEvent 化して bus へ。orchestrator は phase wall-clock timeout のみで補完 |
| 起動時リカバリ | adapter は `agent.list` から既存 pane を再追跡、orchestrator は sweeper で lease 期限切れ effect を再駆動 |
| Schema 互換 | bin に `MIN/TARGET_SCHEMA_VERSION` const、`schema_meta` テーブルで起動時ハンドシェイク (§11.1) |
| データ保持 | `processed_*` 週パーティション、orchestrator nightly で `events_weeks=4` 超を DROP (§11.2) |
| DR | `totsukactl backup/restore` で pg_dump、任意で WAL archive PITR (§11.3) |
| ColumnId | `totsuka-core::ColumnId` enum で正規化、`[github.columns]` で表示名 mapping (§11.4) |
| Clock | `Clock` trait + UTC ストレージ + `[totsuka].timezone` 表示 (§11.5) |
| Error/panic | lib は thiserror + RFC7807、bin は anyhow + `panic=abort`、UDS ハンドラは catch_unwind (§11.6) |
| Secrets | `Secret<T>` newtype で Debug 安全、rotation_warn_days 監視 (§11.7) |
| Back-pressure | 全 mpsc は bounded、bound と full 時動作を §11.8 で明示 |
| Metrics/trace | `/metrics` Prometheus + 任意 OTLP (§11.9) |
| Blocking | subprocess / 大文字列 / fs は `spawn_blocking` + semaphore、`block_in_place` は deny (§11.10) |
| Bootstrap | `totsukactl init` で XDG/config/secrets/compose/migrate を冪等実行 (§11.11) |
| Writeback 競合 | OCC + 人間優先 + suppression フラグ (§11.12) |
| Argv 規律 | append-only merge、secret-like flag は spawn 拒否、secrets は env のみ (§11.13) |
| qa-service repo 選択 | `llm_classify` 既定。`[qa_service.classifier].provider` で **anthropic / openai / openrouter / litellm / openai_compatible** を切替可能 (4 つ必須対応)。構造化出力は Anthropic tool_use / OpenAI json_schema で強制。`confidence_threshold=0.70` 未満は delegated reaction で本人選択 (§8.4)。`channel_map` モードは将来オプション |
| qa-service 回答フロー | **qa-service が回答 post まで担う**。adapter は中継のみ。完了検知は `<<TOTSUKA_DONE>>` sentinel を主、revision 停滞をフォールバック、`answer_timeout_secs=180` で打ち切り (§8.4)。`agent.events.subscribe` は使わず pane.read snapshot のみで判定し §9 と整合 |
| qa-service スレッド継続 | `qa_thread_agent (thread_ts→terminal_id)` を DB 永続化、同一 thread_ts の発言は同一 pane に `agent.send`。`pane_idle_ttl_secs=1800` で pane.close、restart 時は `agent.list` と突き合わせて orphan を回収 |
| Task identity | `task_id = ProjectV2Item.id`。branch `totsuka/{task_id_short}/{phase_short}` (orchestrator 採番)、PR 紐付けは branch 主 + `Totsuka-Task:` trailer 副 (§11.14) |
| Orchestrator conversation driver | agent 出力ベースの完了検知はせず、**PR/CI/review** など GitHub signal で進める。conversation.rs は verifier 起動 + 入力組立のプリミティブのみ、独自完了検知なし (§9.3) |
| Effect re-entry | DiffBack 時は `effect_key = spawn:{task_id}:{phase}:{attempt}` の attempt を +1。retry (`retry_max=1`) は attempt 据え置きで同じ effect_key (§11.15) |
| Worktree GC | `worktree_failed_ttl_hours=72` / scanner 周期 `3600s`、orphan は `git worktree remove --force + branch -D` (§11.16) |
| 通知 (Notifier) | `totsuka-telemetry::notify` に集中。`NotifyKind` enum + dedup_key (caller 責任) + per-sink rate-limit + `${state_dir}/notify_state.json` 永続化。sink = log (常時) / slack (optional) / github (将来)。種別→宛先写像はコード写像 (§13) |

---

## 13. 通知 (Notifier)

親設計 §14 / O17 (種別非依存ディスパッチャ) を具体化する。全 bin が共通 API `totsuka-telemetry::notify` を通じて通知を発火し、本セクションのルールで dedup / rate-limit / 多重 sink 配信される。

### 13.1 NotifyKind (種別)

```rust
pub enum NotifyKind {
    HumanGate1,            // 設計レビュー到達 (親 §4.1)
    HumanGate2,            // 最終レビュー到達
    TaskFailed,            // retry 超過 / 致命的失敗
    TaskStuck,             // phase wall-clock timeout 超過 (Blocked)
    GivingUp,              // restart_max_attempts 超過
    ProcessDead,           // supervisor が子プロセス死亡を検知
    ProcessUnhealthy,      // supervisor が unhealthy 判定 (notify_on_degraded=true 時のみ)
    PgmqDead,              // Postgres コンテナ異常
    ConfigError,           // リポ未登録 / column mapping 不正 / 等 (§5 / §11.4)
    SecretRotationWarn,    // §11.7 token 期限警告
    WritebackConflict,     // §11.12 OCC 競合 (suppress フラグ立て)
    ArgvSecretViolation,   // §11.13 secrets-in-argv 検出
    QaSpawnFailed,         // §8.4 adapter spawn 失敗
    QaAnswerTimeout,       // §8.4 sentinel/revision 停滞両方失敗
    WorktreeGcAlert,       // §11.16 ディスク逼迫等
}
```

新種別の追加は **コード修正のみ** (親設計 O17 のとおり)。各種別の宛先・dedup・rate-limit はコード写像で定義し、設定は wiring (宛先 enable / TTL 上書き) のみ。

### 13.2 API

```rust
// totsuka-telemetry::notify
pub struct NotifyPayload {
    pub title:       String,                  // 1 行サマリ
    pub body:        String,                  // 詳細 (markdown 可)
    pub fields:      Vec<(String, String)>,   // 構造化 (Slack attachment field 等)
    pub link:        Option<String>,          // 関連 URL (Project item / PR)
    pub trace_id:    Option<String>,
}

pub struct Notifier { /* sinks, dedup state, rate buckets */ }

impl Notifier {
    pub async fn notify(&self,
        kind: NotifyKind,
        dedup_key: impl Into<String>,    // 例: "stuck:task:PVTI_abc123"
        payload: NotifyPayload,
    );
}
```

呼出例:
```rust
notifier.notify(
    NotifyKind::TaskStuck,
    format!("stuck:task:{}", task.id),
    NotifyPayload { title: "Task stuck", body: "...", ..Default::default() },
).await;
```

### 13.3 Dedup

- **dedup_key は caller 責任**。同じ種別でも対象 (task / repo / channel) ごとに別キーを与える
- 種別ごとの TTL: `[notifications.dedup_ttl_secs]` で設定 (下記既定)、なければ `dedup_default_secs`
- TTL 内の重複 notify は **drop** (in-memory + persisted state を確認)
- 永続化: `${state_dir}/notify_state.json` に `{dedup: {key: last_sent_ts}}` を atomic write。restart をまたいでも一度きりが守られる
- **TTL=0 は dedup 無効** (毎回通知。HumanGate / TaskFailed / GivingUp / ProcessDead 等)

### 13.4 Rate limit (per-sink)

- 各 sink に **token bucket** (`capacity, refill_per_min`)。bucket が空なら notify は **postponed queue** に積み、次の refill で送信
- queue 上限を超えたら **drop oldest** + warn ログ + metric `notify_dropped_total{sink}`
- bucket 状態も `notify_state.json` に永続化 (`rate_buckets: {sink: {tokens, last_refill}}`)

### 13.5 Sinks (宛先)

| sink | 種別 | 設定 |
|---|---|---|
| `log` | 常時有効 | tracing 経由で `WARN` 以上として構造化ログ (sink としては必ず通る、disable 不可) |
| `slack` | optional | `[notifications.slack].webhook_url` 設定時のみ |
| `github_project_comment` | optional (将来) | Project item にコメント。`[notifications.github].enabled = true` で有効化 |

各種別ごとの **宛先写像** はコード (`totsuka-telemetry::notify::routing::ROUTING_TABLE`) に集中:

```rust
// 例: 写像テーブル (擬似)
TaskStuck       => [Log, Slack],
HumanGate1      => [Log, Slack],
ProcessDead     => [Log, Slack],
SecretRotationWarn => [Log, Slack],
ConfigError     => [Log, Slack],
QaAnswerTimeout => [Log],            // 通知ノイズなので log のみ
WorktreeGcAlert => [Log],            // 同上
ArgvSecretViolation => [Log, Slack], // セキュリティ重要
```

新種別追加は写像テーブルへの行追加のみ。

### 13.6 設定スキーマ (§6 への追加)

```toml
[notifications]
config_error_notify   = true     # ConfigError 種別を発火するか (既存)
dedup_default_secs    = 600
rate_limit_per_min    = 30       # 全 sink 合計の安全上限

[notifications.dedup_ttl_secs]   # 種別 → TTL 秒 (0 = dedup 無効)
human_gate1            = 0
human_gate2            = 0
task_failed            = 0
task_stuck             = 3600
giving_up              = 0
process_dead           = 0
process_unhealthy      = 600
pgmq_dead              = 600
config_error           = 1800
secret_rotation_warn   = 86400
writeback_conflict     = 3600
argv_secret_violation  = 0
qa_spawn_failed        = 300
qa_answer_timeout      = 600
worktree_gc_alert      = 3600

[notifications.slack]
webhook_url            = ""       # 空文字で sink 無効
default_channel        = "#totsuka"
channel_overrides      = {}       # 種別 → channel (例: { human_gate1 = "#review" })
bucket_capacity        = 10
bucket_refill_per_min  = 5

[notifications.log]
# log sink は disable 不可、設定なし

[notifications.github]
enabled                = false    # 将来用、現状未実装
```

### 13.7 観測

- metrics:
  - `notify_total{kind, sink, outcome}` (outcome = `sent` / `deduped` / `rate_limited` / `dropped` / `sink_error`)
  - `notify_dedup_state_size` (gauge)
  - `notify_queue_depth{sink}` (gauge)
- ログ: notify ごとに `info!(kind, dedup_key, sinks=..., deduped, rate_limited)` を 1 行
- restart 時: `notify_state.json` 読み込み失敗は warn + state ゼロから再開 (絶対に startup を止めない)

### 13.8 既存セクションとの接続

- §11.7 SecretRotationWarn は本 API で発火 (TTL=86400)
- §11.12 WritebackConflict も本 API
- §11.16 WorktreeGcAlert も本 API
- §7 heartbeat の dead/unhealthy 通知も本 API (`ProcessDead` / `ProcessUnhealthy`)
- §8.4 qa-service の `qa.spawn_failed` / `qa_answer_timeout` も本 API
- 既存 §5 hot reload のリポ未登録通知も `ConfigError` 種別で本 API

これにより、各 bin は **「種別と dedup_key を選ぶだけ」** で通知でき、宛先・dedup・rate-limit はすべて `totsuka-telemetry` 側に集中する (親 O17 の趣旨と一致)。

---

## 14. 未確定事項 / 次に決めるべきこと

本書のスコープ外として、実装着手時に確定するもの。

- 各 bin の `Cargo.toml` の正確な dependency 版数
- HTTP client over UDS の crate 選定 (`hyperlocal` か手書きか)
- Slack `slack-morphism` の adapter 詳細 (Socket Mode の retry 設計)
- (削除済) 通知ディスパッチャは §13 で確定
- e2e テストハーネス (docker compose で pgmq + herdr モック)
- リリース / 配布方法 (mise / cargo install / Homebrew tap)
- OpenAPI / JSON Schema による IPC スキーマ自動生成の要否
- classifier の `totsuka-llm` crate 化 (orchestrator/notifier で LLM が必要になった時点で昇格)
- 他 provider 追加 (Google Gemini native / Vertex / Cohere 等) の要否
- LiteLLM proxy 自体を totsuka スタックに含めるか (現状は外部前提)
