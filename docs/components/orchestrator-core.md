---
type: Component
title: orchestrator-core クレート
description: totsuka のコア。ヘキサゴナルアーキテクチャの domain（ドメイン・ステートマシン）/ ports（TaskSource・AgentIde・LlmRouter・SecretStore 等の trait）/ adapters（JSON-RPC ブリッジ・SQLite・Keychain）を担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [rust, crate, core, hexagonal, xdg, platform, config, sqlite, statemachine, logging, plugin, worktree, git]
timestamp: 2026-07-12T01:10:00Z
status: active
owner: tomoya-k31
---

# 責務

totsuka のビジネスロジックの中核。外部 I/O を持たず、ports の trait 境界を介してのみ外界とやり取りする（ヘキサゴナル）。OS 依存機能は `platform` に隔離する（§5.6）。

# モジュール構成

| モジュール | 責務 | 実装タスク |
|---|---|---|
| `domain` | 純粋なドメイン型とタスクステートマシン（`domain::state`: `transition(from,event)` 純関数、9 状態、F-71） | #48 / #54 ほか |
| `ports` | 差し替え対象の trait 境界（`SecretStore` / `ProcessProbe`、後続で `TaskSource` / `AgentIde` / `LlmRouter` / 永続化） | 各機能タスク |
| `paths` | XDG Base Directory 準拠のパス解決（`config`/`data`/`state`/`cache`/`runtime`、`totsuka` サフィックス、未設定時 `$HOME` フォールバック）。macOS でも XDG を明示採用 | #46 |
| `platform` | OS 依存実装の隔離。`platform::macos`（Keychain = keyring クレート）、`platform::unix`（`ProcessProbe` = `kill(pid,0)`）、`platform::fallback`（非 macOS の未サポート SecretStore）。`PlatformSecretStore` / `PlatformProcessProbe` で現行 OS の実装を選ぶ | #46 |
| `config` | 設定ロードと検証。`schema`（`config.toml` パース、F-60/61/64）、`raw`（`plugins/{name}.toml` を無解釈保持 → JSON、F-64）、`resolve`（`${ENV}`/`keychain:` シークレット解決・`~`/`${ENV}` パス展開、F-62/65）、`layered`（CLI>env>plugin-file>config-default の優先順位、F-66）、`validate`（静的検証、F-63/58）、`edit`（`toml_edit` で `[plugins.{name}] enabled` のみ書き換え・コメント整形保持、F-57） | #47 / #52 |
| `plugins` | プラグインの on-disk ストア（`store`）。install（prepare→confirm→commit の2段、SHA-256 表示 §5.4、manifest/protocol 互換検証 F-54）・uninstall・list。install=バイナリの存在、enabled=設定の宣言を分離（F-56） | #52 |
| `worktree` | git worktree ライフサイクル（F-20〜25/85）。作成（fetch→origin/{default} を commit 解決して分岐→worktree add、並列安全）、掃除（immediate/retention/manual、未コミットは skip）、孤児検出。ブランチ/配置テンプレート描画とサニタイズ。git は `ports::GitRunner` 越し（`adapters::git::SystemGitRunner`） | #53 |
| `logging` | 構造化ログと機密マスキング。`redact`（フィールド denylist＋値パターン）、`layer`（redact 済み JSON Lines / 人間可読を出力する tracing レイヤ）、`rotation`（日次ログの世代保持）。規約は [ログ規約](/development/logging-conventions.md) | #49 |
| `adapters` | ports の具象実装。`state_db`（SQLite 状態永続化・埋め込みマイグレーション・イベントログ・冪等取り込み → [state.db スキーマ](/data/state-db.md)）、`run_lock`（多重起動防止 F-74）、`plugin_host`（プラグインをサブプロセス起動し NDJSON JSON-RPC で通信、initialize/protocol 互換チェック/リクエスト相関/タイムアウト/クラッシュ隔離/config·validate 委譲、F-51/54/58/59/65, §5.3） | #48 / #51 |

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
