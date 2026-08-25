---
type: Guide
title: 設定例集（config.toml）
description: そのまま貼って動く config.toml の完全版注釈付き例と、選択肢を持つキー（kind・mode・output・verification・cleanup・trigger・シークレット参照・並列上限）の選び分け基準、TOTSUKA_* 環境変数オーバーライドの対応表、および最小構成／GitHub Projects／Slack／設計→実装ハンドオフのシナリオ別レシピ。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/init_cmd.rs
tags: [config, toml, examples, recipes, workflow, secrets, slack, github, herdr, environment]
generated: { by: claude-code/opus-5, at: 2026-08-25T21:00:00+09:00 }
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

**ファイルは 1 本だけ** `$XDG_CONFIG_HOME/totsuka/config.toml`（既定 `~/.config/totsuka/config.toml`）。`--config <path>` で場所を上書きできる。

| テーブル | 役割 |
|---|---|
| `version` / `[[repositories]]` / `[[projects]]` / `[[workflows]]` / `[llm]` / `[worktree]` / `[log]` / `[hooks]` / `[tools.*]` | Orchestrator 本体の設定 |
| `[plugins.<name>]` | プラグインのロスター（`enabled` / `kind` / 共通項目）。Orchestrator が解釈する |
| `[<name>]` | プラグイン個別設定。Orchestrator は無解釈で保持し、シークレット解決後に `initialize` で渡す |

`<name>` は `[plugins.<name>]` のインスタンス名（= プラグインのバイナリ名）と一致させる。**ロスターに無い名前のトップレベルテーブルは検証エラー**になる（#554）。

`plugins/{name}.toml` への分離は **#554 で廃止**した（[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）。残っていても読まれないので、移行時に消すこと。

優先順位（上が強い）:

1. CLI フラグ（`--config`、`--debug`）
2. 環境変数 `TOTSUKA_*`（[対応表は後述](#環境変数によるオーバーライド)）
3. `[<name>]`（プラグイン個別設定）
4. `config.toml` の Orchestrator 側の既定

設定テーブルはいずれも `deny_unknown_fields`。**キーを 1 文字打ち間違えると既定値へのフォールバックではなくパースエラーになる**（これは意図的な設計で、typo が黙って無視される事故を防ぐ）。

**トップレベルと `[[workflows]]` だけは serde の外で検査する**（#554）。プラグインが自分のキーを足せる場所なので serde には判断できず、代わりに:

- トップレベルの未知テーブルは `[plugins.*]` のロスターと照合する（`[worktre]` も `[slak]` も落ちる）
- `[[workflows]]` の余ったキーはその workflow の `source` と `agent` に聞き、**ちょうど 1 つ**が引き取ることを要求する（0 = タイポ、2 = 曖昧。どちらも起動を止める）

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
環境変数名で一意に指し示せないため対象外。`[<name>]` の中身も Orchestrator が解釈しない
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
kind = "claude"                             # アダプタ種別: claude | codex | opencode
command = "claude --model haiku --effort low"  # 空白区切り: 先頭 = プログラム、残り = 基本引数
                                            # モデル / 推論強度に専用キーは無く、ここに書く
# plan_args = ["--permission-mode", "plan"]   # plan モード引数の上書き（claude 既定と同じ）

# ── リポジトリ ────────────────────────────────────────────
[[repositories]]
name = "totsuka"                       # 必須。ブランチ名・ログで使う安定 ID
path = "~/Workspace/github/tomoya-k31/totsuka"  # 必須。`~` / `${ENV}` 展開可。実在必須
summary = "AI 駆動の開発フロー自動化ツール。Rust ワークスペース。"  # LLM のリポジトリ選択材料
tool = "claude"                        # このリポジトリの既定 AI ツール（#196。省略時 default_tool → 組み込み claude）
max_concurrency = 2                    # このリポジトリの同時実行上限（省略時は無制限）
worktree_location = "~/work/wt/{repo_name}/{worktree_name}"  # [worktree].location をこのリポジトリだけ上書き
project = "tomo-prj"                   # 起票先トラッカー（[[projects]].name、#554）

[[repositories]]
name = "dotfiles"
path = "~/Workspace/github/tomoya-k31/dotfiles"
summary = "zsh / mise / GNU Stow による dotfiles 管理。"
tool = "codex"                         # このリポジトリだけ Codex CLI で作業（組み込み codex、#196 Phase 2）
project = "tomo-prj"                   # 起票先トラッカー（[[projects]].name、#554）。無ければトラッカー無し

# ── トラッカー（#554）──────────────────────────────────────
# リポジトリの起票先。`name` と `source` は Orchestrator が読み、
# 残りのキーはその task_source プラグインのもの（無解釈で渡る）。
[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"
owner_type = "user"
project_number = 6
triage_status = "📥 Inbox"             # triage 起票時に付ける Status（省略 = Status なし）

# ── プラグイン ────────────────────────────────────────────
[plugins.github]
enabled = true                # 省略時 false。false のプラグインを workflow から参照するとエラー
kind = "task_source"          # 必須: task_source | agent_ide | notifier
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

[plugins.macos]
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

# ── Claude Code フック受信 ───────────────────────────────────
[hooks]
auth_token_ref = "op://Dev/totsuka/hook-token"                    # 運用上ほぼ必須（後述）
socket_path = "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock"     # 省略時は組み込み既定
spool_dir = "${XDG_STATE_HOME}/totsuka/hooks/spool"               # POST 失敗時の退避先
block_retry_limit = 3                                             # Stop フック差し戻しの連続上限

# ── ワークフロー ───────────────────────────────────────────
# 定義順に first-match。最初にマッチした 1 件だけが実行される。
# mode / output / verification は明示するか、profile（#394）でまとめて決めるかの二択。
[[workflows]]
name = "design"
source = "github"                            # 必須。enabled な task_source 名
trigger = { project_status = "設計待ち" }     # 省略すると全タスクにマッチ
mode = "plan"                                # plan | implement
agent = "herdr"                              # 必須。enabled な agent_ide 名
output = "none"                              # source | none。github は publish しない（#398）
on_success = { set_status = "設計レビュー待ち" }
on_failure = { set_status = "設計失敗" }
verification = "llm"                         # llm | human | none（省略時 llm）
rubric = "設計方針・影響範囲・代替案の比較が明示されていること"
timeout_secs = 1800                          # 無応答上限秒。超過でエスカレーション
tool = "claude"                              # AI ツールの明示ピン（#196。llm 検収は claude 必須のため静的に保証。省略時 repo → default_tool）

# 同じことを profile で書いた版。mode / verification は profile が決めるので書かない
# （書くとエラー）。output だけは上書きしてよいが、ここでは profile の既定
# （implement → none）がそのまま正しいので書かない。
[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
profile = "implement"                        # answer | triage | design | implement
agent = "herdr"
on_success = { set_status = "レビュー待ち" }
rubric = "テストが追加されており、cargo clippy / cargo fmt が通っていること"

# ワークフローごとの前置き指示（#415）。可視・タスク本文の前・新規会話のときだけ。
# リテラル（テンプレート展開なし）なので `{` もそのまま書ける。
# 人間へ問いかけるツールを使わせる指示は無人 pane でハングする → 運用者の責任。
[[workflows]]
name = "github-design"
source = "github"
trigger = { project_status = "Design" }
profile = "design"                           # 完了は人間の pane 上承認（#440）
agent = "herdr"
on_success = { set_status = "Design Review" }
timeout_secs = 0                             # attended pane: D-03 掃引を無効化（#439）
initial_prompt = "/grill-me スキルを使用して、詳細設計を行ってください"
```

# 選択肢の選び方

## シークレット参照 — 4 方式

設定ファイルに生のトークンを書かない。文字列値は次の 4 形式を取れる（`config.toml` の**任意の文字列 leaf** で使える — プラグインの `[<name>]` テーブルの中も含む）。

| 形式 | 例 | 選ぶ基準 |
|---|---|---|
| `op://<vault>/<item>/<field>` | `op://Dev/Openrouter/api_key` | **長命の秘密の推奨。**cross-platform で、非 macOS でも動く唯一のシークレットストア（`keychain:` は macOS 専用）。1Password CLI へのシェルアウトなので事前に `op signin` 済みであること |
| `cmd:<command>` | `cmd:gh auth token` | **別ツールが管理・ローテートする credential の推奨**（#444）。解決のたびにコマンドを実行して stdout を使うので、コピーの陳腐化が起きない。ローテートする token を op/keychain に写すとコピーが黙って死ぬ — その罠がこの形式の起点 |
| `keychain:<service>/<account>` | `keychain:totsuka/hook-token` | macOS 専用。1Password を使っていない環境向け。`security add-generic-password` で登録済みであること |
| `${ENV_VAR}` を含む文字列 | `${GITHUB_TOKEN}` | CI・使い捨て環境向け。**未設定だと起動時エラー**（既定値へのフォールバックはしない）。永続運用には非推奨 |

パス値（`path`、`socket_path`、`spool_dir`、`location`）では加えて `~` が展開される。
`totsuka config show --redacted` はキー名に `token` / `key` / `secret` / `password` / `credential` を含む値を `***redacted***` に伏せて表示する。

詳細は [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)（op://）と [ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)（cmd:）。

## `[plugins.{name}].kind` — task_source / agent_ide / notifier

| 値 | 役割 | 現在の実装 |
|---|---|---|
| `task_source` | タスクの入口。指示を検知して Orchestrator へ push する | `github` / `notion` / `slack` |
| `agent_ide` | 実際にコードを書く AI エージェントを駆動する | `herdr` / `orca` |
| `notifier` | 人間への通知（検収待ち・失敗・エスカレーション） | `notifier-macos` |

**注意**: `enabled` は省略すると `false`。`[plugins.x]` を書いて `kind` だけ設定した状態は「文法的には妥当だが動作しない」。
その状態のプラグインを workflow の `source` / `agent` から参照すると、警告ではなく**バリデーションエラー**になる。

## `[[workflows]].profile` — 4 原型でまとめて決める（#394）

`mode` / `output` / `verification` を個別に選ぶ代わりに、噛み合う組み合わせに付けた名前を 1 つ選ぶ。
組み合わせを人間が合わせる構造が事故の発生源だったため導入した（[ADR-0033](/decisions/adr-0033-workflow-profile.md)）。

| profile | mode | output | verification | 選ぶ場面 |
|---|---|---|---|---|
| `answer` | plan | source | llm | 質問に答えてソースへ返す（Slack メンション・リアクション） |
| `triage` | plan | source | llm | 依頼を GitHub / Notion へ起票する |
| `design` | plan | none | llm | 詳細設計を issue コメント / ページへ書き、status で伝える。**完了は人間が pane 上で承認**（#440） |
| `implement` | implement | none | llm | 実装して PR を出す。**完了は人間が pane 上で承認**（#440） |

```toml
[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"                           # mode / verification は書かない（書くとエラー）
agent = "herdr"
on_success = { set_status = "設計済み" }

[[workflows]]
name = "slack-implement"
source = "slack"
profile = "implement"
output = "source"                            # output だけは profile を上書きできる
agent = "herdr"
```

- `mode` / `verification` との併用は**エラー**。どちらを勝たせても、負けた側が「生きて見える死んだ設定」として残るため
- `output` との併用だけは**可**で、`output` が勝つ。権限ではなく配線先の選択なので、上書きしても安全性は変わらない。Slack 起点の implement が PR URL をスレッドへ返すのに要る
- `profile` を書かないなら `mode` と `output` は従来どおり**必須**
- 4 原型で表せない組み合わせ（`verification = "human"` など）は明示記法で書く。明示記法は非推奨ではない
- **旧バージョンへ戻すときは config も戻すこと** — `profile` は旧バイナリでは未知キーとしてパースエラーになる

## `[[workflows]].mode` — plan / implement

`profile` を使わない場合に書く（使う場合は profile が決めるので書けない）。

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `plan` | worktree を作って設計させる。ペインが git を実行できないので、ブランチ作成もコミットも push も起きない | 設計レビューを人間が挟みたい。実装前に方針を固めたい |
| `implement` | 実装してコミットまで行う | タスクが十分に具体化されている |

plan の結果を人に見せたいなら `output = "source"`（ソース側に書き戻す）を使う。

## `[[workflows]].output` — source / none

| 値 | 挙動 | 選ぶ場面 |
|---|---|---|
| `source` | 結果をタスクソース側へ書き戻す（GitHub Issue コメント、Slack スレッド返信など） | 設計案の提示、Slack での応答、実装タスクの報告。**プラグインが `source` 出力 capability を宣言していないとエラー** |
| `none` | 何も出力しない（worktree に成果物が残るだけ） | 手元で結果を確認してから自分で処理したい |

**`pull_request` は廃止された。** push と PR 作成はエージェントの責務になり
（F-86、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）、Orchestrator は
push しない。残っていると起動時に `unknown variant` で落ちるので `source` に変更し、
PR 作成手順はリポジトリの規約（CLAUDE.md / CONTRIBUTING.md など）に書く。
PR の URL を Slack 返信に載せたい場合は、エージェントの最終メッセージに含めさせる
（`[slack]` の `[prompts].reply_instructions`）。

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

省略または `{}` は**全タスクにマッチ**する。定義順の first-match だが、**その判定を走らせるのはソースプラグインである**（#554）。Orchestrator は `trigger` の中身を一切解釈せず、`initialize` でプラグインへ渡すだけになった。

### 絵文字でワークフローを選ぶ（#396）

Slack のリアクションでどのワークフローを起動するかを、config.toml 側だけで決める。

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }   # 🔨 を自分で付けたら実装させる
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"                # メンション。catch-all なので必ず最後
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

| やりがちな間違い | どうなるか |
|---|---|
| 同じ絵文字を 2 つの workflow に書く | `CONFIG_INVALID` |
| リアクションを持たない workflow（= メンション）を 2 つ書く | `CONFIG_INVALID`（#554） |

本人限定の不変条件（他人のリアクションでは起動しない）は緩和できない。

**「catch-all より前に書け」の制約は #554 で消えた。** Slack プラグイン内ではメンションとリアクションが別のイベント経路なので、順序で隠れることがない。

trigger のキーはすべて**プラグインが解釈する**。`status` / `project_status` / `label` / `labels` / `reaction` が Orchestrator の予約語だったのは #554 まで —— `reaction` は Slack の語、`project_status` は GitHub Projects の語で、core の語彙に置く理由が無かった。どのキーが効くかは各プラグインのページを参照:

| ソース | 効くキー |
|---|---|
| github | `project_status` / `status`、`label` / `labels` |
| notion | `status`、生の `filter` |
| slack | `reaction`（無ければメンション） |

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

未設定はツール側が検出する（#209）。判定材料は agent プラグインのマニフェストで、`hook_completion` を宣言していれば
「フック対応 agent」とみなす（herdr が該当。orca / mock は非該当）。**0.5.0 より前は
`resume_session || diagnostics_snapshot` という de-facto の OR だった**が、
`diagnostics_snapshot` は `diagnostics/snapshot` に応答できることしか言っておらず、
フック対応を含意しない（[ADR-0052](/decisions/adr-0052-declaration-consumed.md)）:

- `config validate` / `totsuka run` — フック対応 agent を使う workflow ごとに**警告**（終了コードは変わらない。`run` は表示して続行）
- `totsuka doctor` — 同じ条件で **fail**（終了コード非 0）。フック対応 agent を使わない構成では warn 表示のみで成功のまま。
  参照を設定したのに解決できない場合は構成によらず fail

```bash
# 例: ランダムトークンを生成して保管する（macOS Keychain の場合。1Password なら item を作る）
security add-generic-password -s totsuka -a hook-token -w "$(openssl rand -hex 32)"
```

# Part 2: `[<name>]`（主要 3 プラグイン）

`notion` / `orca` / `notifier-macos` の全キーは各コンポーネントページ（[task-source-notion](/components/task-source-notion.md) /
[agent-ide-orca](/components/agent-ide-orca.md) / [notifier-macos](/components/notifier-macos.md)）を参照。

## `[github]`（task-source-github）

GitHub Projects (v2) のステータス列をタスクの入口にする。

**ボードはここに書かない。** Orchestrator のトップレベル `[[projects]]`（`source = "github"`）に書き、リポジトリは `[[repositories]].project` で紐づける（#554）。

```toml
[github]
token = "op://Dev/GitHub/totsuka_pat"   # 必須。Projects 読み書き権限のある PAT
github_login = "tomoya-k31"             # 必須。担当者がこのログイン名のカードだけ拾う（大小無視）
status_field = "Status"                 # ステータス列の名前（既定 "Status"）。全ボード共通

# すでに着手中とみなすステータス。ここに入っているカードは再度タスク化されない
in_progress_statuses = ["実装中", "設計中"]

source_name = "github"                          # Task.source に刻まれる名前（既定 "github"）
api_url = "https://api.github.com/graphql"      # GHES 利用時に上書き
max_retries = 3                                 # リトライ可能な API 失敗の再試行回数

# Orchestrator 内部のステータス名 ← → Project 上の表示名の対応（未定義キーは素通し）
[github.status_map]
"レビュー待ち" = "In Review"

# ボードは Orchestrator 側。複数書ける（#542 / #554）
[[projects]]
name = "tomo-prj"                       # 必須。[[repositories]].project が指す名前
source = "github"                       # 必須。このボードを所有するプラグイン
owner = "tomoya-k31"                    # 必須。ユーザー名または組織名
owner_type = "user"                     # "user"（既定）| "organization"
project_number = 3                      # 必須。Project の URL 末尾の数字
triage_status = "📥 Inbox"              # 任意。triage 起票した item に付ける Status（未設定 = Status なし）

[[projects]]
name = "web-board"
source = "github"
owner = "my-org"
owner_type = "organization"
project_number = 7

# 紐付けはリポジトリ側に 1 つだけ書く
[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"

[[repositories]]
name = "web-app"
path = "~/Workspace/github/my-org/web-app"
project = "web-board"
```

この紐付けは取り込みフィルタ（そのボードに載っていても、紐づかないリポジトリの issue は取り込まない）と、リポジトリ → ボードの対応表（`initialize` の応答に載り、Slack 発の triage が起票先を引く材料になる）を兼ねる。**役割は 2 つだが正本は 1 箇所**で、#554 より前はプラグイン側の `repos = [...]` に書いていた。

`project` はスカラー 1 つなので、**同じリポジトリを 2 つのボードに紐づけることはできない** —— 以前は `config/validate` が検出する対象だった。

## `[slack]`（task-source-slack）

Socket Mode で自分宛メンションを受け、本人名義で返信する。導入手順は
[Slack セットアップ Quickstart](/operations/slack-quickstart.md)、トークンの扱いは
[取り扱いポリシー](/security/slack-user-token.md) を参照。

```toml
[slack]
app_token = "op://Dev/Slack/app_token"     # 必須。App-Level Token（xapp- で始まる）
user_token = "op://Dev/Slack/user_token"   # 必須。User OAuth Token（xoxp- で始まる）
target_user_id = "U01ABCDEF"               # 必須。自分の Slack ユーザー ID

# リアクション起動は config.toml の [[workflows]].trigger.reaction で設定する
# （#396。下の「絵文字でワークフローを選ぶ」参照）。reactions:read スコープが
# 必要 → 追加後はアプリ再インストール + 保管先の値を 2 本更新。

thread_context_limit = 6                   # タスク本文に含めるスレッド直近メッセージ数（既定 6）
reply_style = "丁寧語で簡潔に。箇条書きを多用しない。"   # 返信トーンの指示
source_name = "slack"
api_url = "https://slack.com/api"
max_retries = 3

# チャンネル名の prefix ごとに候補リポジトリを絞る（定義順 first-match）
[[slack.channel_groups]]
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

## `[herdr]`（agent-ide-herdr）

```toml
[herdr]
# ソケットの決定順: socket_path > session > $HERDR_SOCKET_PATH > $HERDR_SESSION
#                  > $XDG_CONFIG_HOME/herdr/herdr.sock
#                    （XDG_CONFIG_HOME 未設定時のみ ~/.config/herdr/herdr.sock）
# 通常はどちらも書かず、既定パスに任せてよい
# socket_path = "${XDG_CONFIG_HOME}/herdr/herdr.sock"
# session = "main"

request_timeout_secs = 30                       # herdr への RPC タイムアウト

# dispatch した pane の配置（省略可。以下が既定値）
[herdr.layout]
shell     = true                                # 併設シェル pane を出すか（false ならエージェント全画面）
direction = "down"                              # "down" = 上下 / "right" = 左右（herdr の SplitDirection）
ratio     = 0.8                                 # エージェント側の取り分
```

- **`agent_command` / `plan_args` / `design_preview` はプロトコル 0.4.0 で削除された**（#411、
  [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。まだ書いてあると `initialize` が
  `CONFIG_INVALID` で落ちる（キー名と代替を挙げたメッセージが出る）ので消すこと。argv は
  `config.toml` の `[tools]` が決め（[ADR-0014](/decisions/adr-0014-tool-abstraction.md)）、
  pane の配置は `[herdr.layout]` が決める（[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)）。
- `[herdr.layout]` の `ratio` は**エージェント側**の取り分。範囲検査はせず herdr へそのまま送る（不正値は herdr が拒否し、
  その場合は警告のうえシェル pane なしで続行する）。`direction` は `down` / `right` のみで、
  他の値は `initialize` の時点でエラーになる。`shell = false` のとき `direction` / `ratio` は無視される。

# Part 3: シナリオ別レシピ

## 1. 最小構成 — GitHub Projects + herdr

動く最小限。`[llm]` すら省略できる（省略した場合、`repo_hint` を持たないタスクは
リポジトリを決められず `pending` になる。リポジトリが 1 つなら実害はない）。

```toml
[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"                            # 起票先トラッカー（#554）

[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"
project_number = 6

[plugins.github]
enabled = true
kind = "task_source"

[github]
token = "cmd:gh auth token"
github_login = "tomoya-k31"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
profile = "implement"
agent = "herdr"
on_success = { set_status = "レビュー待ち" }
```

**`[[projects]]` は省略できない。** github プラグインはボードが 1 つも無い構成を `config/validate` で拒否する —— polling する対象が無いので、起動しても何も起きないためである。

## 2. 設計 → 実装ハンドオフ

人間のレビューを 1 回挟む 2 段構え。**どちらの成果物もエージェントが自分で書く**（設計は `gh issue comment`、実装は PR）—— github プラグインは publish しない（#398）。
`design` が先に定義されているので、`設計待ち` のカードは `design` に、`実装待ち` のカードは `implement` にマッチする。

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"                              # push しない。output は none に解決される
agent = "herdr"
on_success = { set_status = "設計レビュー待ち" }  # 人間のレビュー待ちへ

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }        # 人がレビュー後に手で移す
profile = "implement"
agent = "herdr"
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

[plugins.macos]
enabled = true
kind = "notifier"

[llm]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-haiku-4-5"
api_key_ref = "op://Dev/Openrouter/api_key"

[hooks]
auth_token_ref = "op://Dev/totsuka/hook-token"

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
[plugins.macos]
enabled = true
kind = "notifier"

[[workflows]]
name = "migration"
source = "github"
trigger = { labels = ["migration", "high-risk"] }   # 両方のラベルが必要（AND）
mode = "implement"
agent = "herdr"
output = "none"             # github は publish しない（#398）。profile を書かない
                            # 構成では output の明示が必須なので省略できない
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
| リポジトリパス不在 | `path` の展開結果がディスク上に存在しない |
| プレースホルダエラー | `worktree_location` に `{repo}` / `{repo_name}` / `{worktree_name}` / `{task_id}` / `{source}` 以外を使った（`{branch}` は廃止済みで専用のエラーになる） |
| 環境変数未設定 | `${VAR}` 参照先が export されていない |

**警告**（実行は止まらない）の代表例: トリガーの重複、`verification = "human"` なのに notifier が無い、
`verification != "llm"` なのに `rubric` がある、
フック対応 agent を使う workflow があるのに `[hooks].auth_token_ref` が未設定（最後の 1 つは `doctor` では fail 扱い。前述）。

`totsuka run` はエラーがあると起動を中止し、警告は表示した上で続行する。
