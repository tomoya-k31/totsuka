---
type: Guide
title: 設定リファレンス（config.toml）
description: config.toml と plugins/{name}.toml の全キー・デフォルト値・意味の一覧。シークレット参照、ワークフロー、出力ポリシー、掃除ポリシー、並列上限を含む。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/config/schema.rs
tags: [config, reference, toml, secrets, workflow, worktree]
timestamp: 2026-07-14T03:00:00Z
status: active
owner: tomoya-k31
---

# 場所

- 共通設定: `$XDG_CONFIG_HOME/totsuka/config.toml`（既定 `~/.config/totsuka/config.toml`）
- プラグイン個別設定: `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml`（Orchestrator は無解釈で保持し、シークレット解決後に `initialize` へ渡す）
- `--config <path>` で config.toml の場所を上書き可能（最上位の優先レイヤ）

`totsuka init` が雛形を生成する。`totsuka config validate` で検証、`totsuka config show [--redacted]` で表示。

# シークレット参照

文字列値は次のいずれか。プレーンなシークレットは設定に書かない。

- `keychain:<service>/<account>` — macOS Keychain から解決
- `${ENV_VAR}` を含む文字列 — 環境変数から展開
- `~` / `${ENV}` はパスでも展開される

# トップレベル

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `version` | int | 1 | 設定スキーマ版（起動時マイグレーション用） |
| `max_concurrency` | int? | 4 | グローバル同時実行上限（F-40） |
| `[[repositories]]` | 配列 | — | 対象リポジトリ（下記） |
| `[plugins.{name}]` | テーブル | — | プラグインのロスター + 共通項目（下記） |
| `[[workflows]]` | 配列 | — | ワークフロー定義（下記） |
| `[llm]` | テーブル | なし | AI Gateway 設定（下記）。無い場合、LLM が必要なリポジトリ選択は `pending` にフォールバック |
| `[worktree]` | テーブル | — | worktree 配置・掃除（下記） |
| `[log]` | テーブル | — | ログ設定（下記） |
| `[output]` | テーブル | — | 出力ポリシーの PR テンプレート（下記） |

# `[[repositories]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ブランチ名・ログで使う安定 ID |
| `path` | string | 必須 | ローカルクローンのパス（`~`/`${ENV}` 展開） |
| `summary` | string? | なし | LLM リポジトリ選択の説明（F-11） |
| `default_agent` | string? | なし | 既定 agent_ide プラグイン |
| `max_concurrency` | int? | 無制限 | リポジトリ単位の同時実行上限（F-41） |
| `worktree_location` | string? | `[worktree].location` | このリポジトリの worktree 配置テンプレート上書き |

# `[plugins.{name}]`

`{name}` はワークフローの `source` / `agent` と対応するインスタンス名。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `enabled` | bool | false | 有効化フラグ（F-56）。`totsuka plugin enable/disable` でも操作 |
| `kind` | enum | 必須 | `task_source` / `agent_ide` / `notifier` |
| `max_concurrency` | int? | 無制限 | agent プラグイン単位の同時実行上限（F-42） |
| `timeout_secs` | int? | 120 | RPC タイムアウト秒 |
| `log_level` | string? | なし | プラグインのログレベル |
| `poll_interval_secs` | int? | 60 | `run --watch` のポーリング間隔（task_source のみ、F-06） |

# `[[workflows]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ワークフロー名 |
| `source` | string | 必須 | task_source インスタンス名 |
| `trigger` | テーブル | `{}`（全マッチ） | トリガー条件。`status`/`project_status`/`label`/`labels` は Orchestrator が防御的に再判定、他キーはプラグインが `tasks/fetch` で解釈 |
| `mode` | enum | 必須 | `plan`（push/PR 禁止 F-82）/ `implement` |
| `agent` | string | 必須 | agent_ide インスタンス名 |
| `output` | enum | 必須 | `pull_request` / `source` / `none` |
| `on_success` | `{ set_status = "..." }`? | なし | 成功時にソース側ステータスを更新（F-84） |
| `on_failure` | `{ set_status = "..." }`? | なし | 失敗時にソース側ステータスを更新（publish 失敗など retry 可能な失敗では書き戻さない） |

定義順に first-match（F-81）。同一ソース内でトリガーが重なると警告。

# `[llm]`（AI Gateway）

OpenAI 互換 `/chat/completions` を前提。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `base_url` | string | 必須 | ベース URL（例 `https://openrouter.ai/api/v1`） |
| `model` | string | 必須 | モデル名 |
| `max_tokens` | int? | 256 | 分類呼び出しの最大トークン |
| `timeout_secs` | int? | 30 | リクエストタイムアウト |
| `api_key_ref` | string? | なし | API キーのシークレット参照 |

# `[worktree]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `location` | string? | `${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{branch}` | 配置テンプレート。`{repo}`/`{repo_name}`/`{branch}`/`{task_id}`/`{source}`/`${ENV}`/`~` を展開 |
| `cleanup` | policy? | `manual` | implement モードの掃除ポリシー（F-23） |
| `plan_cleanup` | policy? | `immediate` | plan モードの掃除ポリシー（F-85） |

掃除ポリシー値: `"immediate"` / `"manual"` / `{ retention_days = 5 }`。未コミット変更のある worktree は決して削除しない。

# `[log]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `level` | string? | info | `error`/`warn`/`info`/`debug`/`trace`（`--debug` で debug に引き上げ） |
| `log_prompts` | bool | true | プロンプト/ペイロードを記録（debug 以上でのみ実出力、§5.2） |
| `max_files` | int? | 7 | 日次ログの保持世代数 |

# `[output]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `pr_title_template` | string? | `{title}` | PR タイトルテンプレート。`{title}`/`{task_id}`/`{source}` |
| `pr_body_template` | string? | 組み込み既定 | PR 本文テンプレート。`{title}`/`{url}`/`{source}`/`{task_id}`/`{summary}` |

# 例

`[Spec §4.6/§4.9](/product/orchestrator-spec.ja.md)` の例が `totsuka init` の雛形にコメントアウトで含まれる。設計→実装ハンドオフの典型:

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
```
