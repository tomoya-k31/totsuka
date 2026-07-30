---
type: Guide
title: 設定リファレンス（config.toml）
description: config.toml と plugins/{name}.toml の全キー・デフォルト値・意味の一覧。シークレット参照、設定スキーマのバージョニング方針、ワークフロー、出力ポリシー、掃除ポリシー、並列上限、[hooks]・検収設定、task-source-slack の plugins/slack.toml を含む。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/config/schema.rs
tags: [config, reference, toml, secrets, workflow, worktree, slack, hooks, versioning]
generated: { by: human:tomoya-k31, at: 2026-07-31T02:00:00+09:00 }
status: stable
owner: tomoya-k31
---

本ドキュメントはキーの一覧・型・既定値を扱う。実際に貼って動く設定例、選択肢を持つキーの選び分け基準、
シナリオ別レシピは [設定例集](/development/config-examples.md) を参照。

# 場所

- 共通設定: `$XDG_CONFIG_HOME/totsuka/config.toml`（既定 `~/.config/totsuka/config.toml`）
- プラグイン個別設定: `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml`（Orchestrator は無解釈で保持し、シークレット解決後に `initialize` へ渡す）
- `--config <path>` で config.toml の場所を上書き可能（最上位の優先レイヤ）

`totsuka init` が雛形を生成する。`totsuka config validate` で検証、`totsuka config show [--redacted]` で表示。

# シークレット参照

文字列値は次のいずれか。プレーンなシークレットは設定に書かない。

- `keychain:<service>/<account>` — macOS Keychain から解決
- `op://<vault>/<item>/<field>` — 1Password から解決（#156、[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)）。1Password CLI（`op read --no-newline`）へのシェルアウトで、事前に `op signin` 済みの対話セッションが前提。`config.toml` / `plugins/{name}.toml` の**任意の文字列 leaf** で使える（例 `api_key_ref = "op://Dev/Openrouter/api_key"`、Slack の `user_token = "op://Dev/Slack/user_token"`）。`op` は cross-platform のため **非 macOS でも動く唯一の実働バックエンド**。未導入はインストール導線（macOS は `brew install 1password-cli`、他プラットフォームは公式ドキュメント）、item 不在は not found、未サインインは「`op signin` を実行」の actionable エラーになり、`totsuka doctor` は設定に `op://` があるときのみ `op --version` / `op whoami`（非プロンプト）を検査する
- `${ENV_VAR}` を含む文字列 — 環境変数から展開
- `~` / `${ENV}` はパスでも展開される

# トップレベル

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `version` | int | 1 | 設定スキーマ版。不一致は起動時検証でエラーになる（[バージョニング方針](#設定スキーマのバージョニング方針)。自動マイグレーションは無い） |
| `max_concurrency` | int? | 4 | グローバル同時実行上限（F-40） |
| `[[repositories]]` | 配列 | — | 対象リポジトリ（下記） |
| `[plugins.{name}]` | テーブル | — | プラグインのロスター + 共通項目（下記） |
| `[[workflows]]` | 配列 | — | ワークフロー定義（下記） |
| `[llm]` | テーブル | なし | AI Gateway 設定（下記）。無い場合、LLM が必要なリポジトリ選択は `pending` にフォールバック |
| `[worktree]` | テーブル | — | worktree 配置・掃除（下記） |
| `[log]` | テーブル | — | ログ設定（下記） |
| `[output]` | テーブル | — | 出力ポリシーの PR テンプレート（下記） |
| `[hooks]` | テーブル | — | エージェント CLI フックイベント受信の設定（下記、#131） |
| `default_tool` | string? | `"claude"` | グローバル既定の AI ツール名（#196）。workflow / repo が指定しない場合に適用 |
| `[tools.{name}]` | テーブル | — | AI ツールレジストリ（下記、#196）。組み込み既定 `claude` を上書き・拡張 |
| `[prompts]` | テーブル | — | AI ツールへ差し込むプロンプト文の上書き（下記、#314） |

# 設定スキーマのバージョニング方針

現行のスキーマは **v1 のみ**（`CURRENT_SCHEMA_VERSION = 1`。一度も上がっていない）。

`version` が `CURRENT_SCHEMA_VERSION` と一致しない config.toml は起動時検証でエラーになり、
**totsuka が設定を書き換えることはない**。`config validate` / `run` / `doctor` は同じ検証
（`Cx::validate_config`）を共有するため 3 つとも同じ不一致を検出するが、扱いは異なる:
`config validate` と `run` は**エラーで停止**（exit 1）、`doctor` は `config` チェックの
**失敗として報告**する（exit 3。診断コマンドなので他のチェックは続行する）。

エラーは向きによって案内が逆になる（#276）:

- `version` が新しい → totsuka 側が古い。「そのスキーマ版に対応した totsuka へ更新しろ」
- `version` が古い → config 側が古い。「config.toml を現行版へ更新し `version` を書き換えろ」

**`totsuka config migrate` は存在しない。** 移行すべき差分がゼロの段階で移行フレームワークだけ先に
建てても、使われないコードパスが増えるだけで、v2 の移行方式も今は決められないため。

## v2 を切るときに決めること

1. **移行方式** — 起動時自動マイグレーション / 明示コマンド（`config migrate`）/ 手動編集の案内、のいずれか。
   state.db 側は #275 で「`run` だけが移行を適用し、他の入口は `SchemaOutdated` で止める」を選んでいる
   （[ADR-0017](/decisions/adr-0017-state-db-compatibility-policy.md)）。config でも同じ線を採るかを先例として判断する。
2. **`version` 省略時のデフォルト** — 現在 `#[serde(default)]` は `CURRENT_SCHEMA_VERSION` を返す。
   つまり **`version` を書いていない config.toml は常に「現行版」として読まれる**。
   v1 時代に書かれた `version` 無しの config.toml は、totsuka が v2 になった瞬間に v2 の設定として
   黙って解釈され、バージョンガードを素通りする。v2 を切る時点で既定値を 1 に固定するか、
   `version` を必須キーにするかを決める必要がある。

この 2 点を決めるまで `config migrate` は実装しない。

# `[[repositories]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ブランチ名・ログで使う安定 ID |
| `path` | string | 必須 | ローカルクローンのパス（`~`/`${ENV}` 展開） |
| `summary` | string? | なし | LLM リポジトリ選択の説明（F-11） |
| `tool` | string? | `default_tool` | このリポジトリへディスパッチされるタスクの既定 AI ツール（#196）。`[[workflows]].tool` のピンが優先。旧 `default_agent`（未消費のまま削除）とは別軸: agent = pane runner、tool = pane 内 CLI |
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
| `poll_interval_secs` | int? | 60 | task_source のみ。**fetch 型 source**（`task_submit` capability 未宣言）では `run --watch` の Orchestrator 側ポーリング間隔（F-06）。**push 型 source**（`task_submit` 宣言、0.1.6 / [ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）はポーリングされず、この値は `initialize` でプラグインへ転送されプラグイン内部の fetch 周期になる |

# `[[workflows]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ワークフロー名 |
| `source` | string | 必須 | task_source インスタンス名 |
| `trigger` | テーブル | `{}`（全マッチ） | トリガー条件。`status`/`project_status`/`label`/`labels` は Orchestrator が防御的に再判定、他キーはプラグインが `initialize` の `triggers` として受け取り解釈する |
| `mode` | enum | 必須 | `plan`（push/PR 禁止 F-82）/ `implement` |
| `agent` | string | 必須 | agent_ide インスタンス名 |
| `output` | enum | 必須 | `pull_request` / `source` / `none` |
| `on_success` | `{ set_status = "..." }`? | なし | 成功時にソース側ステータスを更新（F-84） |
| `on_failure` | `{ set_status = "..." }`? | なし | 失敗時にソース側ステータスを更新（publish 失敗など retry 可能な失敗では書き戻さない） |
| `verification` | enum | `llm` | 完了自己申告の検収方式（D-01）: `llm`（prompt 型 Stop フックで in-session 検収）/ `human`（`totsuka task verify` 待ち。有効な notifier が無いと警告）/ `none`（検収なし） |
| `timeout_secs` | int? | 1800 | 最終フックシグナルからの無応答上限秒。超過でエスカレーション（D-03） |
| `rubric` | string? | なし | llm 検収の判定基準文（prompt 型フックに埋め込む）。`verification != "llm"` に設定すると警告。`[prompts]`（#314）より前からあるキーで、動作は維持される — 同じワークフローの `[workflows.prompts].verification_rubric` にのみ負け、グローバルの `[prompts].verification_rubric` には勝つ |
| `[workflows.prompts]` | テーブル | — | このワークフロー専用のプロンプト上書き（下記 `[prompts]` の 5 キー。最優先層） |
| `tool` | string? | なし | AI ツールの明示ピン（#196）。優先順位は workflow > repo > `default_tool`。`verification = "llm"` は Claude の prompt 型 Stop フックが必要なので、非 claude 系へ解決されうる構成では `tool = "claude"` のピンを警告で提案 |

定義順に first-match（F-81）。同一ソース内でトリガーが重なると警告。

# `[tools.{name}]`（AI ツールレジストリ、#196）

pane 内で起動する AI ツール CLI の定義。`{name}` は `default_tool` / `[[repositories]].tool` / `[[workflows]].tool` から参照する任意の名前。組み込み既定として `claude` / `codex`（#196 Phase 2）/ `opencode`（#196 Phase 3）が常に存在し、同名エントリで上書きできる。同一 kind の別プロファイル（例 `claude-fast`）も定義可能。**全 kind が dispatch 可能**（アダプタ無し kind の validate 拒否は将来の kind 追加に備えて残置）。

`kind = "codex"` の利用には一回きりのセットアップ（hooks trust・対象リポジトリ trust）が必要 → [Codex ツールのセットアップと hooks trust 運用](/operations/codex-tool-setup.md)。`kind = "opencode"` はアセット自動配置のみで trust 不要だが縮退が多い → [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md)。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `kind` | enum | 必須 | アダプタ種別: `claude` / `codex` / `opencode`。argv 組立と完了検知方式を決める |
| `command` | string? | kind 名 | 空白区切りのコマンドライン。先頭 = プログラム、残り = 基本引数（例 `"claude --model haiku"`） |
| `mode_args` | string[]? | kind 既定 | implement モードで追加する引数（codex 既定: `["--sandbox", "workspace-write", "--ask-for-approval", "on-request"]`、claude / opencode 既定: なし） |
| `plan_args` | string[]? | kind 既定 | plan モードで追加する引数（claude 既定: `["--permission-mode", "plan"]`、codex 既定: `["--sandbox", "read-only"]` — plan permission mode 不在の縮退、opencode 既定: `["--agent", "totsuka-plan"]` — 全 deny の plan エージェント） |

kind ごとの argv 組立の差分: claude はフック設定を `--settings <path>` で受け、resume は `--resume <id>` フラグ。codex はフックがグローバル登録（`~/.codex/hooks.json`、`TOTSUKA_*` env でゲート）のため `--settings` 相当は付かず、resume は `resume <id>` **サブコマンド**（基本引数の直後・モード引数の前に挿入）。 opencode もグローバル配置の JS プラグイン（env ゲート）で完了検知するため `--settings` 相当は無く、resume は `-s <id>` フラグ。opencode は不可視注入が無いため、タスク指示 + マーカー規約は**可視の extra_context** として pane に渡る。

ツール解決はディスパッチ時に workflow ピン > repo 既定 > `default_tool` > 組み込み `claude` の順。解決結果は core が完全な argv/env（`ToolLaunchSpec`）へ組み立てて agent プラグインに渡すため、herdr.toml の `agent_command` / `plan_args` は後方互換フォールバック（deprecated）になった。

# `[prompts]`（AI ツールへ差し込むプロンプト、#314）

claude / codex / opencode に差し込むプロンプト文の上書き。組み込みデフォルトは
`crates/orchestrator-core/src/prompts/defaults.toml` にバイナリ埋め込みされており、
このテーブルは**キー単位の上書き**である（未指定キーは組み込みのまま）。値はインライン文字列のみで、
ファイルパス指定の形式は無い。

| キー | 型 | 既定 | 説明 | プレースホルダ |
|---|---|---|---|---|
| `marker_self_report` | string? | 組み込み | 全ディスパッチに注入される完了自己申告指示。invisible injection 対応ツールは env `TOTSUKA_PROMPT_CONTEXT` 経由、非対応（opencode）は可視 `extra_context` | `{marker_completed}` `{marker_needs_input}` `{marker_failed}` |
| `verification_rubric` | string? | 組み込み | llm 検収の判定基準文 | — |
| `verification_background_exemption` | string? | 組み込み | バックグラウンドタスク実行中の中間停止を検収対象から外す文 | — |
| `verification_marker_convention` | string? | 組み込み | 検収後にマーカーを再掲させる文 | `{marker_completed}` `{marker_needs_input}` `{marker_failed}` |
| `verification_prompt` | string? | `"{rubric}\n\n{background_exemption}\n\n{marker_convention}"` | 上記 3 つの組み立て方。節の並べ替え・省略ができる | `{rubric}` `{background_exemption}` `{marker_convention}` |
| `opencode_plan_agent` | string? | 組み込み | opencode plan モードのエージェントファイル（`agents/totsuka-plan.md`）の**散文本体**。**グローバル専用**（ディスク上の 1 枚を全セッションが共有するため。`[[workflows]].prompts` に書くとパースエラー） | — |

`verification_*` の 4 キーは `verification = "llm"` のワークフローでのみ使われる（prompt 型 Stop フックを持つのは claude だけで、他ツールでは `human` へ縮退する）。

`opencode_plan_agent` は**散文本体のみ**である。YAML frontmatter（`mode: primary` と `permission: {edit: deny, bash: deny, task: deny}`）は Rust 側で固定されており設定できない — この deny マップが plan モードの読み取り専用保証そのもので、散文に見えるキーから `bash: allow` を注入できると権限昇格になるためである（[ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md)）。値が `---` の行を**どこかに**含む場合は検証エラーになる。frontmatter は慣例上ファイル先頭でしか解釈されないので後続の `---` は本来ただの水平線だが、opencode のパーサはこちらで検証できず、ここは権限境界なのでその推論に依存しない設計にしている（本文の水平線は `***` で書ける）。opencode は claude の `--permission-mode plan` や codex の `--sandbox read-only` に相当する構造的な plan フラグを持たないため、**このエージェントファイルが plan 意図の唯一の強制手段**である。

**マーカー自体（`<<STATUS:COMPLETED>>` など）は設定できない。** `on-stop.sh`（bash）と `totsuka-opencode.js` がリテラルをパースし、[ADR-0020](/decisions/adr-0020-status-marker-stays.md) が 3 ツール共通の唯一の完了信号と定めているため。ここで編集できるのは規約を**教える散文**であって規約そのものではない。`{marker_*}` はそのワイヤ定数へ解決される。

## 優先順位

強い順に 4 層。

1. `[[workflows]].prompts.<key>` — ワークフロー専用（`opencode_plan_agent` を除く。グローバル専用のため）
2. `[[workflows]].rubric` — レガシー（rubric の葉のみ）
3. `[prompts].<key>` — グローバル
4. 組み込みデフォルト

**2 が 3 より強いのは意図的**である。どちらもワークフロー単位の設定なので、逆順にすると `[prompts].verification_rubric` を新たに足した瞬間に既存の per-workflow `rubric` が黙って上書きされる。

## 展開規則

- `{placeholder}` の置換は**シングルパス**。置換された値の中に `{token}` があっても再展開されない
- 組み立ては 2 段階（葉 → `verification_prompt`）。各段がシングルなので、rubric 内に書いたリテラル `{marker_convention}` は挿入されるだけで展開されない
- プレースホルダ名は識別子（`[A-Za-z_][A-Za-z0-9_]*`）に限られる。それ以外の波括弧は**中身**として素通しされるので、`{"ok": true}` のような JSON の形をプロンプトに書いてよい（#328）
- 未知のプレースホルダはそのまま出力される（レンダリング時は fail-soft）
- プロンプトの変更は**次のディスパッチから有効**。稼働中セッションの `--settings` は書き換わるが、既に起動しているエージェントには届かない

## 例

```toml
[prompts]
verification_rubric = "変更が意図どおり動くことを、実際にテストを走らせて確認してください。"

[[workflows]]
name = "slack-reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"

  [workflows.prompts]
  verification_rubric = "返信案が質問に直接答えているか、根拠が示されているかを検証してください。"
```

# `[llm]`（AI Gateway）

OpenAI 互換 `/chat/completions` を前提。repo_hint を持たないタスクのリポジトリ選択（F-11）に使うほか、**task_source プラグインへ initialize 時に分類用 default として供給される**（#119、protocol 0.1.2。`api_key_ref` は解決済みの値で渡る。プラグイン自身の LLM 設定が常に優先）。

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
| `location` | string? | `<state dir>/worktrees/{repo_name}/{branch}` | 配置テンプレート。`{repo}`/`{repo_name}`/`{branch}`/`{task_id}`/`{source}`/`${ENV}`/`~` を展開 |
| `cleanup` | policy? | `manual` | implement モードの掃除ポリシー（F-23） |
| `plan_cleanup` | policy? | `immediate` | plan モードの掃除ポリシー（F-85） |

**既定値の解決**: `location` を省略したときの `<state dir>` は `$XDG_STATE_HOME/totsuka`、`XDG_STATE_HOME` 未設定なら XDG 仕様どおり `$HOME/.local/state/totsuka` にフォールバックする（state DB・ログ・hook spool と同じ解決）。既定値はテンプレート文字列ではなく**解決済みのパス**として組み立てられるため、`${ENV}` 展開を経由しない。逆に `location` を**明示した場合の `${ENV}` は未設定だとエラー**（`expand_env` は空文字にフォールバックしない）で、worktree 作成はタスクのディスパッチ時なので run 起動時ではなく毎タスクの失敗として現れる。`totsuka doctor` の `worktree-location` チェックが事前に検出する。`[[repositories]].worktree_location` の上書きも同じ扱い。

掃除ポリシー値: `"immediate"` / `"manual"` / `{ retention_days = 5 }` / `"keep_7d"` / `"keep_28d"`（#210。`keep_*` は `{ retention_days = 7 }` / `{ retention_days = 28 }` の糖衣。他の日数は従来どおり明示形式で）。未コミット変更のある worktree は決して削除しない。

```toml
[worktree]
cleanup      = "keep_7d"              # implement: 7日保持ののち削除
plan_cleanup = "immediate"            # plan: 即削除（既定）
# cleanup    = { retention_days = 3 } # 任意日数は明示形式
```

**pane との連動（#210, [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）**: worktree を「削除する」と判定したとき、その前にタスクの herdr pane が自動で閉じられる（`session/release`）。保持中（retention 未経過 / `manual`）や未コミット変更で削除を見送った worktree の pane は残る。**既定の `cleanup = "manual"` では worktree も pane も自動では消えず、タスクごとに pane が増えていく**点に注意 — コミット済み未 push の作業を pane で確認したい運用でなければ `keep_7d` を推奨する。

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

# `[hooks]`

Claude Code フックイベント受信（UDS）の設定（#131。全キー省略可、`deny_unknown_fields`）。値の実使用は UDS サーバ・フックスクリプト側の issue（#136/#137）で配線される。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `auth_token_ref` | string? | なし | フック POST を認証する Bearer トークンのシークレット参照（E-03、例 `keychain:totsuka/hook-token`）。**運用上は必須**（未設定時の防御は 0600 の UDS パーミッションのみ）。未設定は #209 でツール側が検出するようになった: フック対応 agent（マニフェストが `resume_session` または `diagnostics_snapshot` を宣言）を使う workflow がある場合、`config validate` / `run` が該当 workflow ごとに警告を出し、`doctor` は **fail**（終了コード非 0）。フック対応 agent を使わない構成では doctor は warn 表示のみ（終了コードは成功）。参照を設定したのに解決できない場合は構成によらず fail |
| `socket_path` | string? | 組み込み既定 | 受信 UDS のパス（例 `${XDG_RUNTIME_DIR}/totsuka/agent-events.sock`） |
| `spool_dir` | string? | 組み込み既定 | POST 失敗時にイベントを退避するスプールディレクトリ（E-07、例 `${XDG_STATE_HOME}/totsuka/hooks/spool`） |
| `block_retry_limit` | int? | 3 | Stop フック block 差し戻しの連続上限。超過でエスカレーション（D-02） |

# `plugins/slack.toml`（task-source-slack）

config.toml 側の推奨設定。task-source-slack は Socket Mode で受けたイベントを即座に `task/submit` で push するイベント駆動ソースで、`poll_interval_secs` は使わない（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)。旧: プラグイン内バッファに積み `tasks/fetch` で吸い上げていたため短周期ポーリングを推奨していたが、#187 の push 移行で不要になった）:

```toml
[plugins.slack]
enabled = true
kind = "task_source"
```

`plugins/slack.toml` の全キー（`deny_unknown_fields`。導入手順は [Slack セットアップ Quickstart](/operations/slack-quickstart.md)、トークンの扱いは [取り扱いポリシー](/security/slack-user-token.md)）:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `app_token` | string | 必須 | App-Level Token（`xapp-`、Socket Mode 用）。Keychain 参照推奨 |
| `user_token` | string | 必須 | User OAuth Token（`xoxp-`、本人名義の読み書き）。Keychain 参照推奨 |
| `bot_token` | string? | なし | Bot User OAuth Token（`xoxb-`、[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)・#305）。設定すると返信案・ピッカー到着時に bot が本人へ通知 DM（ナッジ）を送る。**未設定なら機能 off**（起動時 warn 1 回）。設定時は TokenGuard が `auth.test` で probe。Keychain 参照推奨 |
| `target_user_id` | string | 必須 | 自分の Slack ユーザー ID（`U…`）。このユーザー宛メンションをタスク化し、TokenGuard が `auth.test` の identity と一致検証 |
| `thread_context_limit` | int | 6 | タスク本文に含めるスレッド直近メッセージ数 |
| `reply_style` | string? | なし | 返信トーンの指示（タスク本文へ注入、例 `"丁寧語で簡潔に"`） |
| `[prompts]` | テーブル | — | このプラグインが送るプロンプト文の上書き（下記、#318） |
| `source_name` | string | `slack` | `Task.source` に刻印するソース名 |
| `[[repos]]` | 配列 | なし（省略可、#109） | リポジトリ候補。`name`（config.toml の `[[repositories]].name` と一致必須）/ `summary`?（LLM 分類の材料）/ `path`?（README 先頭を分類材料に追加）。**省略時は config.toml の `[[repositories]]`（name/summary/path）がそのまま候補になる**ため通常は書かなくてよい。明示した場合はそちらが優先（候補の絞り込み・summary の上書きに使う） |
| `[[channel_groups]]` | 配列 | なし | チャンネル名 prefix → 候補 repos の絞り込みルール（定義順 first-match）。`prefix` / `repos`（`[[repos]]` に存在する名前のみ） |
| `[llm]` | テーブル | なし（省略可、#119） | リポジトリ分類用 OpenAI 互換 LLM。`base_url` / `model` / `api_key` / `confidence_threshold`（既定 0.6、未満はエフェメラル選択へ）。**省略時は config.toml の `[llm]`（initialize で供給）が default になる**（`api_key_ref` 必須 — キーなし供給は採用されない。`confidence_threshold` は既定 0.6）。明示した場合はそちらが優先。候補 2 件以上でどちらにも無ければ initialize が `CONFIG_INVALID` |
| `api_url` | string | `https://slack.com/api` | Web API ベース URL（テスト用上書き） |
| `max_retries` | int | 3 | リトライ可能な API 失敗の最大再試行回数 |

## `[prompts]`（task-source-slack、#318）

このプラグインが送るプロンプト文の上書き。組み込みデフォルトは `plugins/task-source-slack/src/defaults.toml` にバイナリ埋め込みされており、このテーブルは**キー単位の上書き**である（未指定キーは組み込みのまま）。**キー名がそのまま設定キー**である。

| キー | 用途 | プレースホルダ |
|---|---|---|
| `reply_instructions` | 返信案作成の指示（`Task.instructions` として帯域外配送される） | — |
| `reply_style_suffix` | `reply_style` が設定されているときだけ `reply_instructions` に追記される | `{style}` |
| `body_template` | ペインに表示されるタスク本文 | `{sender}` `{channel}` `{text}` |
| `body_thread_header` | スレッド文脈セクションの見出し | `{count}` |
| `body_thread_line` | スレッド文脈 1 行ぶん | `{line}` |
| `body_thread_unavailable` | 文脈取得に失敗したときにセクションごと差し替わる文 | — |
| `classifier_system` | リポジトリ分類 LLM の system プロンプト | `{repo_names}` |
| `classifier_user` | 同 user メッセージ | `{mention_text}` `{thread_context}` `{catalog}` |
| `classifier_correction` | 応答が JSON として壊れていたときの再試行ターン | — |

注意点:

- **`{text}` は `>` 引用済みで渡る。** 改行から `\n> ` への書き換えは展開**前**に Rust 側で行うので、先頭の `> ` を落としたテンプレートを書いても継続行は壊れないし、`> ` を残すテンプレートが二重引用になることもない。
- **`{text}` `{thread_context}` `{catalog}` は Slack の投稿者が内容を決められる。** 展開はシングルパスなので、本文に `{catalog}` と書かれたメンションはその文字列として挿入されるだけで、候補リスト差し込みにはならない。
- `classifier_system` の既定値は JSON 出力形の `{"repo": string, ...}` をリテラルに含む。プレースホルダ名は識別子（`[A-Za-z_][A-Za-z0-9_]*`）に限られるので、これは中身として素通しされる。
- 未知のプレースホルダはそのまま出力され、`initialize` 時に警告としてログに出る。**エラーにしないのは意図的である**（このプラグインは `config/validate` フックを持っているのでエラーにもできる）— 未知キーはそのまま描画されるので症状はドラフト中に見える `{token}` であり、core の `[prompts]` がエラーにするのは、あちらで消えるのが完了マーカー規約で症状がタイムアウトでのエスカレーションだけだからである。
- ここは **LLM 向けのプロンプトのみ**である。悪い上書きは分類の劣化（スレッド内ピッカーへフォールバックする）や返信案の質低下に留まり、core の `[prompts]` と違って完了検知は壊せない。

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
