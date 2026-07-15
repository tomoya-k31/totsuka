---
type: Component
title: orchestrator-core クレート
description: totsuka のコア。ヘキサゴナルアーキテクチャの domain（ドメイン・ステートマシン）/ ports（TaskSource・AgentIde・LlmRouter・SecretStore 等の trait）/ adapters（JSON-RPC ブリッジ・SQLite・Keychain）を担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [rust, crate, core, hexagonal, xdg, platform, config, sqlite, statemachine, logging, plugin, worktree, git, workflow, scheduler, llm, repo-select, recovery, session, run, output]
timestamp: 2026-07-15T15:00:00Z
status: active
owner: tomoya-k31
---

# 責務

totsuka のビジネスロジックの中核。外部 I/O を持たず、ports の trait 境界を介してのみ外界とやり取りする（ヘキサゴナル）。OS 依存機能は `platform` に隔離する（§5.6）。

# モジュール構成

| モジュール | 責務 | 実装タスク |
|---|---|---|
| `domain` | 純粋なドメイン型。`state`（`transition(from,event)` 純関数、9 状態、F-71）、`workflow`（`source×trigger×mode×agent×output` 定義の解釈・トリガーマッチング（定義順 first-match F-81・status/label 防御的再判定）・検証（plan×pull_request エラー F-82、output=source の capability 検証 F-83、trigger 重複警告 F-81）・on_success/on_failure=OutcomeAction F-84） | #48 / #54 |
| `ports` | 差し替え対象の trait 境界（`SecretStore` / `ProcessProbe` / `LlmRouter` / `GitRunner` / `AgentSession`（再起動時のセッション再接続 F-37）、後続で `TaskSource` / `AgentIde`） | 各機能タスク |
| `paths` | XDG Base Directory 準拠のパス解決（`config`/`data`/`state`/`cache`/`runtime`、`totsuka` サフィックス、未設定時 `$HOME` フォールバック）。macOS でも XDG を明示採用 | #46 |
| `platform` | OS 依存実装の隔離。`platform::macos`（Keychain = keyring クレート）、`platform::unix`（`ProcessProbe` = `kill(pid,0)`）、`platform::fallback`（非 macOS の未サポート SecretStore）。`PlatformSecretStore` / `PlatformProcessProbe` で現行 OS の実装を選ぶ | #46 |
| `config` | 設定ロードと検証。`schema`（`config.toml` パース、F-60/61/64）、`raw`（`plugins/{name}.toml` を無解釈保持 → JSON、F-64）、`resolve`（`${ENV}`/`keychain:` シークレット解決・`~`/`${ENV}` パス展開、F-62/65）、`layered`（CLI>env>plugin-file>config-default の優先順位、F-66）、`validate`（静的検証 F-63/58 ＋ ワークフロー検証 F-81/82/83 を統合する `validate()` エントリ、Error/Warning の Finding を返す）、`edit`（`toml_edit` で `[plugins.{name}] enabled` のみ書き換え・コメント整形保持、F-57） | #47 / #52 / #54 |
| `plugins` | プラグインの on-disk ストア（`store`）。install（prepare→confirm→commit の2段、SHA-256 表示 §5.4、manifest/protocol 互換検証 F-54）・uninstall・list。install=バイナリの存在、enabled=設定の宣言を分離（F-56） | #52 |
| `repo_select` | リポジトリ自動選択（F-10〜15）。repo_hint 解決を最優先、未解決なら LLM 分類（候補=概要＋README先頭N行）。confidence 低/未知repo（1回リトライ後）は pending、API 恒久失敗は failed。`reason` は --dry-run 用。`ReadmeCache`（README hash キャッシュ、F-15）。LLM は `ports::LlmRouter`（OpenAI 互換）越し、reqwest 実装は `adapters::llm::OpenAiRouter`（指数バックオフ §5.3） | #56 |
| `scheduler` | 並列実行制御（F-40〜45）。3 階層スロット（global/repo/agent）を全て満たすと dispatch 可、`counts_toward_slot` は dispatched/running/publishing のみ計上（waiting_input は解放）。DB から `rebuild` で再構築。`PriorityQueue`（priority 降順→FIFO）、`plan_dispatch`（優先度順にスロット確保） | #55 |
| `worktree` | git worktree ライフサイクル（F-20〜25/85）。作成（fetch→origin/{default} を commit 解決して分岐→worktree add、並列安全）、掃除（immediate/retention/manual、未コミットは skip）、孤児検出。ブランチ/配置テンプレート描画とサニタイズ — `render_branch` は task_id 由来の git ref 禁止文字（`:` 空白 `~^?*[\` 制御文字）を `-` へ合法化する（task_id はソース定義で、Slack は `{channel}:{ts}`。git 制約は git 境界で一括吸収、#108） | #53 |
| `logging` | 構造化ログと機密マスキング。`redact`（フィールド denylist＋値パターン）、`layer`（redact 済み JSON Lines / 人間可読を出力する tracing レイヤ）、`rotation`（日次ログの世代保持）。規約は [ログ規約](/development/logging-conventions.md) | #49 |
| `recovery` | 再起動回復（F-37/44、§5.3）。`recover` が slot 計上状態のタスクを `session/attach` で再接続し、エージェント状態にステートマシンを前方同期（`resume_plan`）／attach 失敗は「継続確認待ち」（`RecoveryReport`、**自動 failed にしない**）。`retry_plan`（worktree＋セッション再利用 F-44）・`active_slot_claims`（`SlotManager::rebuild` 用 (repo,plugin)）。詳細 → [state.db スキーマ](/data/state-db.md) | #57 |
| `run` | `run` メインループ（§5.1）。`Engine` が fetch→防御的再マッチ→冪等取り込み（F-73）→リポジトリ選択→スロット確保→worktree 作成→`task/dispatch`→`state/subscribe` の 1 サイクルと、`state/notification` 駆動の監視（waiting_input=スロット解放＋Notifier 配送 F-35/45、done=出力ポリシー実行→`on_success`/`on_failure` 書き戻し F-84→worktree 掃除 F-23/85、retry は worktree＋セッション再利用 F-44、プラグインクラッシュ=failed §5.3）を統合。ワンショット（既定: 全 dispatch 済みタスクが終端/待機に達したら summary で終了）／`--watch`（ソース別ポーリング間隔 F-06、shutdown future で graceful 停止）／`dry_run`（副作用ゼロで判断根拠を報告）。`settings_from_config` が `RootConfig` を解釈（`max_concurrency` F-40、`poll_interval_secs` F-06、`[worktree] cleanup`/`plan_cleanup` F-23/85 — implement 既定 manual・plan 既定 immediate、`[output]` PR テンプレート）。`[llm]` 未設定時は LLM が必要な選択を pending へフォールバック（F-14）。`run::output` サブモジュール（#65）が出力ポリシーを実装: `pull_request`=コミット存在検証（`WorktreeManager::has_commits_to_publish`、ゼロなら failed）→Orchestrator が push（`push_branch`、エージェントは push しない F-86）→PR 作成（`PrCreator` seam、実装 `GhPrCreator`=`gh pr create`、テンプレート `{title}`/`{url}`/`{summary}` 等）、`source`=蓄積したエージェント出力を `result/publish` へ（F-07）、`none`=素通し。plan×pull_request は publish 点でも防御的に拒否（F-82）。publish 失敗は worktree・コミット・セッションを保持したまま failed とし `task retry` で再開可能 | #63 / #65 |
| `adapters` | ports の具象実装。`state_db`（SQLite 状態永続化・埋め込みマイグレーション・イベントログ・冪等取り込み・セッション永続化（`record_session`/`latest_session`/`list_sessions` F-37）→ [state.db スキーマ](/data/state-db.md)）、`run_lock`（多重起動防止 F-74）、`plugin_host`（プラグインをサブプロセス起動し NDJSON JSON-RPC で通信、initialize/protocol 互換チェック/リクエスト相関/タイムアウト/クラッシュ隔離/config·validate 委譲、F-51/54/58/59/65, §5.3）、`agent_session`（`PluginAgentSession`＝`AgentSession` の実装、`session/attach`＋`state/subscribe` 再確立 F-37） | #48 / #51 / #57 |

## 公開型（#46 時点）

- `paths::Paths` — `from_system()` / `from_env(fn)`（テスト用に環境注入可）、各 `*_dir()` アクセサ。
- `ports::SecretString` — `Debug`/`Display` で値を露出しない newtype（§5.2 のログ流出防止）。`expose()` で明示的に取り出す。
- `ports::SecretRef` — `keychain:<service>/<account>` のパース済み表現。
- `ports::SecretStore` / `ports::ProcessProbe` — OS 依存機能の trait 境界。

# 依存

- `thiserror`（エラー型）。`libc`（unix、プロセス生存確認）。`keyring`（macOS 限定、Keychain）。
- 参照元: [orchestrator-cli](/components/orchestrator-cli.md)。

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §6 技術要件](/product/orchestrator-spec.ja.md)
