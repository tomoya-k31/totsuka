---
type: Guide
title: 設定例集（config.toml / plugins/*.toml）
description: そのまま貼って動く config.toml の完全版注釈付き例と、選択肢を持つキー（kind・mode・output・verification・cleanup・trigger・シークレット参照・並列上限）の選び分け基準、TOTSUKA_* 環境変数オーバーライドの対応表、および最小構成／GitHub Projects／Slack／設計→実装ハンドオフのシナリオ別レシピ。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/init_cmd.rs
tags: [config, toml, examples, recipes, workflow, secrets, slack, github, herdr, environment]
generated: { by: human:tomoya-k31, at: 2026-07-31T17:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# このドキュメントについて

[設定リファレンス](/development/config-reference.md) が「全キーの一覧・型・既定値」を扱うのに対し、
本ドキュメントは **実際に貼って動く設定例**と、**選択肢のあるキーをどう選ぶか**を扱う。

- キーの意味を引きたい → [設定リファレンス](/development/config-reference.md)
- 何をどう書けばいいか知りたい → 本ドキュメント

雛形は `totsuka init` が生成する。書いたら必ず `totsuka config validate` を通すこと（後述）。

# ファイル配置と優先順位

| ファイル | 場所 | 役割 |
|---|---|---|
| `config.toml` | `$XDG_CONFIG_HOME/totsuka/config.toml`（既定 `~/.config/totsuka/config.toml`） | Orchestrator 本体の設定 |
| `plugins/{name}.toml` | `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml` | プラグイン個別設定。Orchestrator は無解釈で保持し、シークレット解決後に `initialize` で渡す |

`{name}` は `config.toml` の `[plugins.{name}]` のインスタンス名と一致させる。
`--config <path>` で `config.toml` の場所を上書きすると、`plugins/` の探索基準も**そのファイルの親ディレクトリ**に移る。

優先順位（上が強い）:

1. CLI フラグ（`--config`、`--debug`）
2. 環境変数 `TOTSUKA_*`（[対応表は後述](#環境変数によるオーバーライド)）
3. `plugins/{name}.toml`
4. `config.toml`

設定テーブルはいずれも `deny_unknown_fields`。**キーを 1 文字打ち間違えると既定値へのフォールバックではなくパースエラーになる**（これは意図的な設計で、typo が黙って無視される事故を防ぐ）。

ただし次の 3 つは例外で、**typo が黙って無視される**ので注意すること:

- `trigger` / `on_success` / `on_failure` — 中身は無検査の TOML テーブル。`on_success = { set_statuss = "..." }` はパースを通り、実行時に黙って捨てられる
- `cleanup` / `plan_cleanup` の `{ retention_days = N }` 形式 — untagged なので余分なキーを書いてもエラーにならない

# 環境変数によるオーバーライド

`config.toml` を書き換えずに値を差し替えたいとき（CI・コンテナ実行など）は `TOTSUKA_*` を使う。
対応表にある変数だけが解釈され、`config.toml` の値に勝つ。CLI フラグには負ける。
ホワイトリスト方式を採った理由と fail-loud 方針の背景は [ADR-0009](/decisions/adr-0009-env-override-whitelist.md) を参照。

対応する変数（これ以外の `TOTSUKA_*` は解釈されない）:

| 環境変数 | 適用先 | 型・検証 |
|---|---|---|
| `TOTSUKA_MAX_CONCURRENCY` | `max_concurrency` | 非負整数 |
| `TOTSUKA_LOG_LEVEL` | `[log].level` | `error`/`warn`/`info`/`debug`/`trace` |
| `TOTSUKA_LOG_PROMPTS` | `[log].log_prompts` | `true` / `false` のみ |
| `TOTSUKA_LOG_MAX_FILES` | `[log].max_files` | 非負整数 |
| `TOTSUKA_WORKTREE_LOCATION` | `[worktree].location` | 文字列（`~` / `${ENV}` 展開は従来どおり後段で行われる） |
| `TOTSUKA_HOOKS_AUTH_TOKEN_REF` | `[hooks].auth_token_ref` | 文字列（**シークレット参照**。値そのものではない） |
| `TOTSUKA_HOOKS_SOCKET_PATH` | `[hooks].socket_path` | 文字列 |
| `TOTSUKA_HOOKS_SPOOL_DIR` | `[hooks].spool_dir` | 文字列 |
| `TOTSUKA_HOOKS_BLOCK_RETRY_LIMIT` | `[hooks].block_retry_limit` | 非負整数 |
| `TOTSUKA_LLM_BASE_URL` | `[llm].base_url` | 文字列 ※ |
| `TOTSUKA_LLM_MODEL` | `[llm].model` | 文字列 ※ |
| `TOTSUKA_LLM_MAX_TOKENS` | `[llm].max_tokens` | 非負整数 ※ |
| `TOTSUKA_LLM_TIMEOUT_SECS` | `[llm].timeout_secs` | 非負整数 ※ |
| `TOTSUKA_LLM_API_KEY_REF` | `[llm].api_key_ref` | 文字列（シークレット参照）※ |

※ `[llm]` は `base_url` + `model` が必須のテーブルなので、**env だけからは合成しない**。
`config.toml` に `[llm]` が無い状態で `TOTSUKA_LLM_*` を設定すると起動エラーになる（黙って無視はしない）。

**スコープ外**: 配列・動的キー（`[[repositories]]` / `[[workflows]]` / `[plugins.{name}]`）は
環境変数名で一意に指し示せないため対象外。`plugins/{name}.toml` の中身も Orchestrator が解釈しない
領域（優先順位の第 3 層）なので対象外で、そこへの env 適用はプラグイン側の責務。

## 不正値・typo の扱い

| ケース | 挙動 |
|---|---|
| 対応表の変数の値が型変換・検証に失敗（`TOTSUKA_MAX_CONCURRENCY=abc`） | **起動エラー**（変数名・値・期待型を表示） |
| `TOTSUKA_LLM_*` を設定したが `[llm]` が無い | **起動エラー** |
| 対応表に無い `TOTSUKA_*`（typo した `TOTSUKA_MAX_CONCURENCY` 等） | **警告**（stderr）。起動は継続 |
| 値が空文字列（`TOTSUKA_MAX_CONCURRENCY=`） | **警告 + 未設定扱い**（シェルの「空 = unset」慣習） |

いま有効になっているオーバーライドは `totsuka config show` の末尾に一覧表示される
（`--redacted` を付けると `..._TOKEN_REF` / `..._KEY_REF` の値はマスクされる）。

## 注入系の環境変数との違い（混同注意）

`TOTSUKA_` で始まる環境変数には**逆向きの別系統**がある。Orchestrator がエージェント／フック
プロセスへ**注入する**もので、設定オーバーライドではない（対応表にも無く、警告も出ない）。

| 系統 | 変数 | 向き | 役割 |
|---|---|---|---|
| 設定オーバーライド | 上の対応表の 14 個 | 人／CI → Orchestrator | `config.toml` の値を差し替える |
| 注入（フック） | `TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` / `TOTSUKA_HOOK_SPOOL_DIR` / `TOTSUKA_PROMPT_CONTEXT` | Orchestrator → エージェント pane | フックスクリプトへジョブ固有値を渡す（[フックシグナルフロー](/architecture/hook-signal-flow.md)） |

特に紛らわしいのが 1 字違いのこの 2 つ:

- `TOTSUKA_HOOK_SPOOL_DIR`（単数 `HOOK_`）— **注入**。フックスクリプトが POST 失敗時の書き出し先として読む
- `TOTSUKA_HOOKS_SPOOL_DIR`（複数 `HOOKS_`）— **オーバーライド**。`[hooks].spool_dir` を差し替える

## Examples

CI で並列度を落とし、ログを冗長にする:

```bash
TOTSUKA_MAX_CONCURRENCY=1 TOTSUKA_LOG_LEVEL=debug totsuka run
```

コンテナ実行でパス類を差し替える（イメージ内の `config.toml` は共通のまま）:

```bash
docker run \
  -e TOTSUKA_WORKTREE_LOCATION=/work/worktrees/{repo_name}/{worktree_name} \
  -e TOTSUKA_HOOKS_SOCKET_PATH=/run/totsuka/agent-events.sock \
  -e TOTSUKA_HOOKS_SPOOL_DIR=/var/lib/totsuka/spool \
  totsuka run --watch
```

`[hooks].socket_path` を上書きした場合、`totsuka focus` / `totsuka doctor` も**同じ変数を見せて**
実行すること。これらは同じキーから `run` がバインドしたソケットを解決するため、片方だけに
設定すると別のソケットを見に行く。

# Part 1: 完全版 config.toml

全キーを含む、そのまま `totsuka config validate` を通せる例。
実運用では不要な行を削って使う（**すべてのキーを書く必要はない。既定値で十分なものは書かない方が良い**）。

```toml
# ── トップレベル ────────────────────────────────────────────
version = 1              # 設定スキーマ版。現在 1 のみ。1 以外はエラー
max_concurrency = 4      # 全体の同時実行タスク上限（省略時 4）
default_tool = "claude"  # グローバル既定の AI ツール（#196。省略時も claude）

# ── AI ツールレジストリ（#196） ──────────────────────────────
# 組み込み既定 `claude` / `codex` / `opencode` があるため、通常このセクションは不要。
# コマンドを上書きしたい・別プロファイルを作りたい場合のみ書く。
# codex は一回きりのセットアップが必要 → /operations/codex-tool-setup.md
# opencode はアセット自動配置のみ（縮退に注意）→ /operations/opencode-tool-setup.md
[tools.claude-fast]
kind = "claude"                        # アダプタ種別: claude | codex | opencode
command = "claude --model haiku"       # 空白区切り: 先頭 = プログラム、残り = 基本引数
# plan_args = ["--permission-mode", "plan"]   # plan モード引数の上書き（claude 既定と同じ）

# ── リポジトリ ────────────────────────────────────────────
[[repositories]]
name = "totsuka"                       # 必須。ブランチ名・ログで使う安定 ID
path = "~/Workspace/github/tomoya-k31/totsuka"  # 必須。`~` / `${ENV}` 展開可。実在必須
summary = "AI 駆動の開発フロー自動化ツール。Rust ワークスペース。"  # LLM のリポジトリ選択材料
tool = "claude"                        # このリポジトリの既定 AI ツール（#196。省略時 default_tool → 組み込み claude）
max_concurrency = 2                    # このリポジトリの同時実行上限（省略時は無制限）
worktree_location = "~/work/wt/{repo_name}/{worktree_name}"  # [worktree].location をこのリポジトリだけ上書き

[[repositories]]
name = "dotfiles"
path = "~/Workspace/github/tomoya-k31/dotfiles"
summary = "zsh / mise / GNU Stow による dotfiles 管理。"
tool = "codex"                         # このリポジトリだけ Codex CLI で作業（組み込み codex、#196 Phase 2）

# ── プラグイン ────────────────────────────────────────────
[plugins.github]
enabled = true                # 省略時 false。false のプラグインを workflow から参照するとエラー
kind = "task_source"          # 必須: task_source | agent_ide | notifier
poll_interval_secs = 60       # task_source のみ。push 型ではプラグイン内部の取得周期になる
timeout_secs = 120            # RPC タイムアウト秒（省略時 120）
log_level = "info"            # プラグイン側のログレベル

[plugins.slack]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3           # この agent 経由の同時実行上限（省略時は無制限）
timeout_secs = 120

[plugins.notifier-macos]
enabled = true
kind = "notifier"

# ── LLM（AI Gateway）────────────────────────────────────────
# OpenAI 互換 /chat/completions 前提。repo_hint を持たないタスクのリポジトリ選択に使い、
# task_source プラグインへも initialize 時に既定として供給される。
# このテーブルだけは「省略」か「base_url + model を含む完全形」かの二択（部分指定不可）。
[llm]
base_url = "https://openrouter.ai/api/v1"      # 必須
model = "anthropic/claude-haiku-4-5"           # 必須
max_tokens = 256                               # 分類呼び出しの最大トークン（省略時 256）
timeout_secs = 30                              # リクエストタイムアウト（省略時 30）
api_key_ref = "op://Dev/Openrouter/api_key"    # シークレット参照（後述）

# ── worktree ──────────────────────────────────────────────
[worktree]
# 既定でよければこのキーは書かないこと。既定値は `$XDG_STATE_HOME/totsuka`（未設定時は
# `$HOME/.local/state/totsuka`）を解決済みのパスとして埋めた
# `<state dir>/worktrees/{repo_name}/{worktree_name}`。下は既定とは別の場所へ置く例。
# `${ENV}` は展開されるが未設定変数はエラーになる（`totsuka doctor` が検出する）ので、
# `${XDG_STATE_HOME}` を明示的に書くのは避ける — 既定と同じ場所になるうえ、
# `XDG_STATE_HOME` 未設定機（macOS の既定）では worktree 作成が失敗する。
location = "~/.worktrees/{repo_name}/{worktree_name}"
cleanup = "manual"          # implement モードの掃除: "immediate" | "manual" | { retention_days = 5 }
plan_cleanup = "immediate"  # plan モードの掃除（既定 immediate）

# ── ログ ──────────────────────────────────────────────────
[log]
level = "info"        # error | warn | info | debug | trace（`--debug` で debug に引き上げ）
log_prompts = true    # プロンプト/ペイロードを記録（実出力は debug 以上のときのみ）
max_files = 7         # 日次ログの保持世代数

# ── PR 出力テンプレート ─────────────────────────────────────
[output]
pr_title_template = "{title}"
pr_body_template = """
Automated by totsuka for task **{title}**.

Source: {url}

{summary}
"""

# ── Claude Code フック受信 ───────────────────────────────────
[hooks]
auth_token_ref = "keychain:totsuka/hook-token"                    # 運用上ほぼ必須（後述）
socket_path = "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock"     # 省略時は組み込み既定
spool_dir = "${XDG_STATE_HOME}/totsuka/hooks/spool"               # POST 失敗時の退避先
block_retry_limit = 3                                             # Stop フック差し戻しの連続上限

# ── ワークフロー ───────────────────────────────────────────
# 定義順に first-match。最初にマッチした 1 件だけが実行される。
[[workflows]]
name = "design"
source = "github"                            # 必須。enabled な task_source 名
trigger = { project_status = "設計待ち" }     # 省略すると全タスクにマッチ
mode = "plan"                                # plan | implement
agent = "herdr"                              # 必須。enabled な agent_ide 名
output = "source"                            # pull_request | source | none
on_success = { set_status = "設計レビュー待ち" }
on_failure = { set_status = "設計失敗" }
verification = "llm"                         # llm | human | none（省略時 llm）
rubric = "設計方針・影響範囲・代替案の比較が明示されていること"
timeout_secs = 1800                          # 無応答上限秒。超過でエスカレーション
tool = "claude"                              # AI ツールの明示ピン（#196。llm 検収は claude 必須のため静的に保証。省略時 repo → default_tool）

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
verification = "llm"
rubric = "テストが追加されており、cargo clippy / cargo fmt が通っていること"
```

# 選択肢の選び方

## シークレット参照 — 3 方式

設定ファイルに生のトークンを書かない。文字列値は次の 3 形式を取れる（`config.toml` / `plugins/*.toml` の**任意の文字列 leaf** で使える）。

| 形式 | 例 | 選ぶ基準 |
|---|---|---|
| `op://<vault>/<item>/<field>` | `op://Dev/Openrouter/api_key` | **推奨。**cross-platform で、非 macOS でも動く唯一の実働バックエンド。1Password CLI へのシェルアウトなので事前に `op signin` 済みであること |
| `keychain:<service>/<account>` | `keychain:totsuka/hook-token` | macOS 専用。1Password を使っていない環境向け。`security add-generic-password` で登録済みであること |
| `${ENV_VAR}` を含む文字列 | `${GITHUB_TOKEN}` | CI・使い捨て環境向け。**未設定だと起動時エラー**（既定値へのフォールバックはしない）。永続運用には非推奨 |

パス値（`path`、`socket_path`、`spool_dir`、`location`）では加えて `~` が展開される。
`totsuka config show --redacted` はキー名に `token` / `key` / `secret` / `password` / `credential` を含む値を `***redacted***` に伏せて表示する。

詳細は [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)。

## `[plugins.{name}].kind` — task_source / agent_ide / notifier

| 値 | 役割 | 現在の実装 |
|---|---|---|
| `task_source` | タスクの入口。指示を検知して Orchestrator へ push する | `github` / `notion` / `slack` |
| `agent_ide` | 実際にコードを書く AI エージェントを駆動する | `herdr` / `orca` |
| `notifier` | 人間への通知（検収待ち・失敗・エスカレーション） | `notifier-macos` |

**注意**: `enabled` は省略すると `false`。`[plugins.x]` を書いて `kind` だけ設定した状態は「文法的には妥当だが動作しない」。
その状態のプラグインを workflow の `source` / `agent` から参照すると、警告ではなく**バリデーションエラー**になる。

## `[[workflows]].mode` — plan / implement

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `plan` | worktree を作って設計させるが、**push / PR 作成は禁止** | 設計レビューを人間が挟みたい。実装前に方針を固めたい |
| `implement` | 実装してコミットまで行う | タスクが十分に具体化されている |

`mode = "plan"` と `output = "pull_request"` の**組み合わせはバリデーションエラー**（plan は push しないため PR を作れない）。
plan の結果を人に見せたいなら `output = "source"`（ソース側に書き戻す）を使う。

## `[[workflows]].output` — pull_request / source / none

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `pull_request` | ブランチを push して PR を作る | 通常の実装タスク |
| `source` | 結果をタスクソース側へ書き戻す（GitHub Issue コメント、Slack スレッド返信など） | 設計案の提示、Slack での応答。**プラグインが `source` 出力 capability を宣言していないとエラー** |
| `none` | 何も出力しない（worktree に成果物が残るだけ） | 手元で結果を確認してから自分で処理したい |

## `[[workflows]].verification` — llm / human / none

エージェントの「完了しました」という自己申告を、どう検収するか。

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `llm`（既定） | Stop フックで LLM が in-session 検収し、不十分なら差し戻す。`rubric` に判定基準を書ける | **既定でよい。**基準を文章で書けるタスク全般 |
| `human` | `Verifying` 状態で止まり、`totsuka task verify` を待つ | 影響が大きく人の目を通したい。有効な `notifier` が無いと警告が出る（通知されず気づけないため） |
| `none` | 自己申告をそのまま受け入れる | 検収コストが見合わない軽微なタスク、デバッグ時 |

`rubric` は `verification = "llm"` のときのみ意味を持つ。他の値と併用すると警告になる。
差し戻しが `block_retry_limit`（既定 3）回連続すると、無限ループを避けてエスカレーションする。

## `[[workflows]].trigger` — マッチ条件

省略または `{}` は**全タスクにマッチ**する。定義順の first-match なので、広いトリガーは後ろに置く。

| キー | 型 | 意味 |
|---|---|---|
| `status` / `project_status` | string | タスクのステータスと一致比較（両者は同じ次元） |
| `label` | string | 単一ラベルの存在 |
| `labels` | 配列 | **すべて**含むこと（AND） |
| その他のキー | 任意 | Orchestrator は解釈せず、`initialize` の `triggers` としてプラグインへ渡す |

上記の予約キーは、プラグインの絞り込みを信用せず Orchestrator 側でも防御的に再判定する。
同一ソース内でトリガーが重なりうる 2 つの workflow を定義すると警告が出る（先勝ちで後者が死ぬため）。

## `[worktree].cleanup` / `plan_cleanup` — 掃除ポリシー

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `"immediate"` | 完了後すぐ削除 | `plan` モード（既定）。成果物がソース側に書き戻され worktree に用が無い |
| `"manual"` | 削除しない | `implement` モード（既定）。PR レビューで手元を確認する可能性がある |
| `{ retention_days = 5 }` | N 日経過後に削除 | 手元確認はしたいがディスクも溜めたくない中間解 |

いずれの場合も**未コミット変更のある worktree は決して削除されない**ので、作業中のものを失う心配はない。

`location` テンプレートで使えるプレースホルダは `{repo}` / `{repo_name}` / `{worktree_name}` / `{task_id}` / `{source}` のみ。`{worktree_name}` は `{source}-{task_id}` を git ref 規則で正規化し `/` を潰したもの（Slack なら `slack-C0ABCDEF12-1720000000.123456`）。**`{branch}` は廃止された** — worktree を作る時点でブランチ名はまだ存在しない（エージェントがリポジトリの規約に従って後から決める）ため、ディレクトリ名には使えない。残っていると専用のエラーで起動を止める。
それ以外を書くとバリデーションエラーになる（`${ENV}` と `~` はプレースホルダとは別枠で展開される）。
ブランチ名テンプレートは設定不可で、`agent/{source}-{task_id}` 固定。

## 並列上限の 3 階層

3 つの `max_concurrency` はすべて同時に効く（最も厳しいものが実効値）。

| 階層 | キー | 既定 | 用途 |
|---|---|---|---|
| 全体 | トップレベル `max_concurrency` | 4 | マシン全体の負荷上限 |
| リポジトリ | `[[repositories]].max_concurrency` | 無制限 | 同一リポジトリでの worktree 乱立・コンフリクト抑制 |
| エージェント | `[plugins.{name}].max_concurrency` | 無制限 | `agent_ide` のみ有効。API レート・ライセンス数の制約 |

## `[hooks].auth_token_ref` — 設定すべきか

**すべき。**未設定でもフック POST は受理されるが、その場合の防御は 0600 の UDS パーミッションのみになる。

未設定はツール側が検出する（#209）。判定材料は agent プラグインのマニフェストで、`resume_session` または
`diagnostics_snapshot` を宣言していれば「フック対応 agent」とみなす（herdr が該当。orca / mock は非該当）:

- `config validate` / `totsuka run` — フック対応 agent を使う workflow ごとに**警告**（終了コードは変わらない。`run` は表示して続行）
- `totsuka doctor` — 同じ条件で **fail**（終了コード非 0）。フック対応 agent を使わない構成では warn 表示のみで成功のまま。
  参照を設定したのに解決できない場合は構成によらず fail

```bash
# 例: ランダムトークンを Keychain に入れて参照する
security add-generic-password -s totsuka -a hook-token -w "$(openssl rand -hex 32)"
```

# Part 2: plugins/{name}.toml（主要 3 プラグイン）

`notion` / `orca` / `notifier-macos` の全キーは各コンポーネントページ（[task-source-notion](/components/task-source-notion.md) /
[agent-ide-orca](/components/agent-ide-orca.md) / [notifier-macos](/components/notifier-macos.md)）を参照。

## `plugins/github.toml`（task-source-github）

GitHub Projects (v2) のステータス列をタスクの入口にする。

```toml
token = "op://Dev/GitHub/totsuka_pat"   # 必須。Projects 読み書き権限のある PAT
owner = "tomoya-k31"                    # 必須。ユーザー名または組織名
owner_type = "user"                     # "user"（既定）| "organization"
project_number = 3                      # 必須。Project の URL 末尾の数字
github_login = "tomoya-k31"             # 必須。担当者がこのログイン名のカードだけ拾う（大小無視）
status_field = "Status"                 # ステータス列の名前（既定 "Status"）

# すでに着手中とみなすステータス。ここに入っているカードは再度タスク化されない
in_progress_statuses = ["実装中", "設計中"]

# Orchestrator 内部のステータス名 ← → Project 上の表示名の対応（未定義キーは素通し）
[status_map]
"レビュー待ち" = "In Review"

# 対象リポジトリの絞り込み。省略すると Project 内の全リポジトリが対象
repos = ["totsuka", "dotfiles"]

source_name = "github"                          # Task.source に刻まれる名前（既定 "github"）
api_url = "https://api.github.com/graphql"      # GHES 利用時に上書き
max_retries = 3                                 # リトライ可能な API 失敗の再試行回数
```

## `plugins/slack.toml`（task-source-slack）

Socket Mode で自分宛メンションを受け、本人名義で返信する。導入手順は
[Slack セットアップ Quickstart](/operations/slack-quickstart.md)、トークンの扱いは
[取り扱いポリシー](/security/slack-user-token.md) を参照。

```toml
app_token = "op://Dev/Slack/app_token"     # 必須。App-Level Token（xapp- で始まる）
user_token = "op://Dev/Slack/user_token"   # 必須。User OAuth Token（xoxp- で始まる）
target_user_id = "U01ABCDEF"               # 必須。自分の Slack ユーザー ID

# 自分が付けるとタスクを起こす絵文字（#319）。既定は [] = 無効。
# 自分宛でないメッセージを、会話にノイズを足さずタスク化できる。
# 他人が同じ絵文字を付けても起動しない（緩和する設定は無い）。
# reactions:read スコープが必要 → 追加後はアプリ再インストール + Keychain 2 本更新。
trigger_reactions = ["eyes"]

thread_context_limit = 6                   # タスク本文に含めるスレッド直近メッセージ数（既定 6）
reply_style = "丁寧語で簡潔に。箇条書きを多用しない。"   # 返信トーンの指示
source_name = "slack"
api_url = "https://slack.com/api"
max_retries = 3

# チャンネル名の prefix ごとに候補リポジトリを絞る（定義順 first-match）
[[channel_groups]]
prefix = "proj-totsuka"
repos = ["totsuka"]
```

**省略できるものは省略するのが正解**な 2 つのテーブル:

- `[[repos]]` — 省略すると `config.toml` の `[[repositories]]`（name / summary / path）がそのまま候補になる。
  候補を絞りたい、または Slack 文脈用に別の `summary` を与えたいときだけ書く。
- `[llm]` — 省略すると `config.toml` の `[llm]` が `initialize` 経由で供給される。
  書く場合はキー名が `api_key_ref` ではなく **`api_key`** である点に注意（`base_url` / `model` / `api_key` が必須、
  `confidence_threshold` は既定 0.6）。両方に無い場合、**リポジトリ候補が 2 件以上あるときだけ** `initialize` が
  `CONFIG_INVALID` で失敗する（候補が 0 件か 1 件なら分類の必要が無いので起動する）。

`poll_interval_secs` は使わない。Socket Mode のイベント駆動で即 push するため（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。

## `plugins/herdr.toml`（agent-ide-herdr）

```toml
# ソケットの決定順: socket_path > session > $HERDR_SOCKET_PATH > $HERDR_SESSION
#                  > $XDG_CONFIG_HOME/herdr/herdr.sock
#                    （XDG_CONFIG_HOME 未設定時のみ ~/.config/herdr/herdr.sock）
# 通常はどちらも書かず、既定パスに任せてよい
# socket_path = "${XDG_CONFIG_HOME}/herdr/herdr.sock"
# session = "main"

agent_command = "claude"                        # 起動するエージェント（空白区切りで引数も可）
plan_args = ["--permission-mode", "plan"]       # plan モード時に追加する引数
design_preview = "side_pane"                    # 設計プレビューの表示方法
request_timeout_secs = 30                       # herdr への RPC タイムアウト
```

# Part 3: シナリオ別レシピ

## 1. 最小構成 — GitHub Projects + herdr

動く最小限。`[llm]` すら省略できる（省略した場合、`repo_hint` を持たないタスクは
リポジトリを決められず `pending` になる。リポジトリが 1 つなら実害はない）。

```toml
[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
```

## 2. 設計 → 実装ハンドオフ

人間のレビューを 1 回挟む 2 段構え。設計は書き戻しだけ、実装は PR。
`design` が先に定義されているので、`設計待ち` のカードは `design` に、`実装待ち` のカードは `implement` にマッチする。

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"                                   # push しない
agent = "herdr"
output = "source"                               # 設計案を Issue へ書き戻す
on_success = { set_status = "設計レビュー待ち" }  # 人間のレビュー待ちへ

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }        # 人がレビュー後に手で移す
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
```

## 3. Slack 起点で本人名義返信

Slack のメンションに、調査した上で自分の名義で返信する。コードは書かないので `output = "source"`。

```toml
# config.toml
[plugins.slack]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[plugins.notifier-macos]
enabled = true
kind = "notifier"

[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4-5"
api_key_ref = "op://Dev/Openrouter/api_key"

[hooks]
auth_token_ref = "keychain:totsuka/hook-token"

[[workflows]]
name = "slack-reply"
source = "slack"
mode = "implement"        # 調査のためにコードを読ませる。push は output で抑止
agent = "herdr"
output = "source"         # Slack スレッドへ返信
verification = "llm"
rubric = "質問に直接答えており、根拠となるファイルパスや行が示されていること"
```

## 4. 人間検収を必須にする（高影響タスク）

`verification = "human"` は通知が届かないと気づけないため、`notifier` の有効化とセットで使う。

```toml
[plugins.notifier-macos]
enabled = true
kind = "notifier"

[[workflows]]
name = "migration"
source = "github"
trigger = { labels = ["migration", "high-risk"] }   # 両方のラベルが必要（AND）
mode = "implement"
agent = "herdr"
output = "pull_request"
verification = "human"      # totsuka task verify を待って止まる
timeout_secs = 3600
```

## 5. 複数リポジトリ + 並列制御

リポジトリごとに worktree の乱立を抑えつつ、全体の負荷も抑える。

```toml
max_concurrency = 6                 # マシン全体

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
summary = "AI 駆動の開発フロー自動化ツール。Rust ワークスペース。"
max_concurrency = 2                 # 同一リポジトリでのコンフリクトを抑える

[[repositories]]
name = "dotfiles"
path = "~/Workspace/github/tomoya-k31/dotfiles"
summary = "zsh / mise / GNU Stow による dotfiles 管理。"
tool = "codex"                         # このリポジトリだけ Codex CLI で作業（組み込み codex、#196 Phase 2）
max_concurrency = 1

[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3                 # エージェント側のライセンス・レート制約

[worktree]
cleanup = { retention_days = 3 }    # 3 日は手元で確認できるようにしておく
```

# 検証

```bash
totsuka config validate            # 静的検証 + 各プラグインへの config/validate 問い合わせ
totsuka config validate --offline  # プラグインを起動せず静的検証のみ
totsuka config show --redacted     # 解決後の設定をシークレット伏字で表示
totsuka doctor                     # 依存コマンド・ソケット・シークレットバックエンドの疎通確認
```

`config validate` が **エラー**を返す代表例:

| 症状 | 原因 |
|---|---|
| `unknown field` | キーの typo（`deny_unknown_fields`） |
| プラグイン参照エラー | workflow の `source` / `agent` が未定義、`enabled = false`、または `kind` 違い |
| `mode = plan` × `output = pull_request` | plan は push しないので PR を作れない |
| リポジトリパス不在 | `path` の展開結果がディスク上に存在しない |
| プレースホルダエラー | `worktree_location` に `{repo}` / `{repo_name}` / `{worktree_name}` / `{task_id}` / `{source}` 以外を使った（`{branch}` は廃止済みで専用のエラーになる） |
| 環境変数未設定 | `${VAR}` 参照先が export されていない |

**警告**（実行は止まらない）の代表例: トリガーの重複、`verification = "human"` なのに notifier が無い、
`verification != "llm"` なのに `rubric` がある、
フック対応 agent を使う workflow があるのに `[hooks].auth_token_ref` が未設定（最後の 1 つは `doctor` では fail 扱い。前述）。

`totsuka run` はエラーがあると起動を中止し、警告は表示した上で続行する。
