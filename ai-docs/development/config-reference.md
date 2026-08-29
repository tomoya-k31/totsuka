---
type: Guide
title: 設定リファレンス（config.toml）
description: "config.toml の全キー・デフォルト値・意味の一覧。設定ファイルは 1 本で、プラグイン個別設定もトップレベルの [<name>] テーブルに入る。シークレット参照、設定スキーマのバージョニング方針、[[projects]] のトラッカー宣言、ワークフローとプラグインが定義する追加プロパティ、出力ポリシー、掃除ポリシー、並列上限、[hooks]・検収設定、task-source-github の [github]、task-source-notion の [notion]、task-source-slack の [slack]、agent-ide-herdr の [herdr] を含む。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/config/schema.rs
tags: [config, reference, toml, secrets, workflow, worktree, github, notion, slack, hooks, versioning]
generated: { by: claude-code/opus-5, at: 2026-08-27T06:45:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/config-reference.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/config-reference.md docs/config-reference.ja.md -->

本ドキュメントはキーの一覧・型・既定値を扱う。実際に貼って動く設定例、選択肢を持つキーの選び分け基準、
シナリオ別レシピは [設定例集](/development/config-examples.md) を参照。

# 場所

**設定ファイルは 1 本だけ** `$XDG_CONFIG_HOME/totsuka/config.toml`（既定 `~/.config/totsuka/config.toml`）。

- `--config <path>` で場所を上書き可能（最上位の優先レイヤ）
- プラグイン個別設定は同じファイルのトップレベル `[<name>]` テーブル（Orchestrator は無解釈で保持し、シークレット解決後に `initialize` へ渡す）

`[<name>]` は **#554 で廃止**した（[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）。所有をファイル位置で表現していたため、`[[workflows]]` のような core の構造体の中には届かなかった。

**残っていても読まれない。** `version` は上げず検出もしないと決めたので、旧ファイルはパースエラーにもならず、プラグインが空設定で起動する。移行時に消すこと。

`totsuka init` が雛形を生成する。`totsuka config validate` で検証、`totsuka config show [--redacted]` で表示。

# シークレット参照

文字列値は次のいずれか。**プレーンなシークレットは設定に書かない**（F-62）。

**通常は `op://`（1Password）を使う。** クロスプラットフォームで動く唯一の
シークレットストアで（`keychain:` は macOS 専用）、
`config.toml` の任意の文字列 leaf に書ける。`${ENV_VAR}` と `cmd:` は
用途がはまるときの選択肢、`keychain:` は macOS 専用。

- `op://<vault>/<item>/<field>` — 1Password から解決（#156、[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)）。1Password CLI（`op read --no-newline`）へのシェルアウトで、事前に `op signin` 済みの対話セッションが前提。`config.toml` の**任意の文字列 leaf** で使える（例 `api_key_ref = "op://Dev/Openrouter/api_key"`、Slack の `user_token = "op://Dev/Slack/user_token"`）。`op` は cross-platform のため **非 macOS でも動く唯一のシークレットストア**（`keychain:` は macOS 専用。`${ENV_VAR}` と `cmd:` もどこでも動くが、値を持つのは環境や別ツールでありストアではない）。未導入はインストール導線（macOS は `brew install 1password-cli`、他プラットフォームは公式ドキュメント）、item 不在は not found、未サインインは「`op signin` を実行」の actionable エラーになり、`totsuka doctor` は設定に `op://` があるときのみ `op --version` / `op whoami`（非プロンプト）を検査する
- `cmd:<command>` — コマンドを `/bin/sh -c` で実行し、その **stdout を秘密値**として使う（#444、[ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)）。`gh auth token` のように**別ツールが管理・ローテートする credential** 向け — 解決のたびに現在値を取るので、コピーの陳腐化が起きない（例 `token = "cmd:gh auth token"`）。末尾の改行は除去される。非ゼロ exit と空出力は起動時エラー（stderr の先頭行を引用、stdout は §5.2 により決して引用しない）。実行は `totsuka run` の解決時のみで、parse や `config show` はコマンドを実行しない。`totsuka doctor` は `op://` と同じ理由（非対話原則、#289）で `cmd:` を含むプラグインの probe を skip する。**コマンド文字列に秘密を直書きしないこと** — 参照文字列は設定の一部としてエラーメッセージに引用されうる。「設定に平文の秘密を書かない」規則はコマンド文字列にも適用され、秘密はコマンドに**取得させる**（それがこの形式の目的）
- `${ENV_VAR}` を含む文字列 — 環境変数から展開。export 済みの値をそのまま使いたいときに
- `keychain:<service>/<account>` — macOS Keychain から解決。**macOS でしか動かない**ので、
  他プラットフォームへ持ち運ぶ設定には使わない
- `~` / `${ENV}` はパスでも展開される

# トップレベル

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `version` | int | 1 | 設定スキーマ版。不一致は起動時検証でエラーになる（[バージョニング方針](#設定スキーマのバージョニング方針)。自動マイグレーションは無い） |
| `max_concurrency` | int? | 4 | グローバル同時実行上限（F-40） |
| `[[repositories]]` | 配列 | — | 対象リポジトリ（下記） |
| `[[projects]]` | 配列 | — | リポジトリの起票先トラッカー（下記、#554） |
| `[plugins.{name}]` | テーブル | — | プラグインのロスター + 共通項目（下記） |
| `[<name>]` | テーブル | — | プラグイン自身の設定（下記）。`{name}` は `[plugins.*]` のロスターに居る名前だけ |
| `[[workflows]]` | 配列 | — | ワークフロー定義（下記） |
| `[llm]` | テーブル | なし | AI Gateway 設定（下記）。無い場合、LLM が必要なリポジトリ選択は `pending` にフォールバック |
| `[worktree]` | テーブル | — | worktree 配置・掃除（下記） |
| `[log]` | テーブル | — | ログ設定（下記） |
| `[hooks]` | テーブル | — | エージェント CLI フックイベント受信の設定（下記、#131） |
| `default_tool` | string? | `"claude"` | グローバル既定の AI ツール名（#196）。workflow / repo が指定しない場合に適用 |
| `[tools.{name}]` | テーブル | — | AI ツールレジストリ（下記、#196）。組み込み既定 `claude` を上書き・拡張 |

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
| `project` | string? | なし | 起票先トラッカー（`[[projects]].name`、#554）。**1 つだけ**。無いのは正常な状態（トラッカーを設定していない） |

# `[[projects]]`（トラッカー、#554）

リポジトリの起票先。GitHub Project / Notion database / 将来の Jira project を同じ形で並べる。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | `[[repositories]].project` が指す安定 ID |
| `source` | string | 必須 | このトラッカーを所有する task_source プラグイン名 |
| その他のキー | — | — | そのプラグインのもの。core は**無解釈**で `initialize` へ渡す |

```toml
[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"        # ← github が読む
owner_type = "user"
project_number = 6
triage_status = "📥 Inbox"

[[projects]]
name = "design-db"
source = "notion"
database_id = "…"           # ← notion が読む

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"
```

**`source` を書かせるのは推測できないからではない。** `project_number` を理解するのは github だけなので推測はできる。書かせるのは、**参照連鎖 `[[repositories]].project` → `[[projects]].name` → `[plugins.<source>]` をプラグインを起動せずに辿れる**ようにするため —— 壊れた参照は `config validate --offline` でも、ファイルを読む人間にも見える。

**`[[workflows]]` と違い引き取り規則は要らない。** 要素は `source` でちょうど 1 つのプラグインを名指すので所有が曖昧にならず、プラグイン自身の `deny_unknown_fields` がタイポを弾く。

検証（`config validate`）:

- `name` の重複はエラー
- `source` が有効な task_source でなければエラー
- `[[repositories]].project` が実在しない `name` を指したらエラー

## 逆引きリストは無くなった（ADR-0056 §4 の置き換え）

以前は各プラグインの設定に `[[projects]].repos` / `[[databases]].repos` を書き、それが**取り込みフィルタ兼 repo→ボードのマッピング**という 2 役を負っていた。#554 でその 1 箇所（`[[repositories]].project`）へ移した。複製ではなく移動である。

得られたもの:

- **2 プラグインが同じリポジトリを主張する状態が書けない。** 1 リポジトリ → 1 project → 1 source なので、`ClaimConflict` の検出機構ごと削除した
- github / notion / jira と増えても `repos = [...]` が 3 本に分かれない
- `repos` と `[[repositories]].name` を一致させる運用上の前提が消えた

# `[plugins.{name}]`

`{name}` はワークフローの `source` / `agent` と対応するインスタンス名。**ロスターであって設定ではない** —— プラグイン自身の設定はトップレベルの `[<name>]` に書く。

このロスターは、`[<name>]` を正当と認める根拠でもある: **ロスターに無い名前のトップレベルテーブルは検証エラー**になる（#554）。`RootConfig` の `deny_unknown_fields` を外した代わりで、検査はむしろ強くなった —— core キーのタイポ（`[worktre]`）もプラグイン名のタイポ（`[slak]`）も落ちる。以前は前者しか落ちなかった。

**プラグイン名に core のトップレベルキーは使えない**（`version` / `max_concurrency` / `repositories` / `projects` / `plugins` / `default_tool` / `tools` / `workflows` / `llm` / `worktree` / `log` / `hooks` / `prompts`）。使うと `[<name>]` が core のキーとして読まれ、**プラグインが空設定で黙って起動する**。プラグイン名はバイナリ名と同一で改名できない（[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md)）ので、ロスター登録の時点で拒否する。

なおこの予約リストは将来 core がトップレベルキーを増やせば伸びる。つまり**その名のサードパーティプラグインを後から使えなくする**。名前空間を共有した代償である。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `enabled` | bool | false | 有効化フラグ（F-56）。`totsuka plugin enable/disable` でも操作 |
| `kind` | enum | 必須 | `task_source` / `agent_ide` / `notifier` |
| `max_concurrency` | int? | 無制限 | agent プラグイン単位の同時実行上限（F-42） |
| `timeout_secs` | int? | 120 | RPC タイムアウト秒 |
| `log_level` | string? | なし | プラグインのログレベル |
| `restart` | bool | true | クラッシュしたら再起動するか（#495 / [ADR-0051](/decisions/adr-0051-plugin-supervision.md)）。指数バックオフ（1s / 2s / 4s …）で**最大 5 回・5 分のスライディング窓**、尽きたら `escalated` を通知する。**`false` にしても検知は残る** — ログに出て `RunSummary.plugin_crashes` に計上され、`escalated` も飛ぶ（`plugin_restarts` のほうは 0 のまま。だから死亡を数える counter が別に要る）。agent なら在席タスクも畳まれる。止まるのは再起動だけで、プラグインを手で調べたいときの形。バックオフの形は設定に出していない（運用者が調整する材料を持たないため） |

`poll_interval_secs` はここには**無い**（0.6.0 / #554 で各ソースの `[<name>]` へ移動 — core は使わず転送するだけだった）。下の `[github]` の節を参照。

# `[[workflows]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ワークフロー名 |
| `source` | string | 必須 | task_source インスタンス名 |
| `trigger` | テーブル | `{}` | トリガー条件。**中身を解釈してタスクを選ぶのはプラグインである**（#554）。ただし `status` は **core 所有のキー**で（#575、[ADR-0062](/decisions/adr-0062-status-vocabulary.md)）、Orchestrator が閉路検査の列グラフを組むために読む —— `on_*` の書き戻し先と文字列を突き合わせるだけで、タスクの照合には使わない。受理するかは各ソースの自由（状態列を持たない slack は未知キーとして拒否する）。プラグインが `initialize` の `workflows` として受け取り、first-match を走らせる。github の `status` トリガーは**列への入場がリクエスト**（#556）: 完了後でも人間がカードをトリガー列へ差し戻せば同じワークフローが再実行される（誰が再実行するかは assignee と claim が決める）。**別のワークフローのトリガー列へ入った場合は、その会話がそのワークフローへ引き渡される**（#565、列パイプライン）— worktree とエージェントのセッションを保ったまま次の段が始まる。引き渡しは**完了済みの会話だけ**。実行中に別ワークフローの列へ移された配送は見送られ、**ポーリング型のソース（github / notion）なら次の tick で運び直されて引き渡しが成立する**が、ack を先に返す Slack は再配送しないのでそのトリガーは失われる（実行が終わってから付け直すこと）。**この表の未知キーは `initialize` の硬い失敗になる**（#574）。トリガーの解釈は `.get("…")` なので、読み手の居ないキーは黙って捨てられ、条件が 1 つ減る —— つまりタイポはトリガーを**狭めず広げる**（`assinee` と書くと「条件なし」になり、除外したかったタスクにこそ発火する）。エラーはそのソースが読む有効キーを列挙するので、改名からの移行案内も兼ねる。`trigger = {}`（catch-all）はキーが無いので常に有効 **`assignee` は取り込みの assignee ゲートそのものである**（#572、[ADR-0063](/decisions/adr-0063-trigger-assignee.md)）。`"@me"` / `"@none"` / `"@any"` / ログイン名 / それらの配列（OR）で、**省略時は `["@me", "@none"]`** —— これは #572 以前のプラグイン全体のゲートと同一である。旧ゲートは削除したので**二重にはならない**（書いた条件を書いていない条件が上書きすることが構造的に起きない）。`@` はログイン名に使えない文字なので、`me` / `none` / `any` という実在しうるログイン名と衝突しない。**`@any` は他人のタスクも取り込む**ので、書くときは意図的であること。何と突き合わせるかはソース固有で、github は Issue 組み込みの assignee と `github_login`、notion は `property_map.assignee` が名指すプロパティと `notion_user_id` を使う。`assignee` を単独で書く（`status` を併記しない）と配送に lane identity が付かず **1 タスク 1 回**になるので、起動時に警告が 1 行出る |
| `profile` | enum? | なし | 4 原型のいずれか（`answer` / `triage` / `design` / `implement`）。`mode` / `output` / `verification` の 3 つをまとめて決める。うち `mode` / `verification` は併記不可、`output` は併記すればそちらが勝つ（下記） |
| `mode` | enum | `profile` が無ければ必須 | `plan`（設計・起案。worktree は作るが push・PR は**想定していない** — F-82。ただし**強制はされていない**、下記）/ `implement` |
| `agent` | string | 必須 | agent_ide インスタンス名 |
| `output` | enum | `profile` が無ければ必須 | `source` / `none`。**`pull_request` は廃止** — push と PR 作成はエージェントの責務になった（F-86、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）。残っていると起動時に `unknown variant` で落ちるので `source` に変更し、PR 作成手順はリポジトリの規約に書く |
| `on_start` | `{ status = "..." }`? | なし | dispatch 直前にソース側ステータスを更新（#556、[ADR-0059](/decisions/adr-0059-task-claim-exclusion.md)）。ボードが「実行中」を映し、多人数運用では `in_progress_statuses` による取り込み除外の第 2 防御線になる。**未設定なら何も書かない**（従来挙動）。**使うなら `on_failure` も設定すること** — 失敗時に列が実行中のまま残り、ボードと実態が食い違う。**`on_start` / `on_success` / `on_failure` の未知キーは起動時エラーになる**（#574、有効キーは `status` のみ）。`trigger` と違いこれは core が読むテーブルなので、検査も `config validate`（`run` が共有する）の側にある。検査が要るのは壊れ方が無言だからで、`set_stauts` と書くと**タスクは成功したのにボードだけ動かない** |
| `on_success` | `{ status = "..." }`? | なし | 成功時にソース側ステータスを更新（F-84） |
| `on_failure` | `{ status = "..." }`? | なし | 失敗時にソース側ステータスを更新（publish 失敗など retry 可能な失敗では書き戻さない） |
| `verification` | enum | `llm` | 完了自己申告の検収方式（D-01）: `llm`（prompt 型 Stop フックで in-session 検収）/ `human`（`totsuka task verify` 待ち。有効な notifier が無いと警告）/ `none`（検収なし）。`profile` 指定時は書けない |
| `timeout_secs` | int? | 1800 | 最終フックシグナルからの無応答上限秒。超過でエスカレーション（D-03）。**`0` はこのワークフローを掃引の対象外にする**（#439、[ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md)）— 人間が pane を見ている attended 運用向け。真にハングしたエージェントも検知されなくなるので、無人ワークフローには設定しないこと |
| `rubric` | string? | なし | llm 検収の判定基準文（prompt 型フックに埋め込む）。`verification != "llm"` に設定すると警告。**唯一のプロンプト上書き面**（下記）で、profile の既定より強い |
| `tool` | string? | なし | AI ツールの明示ピン（#196）。優先順位は workflow > repo > `default_tool`。`verification = "llm"` は Claude の prompt 型 Stop フックが必要なので、非 claude 系へ解決されうる構成では `tool = "claude"` のピンを警告で提案 |
| `initial_prompt` | string? | なし | このワークフローのエージェントに渡す**追加の前置き指示**（#415、[ADR-0038](/decisions/adr-0038-workflow-initial-prompt.md)）。**可視**（pane に見える）・**タスク本文の前**・**新規会話のときだけ**。下記 |
| `cleanup` | `[worktree]` と同じ語彙 | なし | この workflow のタスクの worktree 掃除を **`[worktree]` の mode 既定より優先**して上書き（#548、ADR-0057）。`manual` にすると pane も worktree も残る（pane の寿命は worktree に従う、ADR-0010）。**タスク完了後に workflow を削除・改名すると引けなくなり mode 既定へ縮退する**（仕様。sweep が 1 行 log に出す） |

定義順に first-match（F-81）。**その判定を走らせるのはソースプラグインである**（#554） —— `initialize` で workflow 群を定義順に受け取り、`task/submit` でどれに属するかを名指す。Orchestrator は名前が実在しその `source` が submit してきたプラグインかだけを検証する。

## プラグインが定義する追加プロパティ（#554）

プラグインは `[[workflows]]` に自分のキーを**フラットに**足せる。core のキーと同格に書く。

```toml
[[workflows]]
name = "slack-books"
source = "slack"
agent = "herdr"
profile = "triage"
publish = "direct"      # ← slack が定義するプロパティ
```

**所有者は core が決めず、聞いて解決する。** workflow は `source` と `agent` の両方を名指すので、そのキーがどちらのものか Orchestrator には分からない。余ったキーは `initialize` で両方へ渡り、各プラグインが消費するものを答える:

| 引き取り手 | 判定 |
|---|---|
| 0 | **エラー**。タイポ（`profil = "triage"` はここで落ちる）か、その workflow が名指していないプラグイン向けのキー |
| 1 | そのプラグインのもの |
| 2 | **エラー**。1 つのキーが 2 つの意味を持つので、Orchestrator は勝手に決めない |

`WorkflowConfig` の `deny_unknown_fields` はこのために外したが、**タイポ検出は失われていない** —— 誰も引き取らないキーが同じ場所で落ちる。

検査は `totsuka run` と `totsuka config validate` の**両方**にある。`run` は後者を呼ばないので、片方だけだと「`config validate` を実行しない運用者には何も検出されない」。**`--offline` は検査できない**（プラグインに聞けないため）。

現時点で存在する追加プロパティ:

| キー | 所有 | 意味 |
|---|---|---|
| `publish` | slack | `draft`（承認フロー、既定）/ `direct`（承認なしで即投稿）。#548 / [ADR-0057](/decisions/adr-0057-per-workflow-publish-and-cleanup.md)。読めない値は**起動時エラー**（承認ゲートを外したつもりのタイポが黙らない） |

### `reaction` — 絵文字でワークフローを選ぶ（#396）

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }     # :hammer: を本人が付けたら実装タスク
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"                  # メンション: catch-all。必ず最後
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- 絵文字名は Slack が報告する形（コロン無し）の**文字列**。`":eyes:"` と書いても剥がされる。👀 は `eyes`、👁 は `eye` で別物
- **同一絵文字を 2 つの workflow に書くと `CONFIG_INVALID`**。first-match で片方が黙って勝つのを許さない
- **リアクションを持たない workflow（= メンション）が 2 つあっても `CONFIG_INVALID`**（#554）。同じ理由
- 本人限定の不変条件は不変（他人のリアクションでは起動しない、→ [ADR-0025](/decisions/adr-0025-reaction-task-trigger.md)）

**定義順の危険は #554 で消えた。** 以前は「リアクション workflow を catch-all より前に書け」という制約があり、後ろに置くと絵文字が無反応になった。これは Orchestrator が 1 本のリストを first-match していたことに由来する。今は Slack プラグインが判定し、**メンションとリアクションは別のイベント経路**なので、順序で隠れることがない。`reaction` の値が文字列でないときの「逆方向に 2 つ壊れる」状態も同様に消えた —— core にはもう `reaction` という語彙が無い。

## `initial_prompt` — ワークフローごとの前置き指示（#415）

```toml
[[workflows]]
name = "github-design"
source = "github"
trigger = { status = "Design" }
profile = "design"
agent = "herdr"
on_success = { status = "Design Review" }
initial_prompt = "/grill-me スキルを使用して、詳細設計を行ってください"
```

これ以前、エージェントへの追加指示は **Slack ソースの `reply_instructions` / `implement_instructions` しか手段が無く**、GitHub / Notion ソースには存在しなかった。しかもソース単位でワークフロー単位ではなかった。`initial_prompt` はその穴を、プラグインに手を入れずに塞ぐ。

| 性質 | 内容 |
|---|---|
| **可視** | pane に見える形で入る。タスクの進め方を丸ごと変えうる指示なので、後から「なぜこの動きをしたのか」を追えるようにする。不可視の `TOTSUKA_PROMPT_CONTEXT` は「requester に届く成果物に混ぜたくないもの」専用で、中身は変えていない |
| **先頭** | タスク本文の**前**に置かれる（`{initial_prompt}\n\n{従来の extra_context}`）。位置に選択肢は無く、herdr が `{extra_context}\n\n---\n{task_body}` を組み立てる |
| **新規会話のみ** | resume ディスパッチ（スレッド返信・retry の会話継続）では入らない。`/grill-me` のような**開始宣言**は 3 ターン目に再入力されるとスキルが再起動して文脈を壊す。resume 非対応ツールは毎回が新規会話なので毎回入る（正しい） |
| **リテラル** | `template::render` を通さない。`{` はそのまま書ける（JSON 例・コード断片） |
| **未設定なら現状と同一** | 空文字列・空白のみは「未設定」と同じ扱いで無言で無視。設定していないワークフローの `extra_context` はバイト同一 |

**無人ハングは設定した運用者の責任。** `AskUserQuestion` のように人間へ問いかけるツールを使わせる指示を書くと、無人 pane では Stop すら発火せず（ツール応答待ちで停止）、`timeout_secs` で Escalated になる。core は但し書きを自動で足さない — 足すと `initial_prompt` に書いた内容と矛盾する指示が混ざりうるため。

`rubric`（下記）とは**別レイヤ**。あちらは llm 検収の判定条件の置換で、プレースホルダ検査に照らして厳格に検証される。`initial_prompt` は指示の上乗せで、置換対象が存在せず、送り先も違う。

## `profile` — 4 原型（#394、[ADR-0033](/decisions/adr-0033-workflow-profile.md)）

`mode` / `output` / `verification` の噛み合う組み合わせに名前を付けたもの。解決テーブルは Rust 固定で、設定側からは原型名を選ぶだけになる（deny セットのような権限に関わる決定を設定文字列から到達可能にしないため — [ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) と同じ理由）。

| profile | mode | output | verification | 想定用途 |
|---|---|---|---|---|
| `answer` | `plan` | `source` | `llm` | 質問に答え、ソースへ返信する |
| `triage` | `plan` | `source` | `llm` | 依頼を GitHub / Notion へ起票する |
| `design` | `plan` | `none` | `llm` | 詳細設計を issue コメント / ページへ書く |
| `implement` | `implement` | `none` | `llm` | 実装して PR を出す |

```toml
[[workflows]]
name = "gh-design"
source = "github"
trigger = { status = "設計待ち" }
profile = "design"
agent = "herdr"
on_success = { status = "設計済み" }
```

併用の規則:

| 構成 | 結果 |
|---|---|
| `profile` + `mode` / `verification` | **エラー**。profile が決める値なので、書くと「生きて見える死んだ設定」が残る |
| `profile` + `output` | **可**。`output` が profile の値に勝つ。権限ではなく配線先の選択なので上書きを許している（Slack 起点の implement が PR URL をスレッドへ返すのに要る） |
| `profile` 無し + `mode` / `output` の欠落 | **エラー**。`profile` を書くか、両方を明示するか |
| `profile` + `rubric` / `tool` / `timeout_secs` / `on_start` / `on_success` / `on_failure` | 可 |
| `status`（`on_start` / `on_success` / `on_failure`）が作る**列の閉路** | **エラー**（#556 → #565 で一般化）。列を節点・書き戻しを辺とするグラフに閉路があると、**人間が 1 人も挟まらないまま永久に再実行され続ける**（毎周エージェントが起動して実費が出る）。自分のトリガー列へ書き戻す構成はその長さ 1 の場合。エラー文は実際の経路を名指しする。直し方は「どのワークフローもトリガーにしていない列を 1 hop 挟む」（人がそこからカードを動かす）。検査は**同一 `source` 内・字面の一致のみ** — 列名がたまたま同じだけの別のボードは閉路ではなく、`source` がそれを分けている |

`profile` は必須ではない。4 原型で表せない組み合わせ（例: `verification = "human"` — 4 原型はいずれも `llm` に解決する）は明示記法で書く。

**ロールバック時の注意**: `profile` を書いた config は旧バイナリでは未知キーとして**パースエラー**になる。totsuka を前のバージョンへ戻すときは config も戻すこと。

**profile が追加で決めるもの**（#394 の時点では mode/output/verification だけだった）:

| 追加された挙動 | 対象 profile | issue |
|---|---|---|
| claude の `--settings` へ `permissions.deny` を注入 | answer / triage / design | #395 |
| `Bash` を**ツールごと** deny（コマンドを 1 つも実行できない） | answer | #410 |
| claude の `--permission-mode plan` を**渡さない** | answer / triage / design | #410 / #409 |
| worktree がブランチ上にあったら**成功として扱わず失敗**させる（走行中の検知では pane も閉じる） | answer / triage / design | #409 / #410 |
| claude の `--settings` へ `permissions.defaultMode = "auto"` を注入 | 全 profile | #420 |
| ソースプラグインへ `instructions_kind` を伝え、書き込み先の指示を出させる | triage / design / implement | #398 |
| 検収 rubric を「成果物 URL の実在」に差し替え | triage | #398（design / implement は #440 で下記の承認検収へ移行） |
| 完了自己申告の指示を「先に NEEDS_INPUT で人間に確認を求め、pane 上の明示承認後にのみ COMPLETED」版に差し替え | design / implement | #440 |
| 検収 rubric を「人間が会話上で完了を明示承認済みか」に差し替え | design / implement | #440 |
| ソースプラグインへ `task_id_prefix` を伝え、会話とは別 ID のタスクを立てさせる | implement (`impl:`) / triage (`books:`) | #397 |
| 必要な外部ツール（`gh`）の不在を dispatch 前に検知して待機させる | implement | #399 |

### design / implement の完了は人間が pane 上で承認する（#440）

`design` / `implement` は attended pane（人間が pane を見ている）前提の profile で、**完了の最終判断は人間が行う**（[ADR-0043](/decisions/adr-0043-human-approved-completion.md)）。エージェントへの完了自己申告の指示が差し替わり、次の流れになる:

1. エージェントは作業を終えたと思ったら `COMPLETED` を**出さず**、内容を要約して確認を求め、`NEEDS_INPUT reason="awaiting completion confirmation"` で停止する。**この reason は運用者の目に届く** — `WaitingInput` の通知本文としてそのまま Slack へ出る
2. totsuka はタスクを `waiting_input` に park する（D-03 掃引対象外・並列 slot 解放・notifier 通知 — すべて従来動作）
3. 人間が pane 上で明示的に承認すると、エージェントが `COMPLETED` を出して終端する

llm 検収の rubric も「この完了申告より前の会話で人間が明示的に承認しているか」の条件に差し替わる。ジャッジはセッション内で会話を見られるので、**確認を飛ばして COMPLETED を出したエージェントは、マーカー欠落を止めるのと同じ層でブロックされる**。確認依頼の停止自体は NEEDS_INPUT なので non-claim 枝（#389）を満たし、ブロックされない。

長い自走中の誤エスカレートを避けたい attended workflow は `timeout_secs = 0`（D-03 掃引のオプトアウト、#439 / [ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md)）を併用する。

既知の制限: `WaitingInput` 中の 2 回目の NEEDS_INPUT（修正指示 → 再確認）は冪等 no-op で**再通知が飛ばない**。attended pane では人間が会話の当事者なので実害は小さい（質問ツール経路は下記のとおり #487 で解消）。

#### 訊き方は質問ツールの選択 UI（#487）

上の流れの「確認を求める」部分は、ツールに native の質問ツールがあればそれを使う（[ADR-0050](/decisions/adr-0050-question-tool-asking.md)）:

- **claude**: `AskUserQuestion`（単一選択ピッカー。「Approve completion / Request changes」等）。ダイアログ待機中はターンが終わらず `NEEDS_INPUT` が届かないため、design / implement の `--settings` にだけ描画される PreToolUse フック（`on-ask-user-question.sh`）が `QuestionPending` イベントを送り、totsuka はそれで従来どおり `waiting_input` へ park する（通知本文は質問文の要約）
- **opencode**: native の `question` ツール（同じ指示が visible extra_context で届く）。`totsuka-opencode.js` の `tool.execute.before` が同じ `QuestionPending` を送り、ダイアログ待機中の idle を UNKNOWN と誤判定しないようガードする
- **codex**: 質問ツールが Default mode に無い（`request_user_input` は Plan Mode 限定）ため従来どおり `NEEDS_INPUT` で停止するが、選択肢を**番号付きリスト**で提示し、人間は番号 1 文字で回答できる

質問ツールが使えない・失敗した場合のフォールバックは常に「番号付きリスト + `NEEDS_INPUT`」で、プロンプト自体に含まれている。park 中に**新しい**質問が来た場合、質問経路では再通知される（NEEDS_INPUT 経路の既知の制限を解消）。

### 成果物 URL 検収の落とし穴（#398）

`triage` の検収 rubric は「最終メッセージに成果物（issue コメント / Notion ページ / PR）の URL が実際に含まれているか」を条件にする。この profile は `result/publish` を通らないので、**この URL が「成果物がどこかに存在する」ことを Orchestrator が知る唯一の経路**である（`design` / `implement` も #440 までは同じ URL 検収だったが、人間承認検収へ移行した — 人間が成果物を見て承認しているのに URL を要求し直すのは二重検収になるため）。

rubric の優先順位（強い順）: `[[workflows]].rubric` > **profile の既定**（triage = URL 検収 #398、design / implement = 承認検収 #440）> 汎用既定。

**#465 より前は、この上にグローバルな段があった。** `[prompts].verification_rubric` を設定済みの構成は `triage` workflow でも URL 検収にならず、`[prompts].marker_self_report` を設定済みだと `design` / `implement` が確認プロトコルにならなかった。症状はどちらも「投稿していない設計を『書いた』と申告したタスクが通る」方向 — つまり検収が緩くなる方向 — なので、梯子の順序を入れ替えるのではなく**グローバルな段そのものを削除**した。今この既定に勝てるのは同じワークフローの `rubric` だけである。

### 外部ツールの未整備で待機する（#399）

`profile = "implement"` のタスクは PR を作るので `gh` が要る。**未整備なら dispatch されず `Queued` のまま待機**し、通知が一度出る。整備すれば数分以内（検査結果のキャッシュ TTL）に自分で流れ出すので、操作は不要。

通知は流れて消えるので、`totsuka status` にも待機理由が出る（#407、[ADR-0037](/decisions/adr-0037-task-notes-in-the-event-log.md)）:

```text
not starting yet:
  task 12 (2026-08-11T09:00:00Z): gh unavailable in the orchestrator's environment → …
```

`--json` では該当タスクの `wait_reason`（`kind` / `since` / `message`）に入る。待機していないタスクにはキーごと出ない。**表示は Orchestrator が記録した内容で、`totsuka status` はツールを再検査しない** — status はオペレータのシェルで走るので、そこで `gh` が見えても Orchestrator から見えているとは限らないため。表示は dispatch できた時点で自動的に消えるが、**`totsuka run` が止まっている間に環境を直しても消えない**（次に `run` が回ったときに消える）。

`totsuka doctor` に `agent-tool:gh` の行が出る（必要とする workflow がある構成でのみ）。

**この検査は間違うことがある。** 判定は totsuka のプロセスで走り、エージェントは pane（`.zshenv` / mise が効いた環境）で走るので、**pane からしか `gh` が見えない構成では「無い」と判定されます**。そのため:

- doctor は `fail` ではなく **`warn`**（exit code を動かさない）
- dispatch は失敗させず**待機**（偽陰性でもタスクは消えない）

心当たりがあればこの警告は無視して構いません。

**検査しない範囲**: `triage` / `design` も外部へ書きますが、**どこへ**書くか（GitHub の `gh` か Notion MCP か）は source 依存で、totsuka はプラグインのインスタンス名からそれを判別できません。推測して誤ると動いたはずのタスクを止めるので、検査しません。doctor はその旨を `agent-tool:external-write` の skip 行で明示します。

**検査は「設定してあるか」だけ**で、`gh auth status` は実行しません（dispatch 経路にネットワーク呼び出しを持ち込まないため）。期限切れトークンは通ってしまい、従来どおり pane で失敗します。

### リアクションで実装タスクを起こす（#397）

Slack の「質問 → 方針決定 → 実装」は、**実行中タスクの権限を広げるのではなく、本人のリアクションで別タスクを起こす**。

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
output = "source"                 # PR の URL をスレッドへ返すため（profile 既定は none）
agent = "herdr"

[[workflows]]
name = "slack-reply"              # catch-all。必ず最後
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- タスク ID は `impl:{channel}:{反応したメッセージの ts}`。スレッドの `answer` タスク（`{channel}:{thread_ts}`）と衝突しない
- **文脈のスコープは反応した位置で決まる**: スレッドの先頭（またはスレッド外の単独メッセージ）→ **スレッド全会話**、スレッド内の返信 1 つ → **そのメッセージのみ**
- リポジトリは会話から継承する（`answer` タスクが解決済みならそれを使い、LLM 呼び出しもピッカーも走らない）
- 報告は**承認ゲートを通る**。実装報告こそ誤送信の影響が大きい

**制限**: 親リアクションはスレッド全体を文脈にするが、`conversations.replies` の取得は **200 件でクランプ**している。それを超えるスレッドは古い方から欠ける（ページング未実装）。

**注意**: `answer` タスクが実行中のうちに `:hammer:` を付けると 2 つのタスクが並走する。別 worktree なので壊れないが、「方針が決まる前に実装が始まる」ことになる。

### ソースプラグインの `[prompts]`（#398）

`[github.prompts]` / `[notion.prompts]` が増えた。profile が `instructions_kind` を伝えたときに、そのプラグインがタスクへ載せる書き込み先の指示文。

| キー | 使われるとき | プレースホルダ |
|---|---|---|
| `triage_instructions` | `profile = "triage"` | github: `{issue_number}` `{repo}` / notion: `{page_url}` `{title}` |
| `design_instructions` | `profile = "design"` | 同上 |
| `implement_instructions` | `profile = "implement"` | 同上 |

いずれも省略可（埋め込みの既定を使う）。**profile を使わない構成ではこのキー群は一切使われず、タスクの `instructions` は従来どおり空**になる。

Slack ソースは同じ `instructions_kind` を読んで自前の 3 キー（`reply_instructions` / `implement_instructions` / `triage_instructions`）から選ぶ（#450）。**選択は kind であって task-id 接頭辞ではない** — `triage` と `implement` は**どちらも接頭辞を持つ**（`books` / `impl`）ので、接頭辞で分岐すると triage のタスクに実装指示が渡る。kind が不明・不在のときは成果物を推測せず `reply_instructions` に縮退する。

**`profile = "design"` を Slack ソースに書くと何も起きない。** `design` は現行コアが送る kind だが Slack プラグインは対応する指示文を持たず、しかも `design` の `output` は `none` なので下書きも publish されない — 返信案の指示を受けたエージェントが動いて、結果がどこにも出ない。設定は検証を通ってしまうので、プラグインは `initialize` 後の dispatch 時に警告ログを出す（#450）。Slack 起点で起票させたいなら `triage` を使う。

**組み込みの既定文は英語で、言語を名指ししない**（[ADR-0054](/decisions/adr-0054-prompt-language-policy.md)）。
成果物の言語は「スレッド / 元 issue / 元ページと同じ言語で書け」という規則でエージェントに決めさせている。
自分で上書きするときも**言語名を書かないほうがよい** — 書くと、エージェント側の設定と元メッセージの言語の
両方を上書きすることになる。特定の言語を強制したいときだけ明示する。

なお `[slack]` の `body_template` などタスク**本文**のラベルは日本語のままである。
ペインでそれを読むのは人間だからで、指示文（英語）とは別の判断になっている。

展開はシングルパス — issue タイトルや Notion ページ名は他人が書ける内容なので、そこに書かれた `{placeholder}` は文字列として挿入されるだけで指示にはならない。

## `mode = "plan"` は git を構造的には止めない（#378）

F-82 は plan を「worktree は作るが push・PR は行わない」モードとして定義しており、
実装も `--permission-mode plan` / `--sandbox read-only` / `bash: deny` がそれを担保する前提で
書かれてきた。**実機ではその前提が破れた** — plan モードのタスクがブランチを切り、コミットし、
push し、PR まで作成した。対象リポジトリの `CLAUDE.md` が「終わったら push して PR を作れ」と
指示していたためである。**claude の `--permission-mode plan` に至っては、`permissionMode` が
`plan` のままファイルが書かれた実測がある**（#410）ので、書き込みを止める機構として数えないこと。

**素の `mode = "plan"`（profile 無し）は今も検出だけ**である。worktree にブランチが現れると
`run` が警告を出す（ブランチ名つき）。既存の構成がアップグレードで黙って厳しくならないよう、
ここは意図的に警告のままにしてある。

**profile を書いた workflow は失敗する**（#409、[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md)）。なお **read-only profile の read-only 性は保証ではない**（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md)）— OS レベルで封じるサンドボックスは実現可能と実測済みだが、実装しないと決めた。`cat >` でのファイル書き込みや `&&`・パイプを挟んだ git/gh は deny を素通りする。
read-only profile（answer / triage / design）のタスクの worktree がブランチ上にあると、成果物を
公開せず `fail_publish` で失敗し、worktree とコミットは調査用に保持される。**これは防止ではない** —
ブランチがある時点で push は済んでいるかもしれず取り返せない。失敗させることで「黙って成功」を
避けているだけである。復帰するには worktree を detach してから `totsuka task retry`（そのままの
retry は同じ検査で再び落ちる）か、`totsuka task cancel`。

副作用の無いモードとして plan を選ぶ場合は、いずれにせよ**対象リポジトリの規約に
push / PR を指示する記述が無いか**を確認すること。

# `[tools.{name}]`（AI ツールレジストリ、#196）

pane 内で起動する AI ツール CLI の定義。`{name}` は `default_tool` / `[[repositories]].tool` / `[[workflows]].tool` から参照する任意の名前。組み込み既定として `claude` / `codex`（#196 Phase 2）/ `opencode`（#196 Phase 3）が常に存在し、同名エントリで上書きできる。同一 kind の別プロファイル（例 `claude-fast`）も定義可能。**全 kind が dispatch 可能**（アダプタ無し kind の validate 拒否は将来の kind 追加に備えて残置）。

`kind = "codex"` の利用には一回きりのセットアップ（hooks trust・対象リポジトリ trust）が必要 → [Codex ツールのセットアップと hooks trust 運用](/operations/codex-tool-setup.md)。`kind = "opencode"` はアセット自動配置のみで trust 不要だが縮退が多い → [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md)。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `kind` | enum | 必須 | アダプタ種別: `claude` / `codex` / `opencode`。argv 組立と完了検知方式を決める |
| `command` | string? | kind 名 | 空白区切りのコマンドライン。先頭 = プログラム、残り = 基本引数（例 `"claude --model haiku"`） |
| `mode_args` | string[]? | kind 既定 | implement モードで追加する引数（codex 既定: `["--sandbox", "workspace-write", "--ask-for-approval", "never"]`、opencode 既定: `["--auto"]`、claude 既定: なし） |
| `plan_args` | string[]? | kind 既定 | plan モードで追加する引数（claude 既定: `["--permission-mode", "plan"]`、codex 既定: `["--sandbox", "read-only", "--ask-for-approval", "never"]` — plan permission mode 不在の縮退、opencode 既定: `["--agent", "totsuka-plan", "--auto"]` — 全 deny の plan エージェント） |

kind ごとの argv 組立の差分: claude はフック設定を `--settings <path>` で受け、resume は `--resume <id>` フラグ。codex はフックがグローバル登録（`~/.codex/hooks.json`、`TOTSUKA_*` env でゲート）のため `--settings` 相当は付かず、resume は `resume <id>` **サブコマンド**（基本引数の直後・モード引数の前に挿入）。 opencode もグローバル配置の JS プラグイン（env ゲート）で完了検知するため `--settings` 相当は無く、resume は `-s <id>` フラグ。opencode は不可視注入が無いため、タスク指示 + マーカー規約は**可視の extra_context** として pane に渡る。

## モデルと推論強度の指定

**`[tools.{name}]` に `model` / `effort` の専用キーは無い。** 受け付けるのは上表の 4 キーだけで、`ToolConfig` は `deny_unknown_fields` なので書くと設定のパース時点で落ちる:

```text
unknown field `model`, expected one of `kind`, `command`, `mode_args`, `plan_args`
```

モデルと推論強度は、**ツール CLI 自身のフラグとして `command` に書く**。

```toml
[tools.claude-fast]
kind = "claude"
command = "claude --model haiku --effort low"

[tools.claude-deep]
kind = "claude"
command = "claude --model opus --effort high"
```

綴りは totsuka の抽象ではなく**ツール CLI のもの**なので kind ごとに違う。以下は実測（claude 2.1.233 / codex 0.145.0 / opencode 1.18.4）。**totsuka が起動するのは対話 CLI**（`command` 既定は kind 名そのもの）なので、非対話サブコマンド（`codex exec` / `opencode run`）にしか無いフラグは使えない。

| kind | モデル | 推論強度 |
|---|---|---|
| claude | `--model <alias\|full-name>` | `--effort <low\|medium\|high\|xhigh\|max>` |
| codex | `-m, --model <MODEL>` | `-c model_reasoning_effort=<value>`（専用フラグは無い） |
| opencode | `-m, --model <provider/model>` | **対話 CLI では指定できない**（下記） |

codex の `model_reasoning_effort` は `-c` による設定上書きなので、**CLI 側は値を検証しない**。不正な値でも起動は通り、最初のリクエストで API がエラーを返す（実測: `bogusvalue` を渡すと `Supported values are: 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', and 'max'.`）。

opencode の推論強度は `--variant` だが、**これは `opencode run`（非対話）のフラグで、totsuka が起動する対話 TUI には無い**。代わりに opencode 側の `opencode.json` でエージェントに `variant` を設定することになるが、公式スキーマはこれを「そのエージェントに**設定されたモデルを使うときにのみ**適用される」と定義しているため、`command` に `-m` を書くと効かない可能性がある（未実測）。モデルと variant は片方だけ totsuka 側に置かず、どちらで指定するかを揃えること。

### workflow ごとに切り替える

ツール解決は workflow ピン > repo 既定 > `default_tool` > 組み込み `claude` の順（後述）なので、レジストリに複数のプロファイルを置いて `[[workflows]].tool` で選ぶ:

```toml
[[workflows]]
name = "triage"
tool = "claude-fast"

[[workflows]]
name = "implement"
tool = "claude-deep"
```

### `mode_args` / `plan_args` には書かない

この 2 つは kind 既定を**丸ごと置き換える**（後述）。`plan_args = ["--effort", "low"]` と書くと claude 既定の `["--permission-mode", "plan"]` が消え、plan モードの構造的な境界が外れる。モードに依らない起動オプションは `command` 側に置くこと。

### `command` はシェルではない

`command` は `split_whitespace()` で分割されるだけで、シェル的なクォートは解釈されない。したがって**空白を含む単一引数は `command` に書けない**。必要な場合は配列である `mode_args` / `plan_args` を使うことになるが、その場合は上記のとおり kind 既定を自分で書き足す必要がある。

## 承認プロンプトで止まらないこと（#420）

**pane には答える人が居ない**ので、3 ツールとも「人間に確認を求めない」設定で起動する。綴りはツールごとに違うが、意図は同じである。

| ツール | 綴り | どこで |
|---|---|---|
| claude | `permissions.defaultMode = "auto"` | `--settings` のファイル（profile がある workflow のみ） |
| codex | `--ask-for-approval never` | plan / implement 両方の既定 argv |
| opencode | `--auto` | plan / implement 両方の既定 argv |

**これは「エージェントにできることを広げる」設定ではない。** 境界はそれぞれ別の機構が持っており、この設定はそれを緩めない:

- claude の `deny` は**どの permission mode でも適用される**ので、profile の deny セットは `auto` でも同じ強さで効く
- codex の `--sandbox` は承認ポリシーとは**別のフラグ**である（両方まとめて捨てる `--dangerously-bypass-approvals-and-sandbox` が第 3 のフラグとして存在するのが、独立している証拠）
- opencode の `--auto` は CLI 自身が「**explicitly denied を除いて**自動承認する」と説明しており、plan エージェント `totsuka-plan` の `edit/bash/task: deny` はそのまま残る

変わるのは、**境界が拒否しないもの**に対して人間に聞くかどうかだけである。

放っておくとどうなるかは実測してある: 何も設定していないマシンの claude は `default`（CLI 表示は `manual`）で起動し、allowlist に無い Bash コマンドの手前で `Do you want to proceed?` を出したまま動かなくなる。codex は `on-request`（モデルが必要と判断したら聞く）、opencode は `doom_loop` / `external_directory` が `ask` である。

`mode_args` / `plan_args` を明示すると**既定を丸ごと置き換える**ので、これらのフラグも消える。無人で回すなら自分で書き足すこと。

ツール解決はディスパッチ時に workflow ピン > repo 既定 > `default_tool` > 組み込み `claude` の順。解決結果は core が完全な argv/env（`ToolLaunchSpec`）へ組み立てて agent プラグインに渡す。かつて後方互換フォールバックだった herdr.toml の `agent_command` / `plan_args` は、**プロトコル 0.4.0 で削除済み**（#411、[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。`deny_unknown_fields` なので**残っていると `initialize` が `CONFIG_INVALID`** になり、`removed_keys_in` がキー名と代替を名指しする。

# プロンプト文（組み込み、#314 → #465）

claude / codex / opencode に差し込むプロンプト文は
`crates/orchestrator-core/src/prompts/defaults.toml` にバイナリ埋め込みされており、
**設定から上書きできない**。上書きできるのは llm 検収の判定基準文だけで、綴りは
`[[workflows]].rubric` である。

`[prompts]`（グローバル 8 キー）と `[[workflows]].prompts`（7 キー）は #314 で入り
[#465](https://github.com/tomoya-k31/totsuka/issues/465) で削除された
（[ADR-0023 の Amendment](/decisions/adr-0023-configurable-prompt-surface.md)）。
理由は 2 つ:

- **グローバルなキーを 1 つ書くだけで、後から入った profile の検収が黙って無効化された。**
  `[prompts].verification_rubric` を設定していると `triage` が URL 検収にならず、
  `[prompts].marker_self_report` を設定していると `design` / `implement` が確認プロトコルに
  ならない。どちらも症状が「検収が緩くなる」方向、つまり**気づきにくい方向**へ倒れる
- **設計が追い越した。** `[prompts]` の後に入った 3 キー（`verification_nonclaim_exemption` /
  `verification_rubric_artifact_url` / `marker_self_report_confirm`・`verification_rubric_human_approval`）
  は最初から設定不可で、profile が選ぶものだった

**まだ書いてある config は起動しない。** キーごとに何になったかを名指しするエラーで落ちる:

```text
[prompts] sets `verification_rubric`, which was removed in favour of built-in
prompt text → write the criteria as `rubric` on the workflow itself — the one
prompt key that survived
```

| 消えたキー | 代わりに |
|---|---|
| `verification_rubric` | `[[workflows]].rubric` に書く |
| `marker_self_report` | 代替なし。完了プロトコルは workflow の `profile` が選ぶ（design / implement は人間確認版）。上書きはまさにそれを打ち消していた |
| `branch_convention` | 代替なし。ブランチ規約はエージェントが対象リポジトリから読む（[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)） |
| `verification_prompt` / `verification_marker_convention` / `verification_background_exemption` / `verification_nonclaim_exemption` | 代替なし。判定プロンプトの組み立て方は組み込みで、そのうち運用者のものだったのが `rubric` である |
| `opencode_plan_agent` | 代替なし。opencode plan エージェントの散文は組み込み（`permission` の deny マップは元々設定不可） |

**プロンプト文の変更にはリビルドが要る。** #314 はまさにそれを避けるために入ったので、
これは意図的な巻き戻しである。前提が変わった — プロンプト文は運用者が触るチューニング対象
ではなく、profile と検収機構に結びついた**動作の一部**だと分かった。

## `rubric` の書き方

`[[workflows]].rubric` は判定プロンプトの**枝の 1 つ**に入る。組み立て後の全体はこうなる
（`{...}` の位置に組み込みの各枝が入る）:

```text
This stop may be allowed. That is, at least one of the following holds:

{nonclaim_exemption}      ← 最終メッセージが NEEDS_INPUT / FAILED を報告している停止（#389）
{background_exemption}    ← バックグラウンドタスク実行中の中間停止（ハートビート）
{rubric}                  ← ここに `rubric` が入る

{marker_convention}       ← ok: false のとき reason に何を書かせるか
```

**組み込みのプロンプト文は英語である**（#465）。`rubric` に何語で書くかは自由で、
実運用では日本語の `rubric` が英語の枠に入る形になる — これは運用者の文字列であって
不整合ではない。

> **`rubric` は「命令」ではなく「条件」である。** Claude Code は `prompt` 型フックの本文を
> 固定のシステムプロンプト配下でモデルに渡し、`{"ok": true|false, "reason": "..."}` を返させる
> （本体 2.1.224 で確認）。`ok: true` で停止が通り、`ok: false` がブロックで `reason` が
> エージェントへ差し戻される。**モデルはブロックを制御していないので、「ブロックせず許可して
> ください」と書いても効かない。** #389 でその形を一度出荷し、実機でジャッジが当該文言を
> 逐語引用しながら 8 回連続で `ok: false` を返した。ここに書くテキストは、**許可してよい全
> ケースで真になる条件**として書くこと。

`rubric` は `verification = "llm"` のワークフローでのみ使われる（prompt 型 Stop フックを
持つのは claude だけで、他ツールでは `human` へ縮退する）。他の verification に設定すると警告。

**マーカー自体（`<<STATUS:COMPLETED>>` など）は設定できない。** `on-stop.sh`（bash）と
`totsuka-opencode.js` がリテラルをパースし、[ADR-0020](/decisions/adr-0020-status-marker-stays.md)
が 3 ツール共通の唯一の完了信号と定めているため。

## 優先順位

強い順に 3 層。

1. `[[workflows]].rubric`
2. **profile の既定**（triage = 成果物 URL 検収 #398、design / implement = 人間承認検収 #440）
3. 組み込みデフォルト

#465 より前はこの上に `[[workflows]].prompts.verification_rubric` が、2 と 3 の間に
グローバルの `[prompts].verification_rubric` が挟まっていた。**消えたのは 2 を飛び越せる段**である。

## 展開規則

- `rubric` に**プレースホルダは書けない**。`{name}` の形は検証エラーになる — 枝はまず単独で
  レンダリングされ、そのあと組み立てが `{rubric}` を埋めるので、枝の中の名前には解決先が無く
  そのまま文字列として出荷される
- プレースホルダ名は識別子（`[A-Za-z_][A-Za-z0-9_]*`）に限られる。それ以外の波括弧は**中身**として
  素通しされるので、`{"ok": true}` のような JSON の形を `rubric` に書いてよい（#328）
- 波括弧の中にさらに `{` があると、その範囲全体が 1 つの未知の名前として素通しされる。この形は
  警告として報告される
- なお `[worktree]` の `location` は置換方式が異なる（`str::replace` 連鎖）ため、**波括弧の中身は
  identifier に限らずすべて検査される**。`{repo-name}` のようなタイポはエラーのままである
- 組み立ては 2 段階（枝 → 全体）で各段シングルパスなので、`rubric` に書いたリテラル
  `{marker_convention}` は挿入されるだけで展開されない
- `rubric` の変更は**次のディスパッチから有効**。稼働中セッションの `--settings` は書き換わるが、
  既に起動しているエージェントには届かない

## 例

```toml
[[workflows]]
name = "slack-reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"
rubric = "返信案が質問に直接答えているか、根拠が示されているかを検証してください。"
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
| `location` | string? | `<state dir>/worktrees/{repo_name}/{worktree_name}` | 配置テンプレート。`{repo}`/`{repo_name}`/`{worktree_name}`/`{task_id}`/`{source}`/`${ENV}`/`~` を展開。`{worktree_name}` は `{source}-{task_id}` を git ref 規則で正規化して `/` を潰したもの。**`{branch}` は廃止** — ブランチは worktree ができた後にエージェントが決めるので、作成時点のディレクトリ名には使えない。残っていると設定エラーで起動しない |
| `cleanup` | policy? | `manual` | implement モードの掃除ポリシー（F-23） |
| `plan_cleanup` | policy? | `immediate` | plan モードの掃除ポリシー（F-85） |

どちらも **`[[workflows]].cleanup` が書かれていればそちらが勝つ**（#548）。ここの 2 キーは mode で選ばれる既定であり、workflow 単位の例外は workflow 行に書く。

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

# `[hooks]`

Claude Code フックイベント受信（UDS）の設定（#131。全キー省略可、`deny_unknown_fields`）。値の実使用は UDS サーバ・フックスクリプト側の issue（#136/#137）で配線される。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `auth_token_ref` | string? | なし | フック POST を認証する Bearer トークンのシークレット参照（E-03、例 `op://Dev/totsuka/hook-token`）。**運用上は必須**（未設定時の防御は 0600 の UDS パーミッションのみ）。未設定は #209 でツール側が検出するようになった: フック対応 agent（マニフェストが `hook_completion` を宣言）を使う workflow がある場合、`config validate` / `run` が該当 workflow ごとに警告を出し、`doctor` は **fail**（終了コード非 0）。フック対応 agent を使わない構成では doctor は warn 表示のみ（終了コードは成功）。参照を設定したのに解決できない場合は構成によらず fail |
| `socket_path` | string? | 組み込み既定 | 受信 UDS のパス（例 `${XDG_RUNTIME_DIR}/totsuka/agent-events.sock`） |
| `spool_dir` | string? | 組み込み既定 | POST 失敗時にイベントを退避するスプールディレクトリ（E-07、例 `${XDG_STATE_HOME}/totsuka/hooks/spool`） |
| `block_retry_limit` | int? | 3 | Stop フック block 差し戻しの連続上限。超過でエスカレーション（D-02） |

# `[github]`（task-source-github）

config.toml 側の推奨設定。**ポーリング型の task_source**（もう 1 つは下の `[notion]`）で、`poll_interval_secs` がそのままプラグイン内部の fetch 周期になる（0.6.0 / #554 で `[plugins.github]` から `[github]` へ移動。task-source-slack はイベント駆動でこの値を使わない）:

```toml
[plugins.github]
enabled = true
kind = "task_source"

[github]
poll_interval_secs = 60   # 省略時も 60。`0` は警告を出して 60 へフォールバックする
```

`[github]` の全キー（`deny_unknown_fields`。**未知キーは `initialize` の硬い失敗になる**ので、タイポは起動時に分かる）:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `token` | string | 必須 | API トークン（オーケストレータが解決して渡す、F-65）。プラグインは bearer として送る以外に触らない。必要な権限は [task-source-github](/components/task-source-github.md) を参照。`cmd:gh auth token` が使える（[ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)） |
| `status_field` | string | `Status` | ステータス列を保持する SingleSelect フィールド名（F-02）。**全ボード共通** |
| `github_login` | string | 必須 | 自分のログイン名。自己アサインされたタスクの検出（F-08）と、claim の self-assign 先（#556）に使う。**1 login = 1 インスタンス**: 同じ login で複数の totsuka を動かすと claim の裁定が原理的にできない（assignee にはログイン名しか載らない）ため非対応 |
| `in_progress_statuses` | string[] | `[]` | 「進行中」とみなして ingest から除外するステータス名（F-08）。**全ボード共通** |
| `source_name` | string | `github` | `Task.source` に刻印するソース名。ボードを増やしても変わらない（だから `[[workflows]].source = "github"` は 1 本のまま） |
| `api_url` | string | `https://api.github.com/graphql` | GraphQL エンドポイント（GitHub Enterprise / テスト用の上書き） |
| `claim_verify_delay_ms` | int? | `750` | claim（#556）の self-assign 書き込みから読み戻しまでの待ち ms。読み戻しが競合と黙殺の両方を検出するので、API に反映される前に読んではいけない。既定値は実測（p95 ≈ 700ms / max 983ms）に基づく。`0` も有効（テスト用。早すぎる読みは再試行 1 回を足すだけ） |
| `max_retries` | int | 3 | リトライ可能な API 失敗の最大再試行回数 |
| `[prompts]` | テーブル | — | このプラグインが送るプロンプト文の上書き（下記、#398） |

ボードは `[github]` ではなく **Orchestrator の `[[projects]]`** に書く（#554）。`source = "github"` の要素が、そのプラグインのボードになる:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | core のキー。`[[repositories]].project` が指す |
| `source` | string | 必須 | core のキー。`"github"` |
| `owner` | string | 必須 | Project の所有者ログイン |
| `owner_type` | enum | `user` | `user` / `organization` |
| `project_number` | int | 必須 | 所有者配下の ProjectsV2 番号 |
| `triage_status` | string? | なし | triage 起票時に付ける Status。**省略すると Status なしで追加される**（人間のトリアージゲートが残る）。polling trigger と同じ値を書くとそのゲートは消え、起票が即・無人実装へ流れる |

`owner` / `owner_type` / `project_number` / `triage_status` は github のキーなので `deny_unknown_fields` で検査される（`name` / `source` は core が読むので届かない）。

```toml
[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"
owner_type = "user"
project_number = 6
triage_status = "📥 Inbox"

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"
```

## リポジトリの紐付けは `[[repositories]].project` から derive される（#554）

以前は `[[projects]].repos` に書き、それが役割を 2 つ兼ねていた:

1. **ingest フィルタ**: そのボードに載っていても、ここに無いリポジトリの issue は取り込まれない
2. **リポジトリ → ボードの順方向マッピング**: `initialize` の応答（`claimed_repos`）でオーケストレータへ渡り、Slack 発などの triage タスクが「どのボードへ起票するか」を知る材料になる

**この 2 つは今も分けられないが、正本が 1 箇所になった。** `[[repositories]].project` がボードを名指し、プラグインは `initialize` でその紐付けを受け取って両方を導出する。

得られたもの:

- **1 つのリポジトリを 2 つのボードに書けない。** `project` はスカラー 1 つなので、書けない状態になった。以前は `config/validate` が検出する対象だった（ボードの同一性は `(owner, project_number)` で見る必要があった —— ProjectsV2 の番号は所有者ごとなので、番号だけで比べると `me/#7` と `acme/#7` が同じに見えて重複を見逃す）
- **github と notion をまたぐ重複も書けない。** 以前はプラグインからは見えず、オーケストレータの起動時 warn と `doctor` で検出していた。その機構（`ClaimConflict`）は削除した
- `repos` と `[[repositories]].name` を一致させる運用上の前提が消えた

## `project_number` の誤りは起動時には出ない

`project_number` が 0 や負でも **`initialize` は成功する**。正数を要求する検査は `config/validate` の側にしかなく、`initialize` は serde のデシリアライズが通れば起動する。

結果として症状は「毎 poll で Project が見つからず、**タスクが 1 件も取り込まれない**」になる。起動ログは正常なので、これは一番切り分けにくい壊れ方である。捕まえられるのは `totsuka doctor` と `totsuka config validate` だけなので、設定を書いたらどちらかを通すこと。

（未知キーのほうは対照的に `initialize` の硬い失敗になる。`deny_unknown_fields` は serde の層で効くため。）

## #542 / #554 より前の設定は起動しない

トップレベルの `owner` / `owner_type` / `project_number` / `repos` は #542 で `[[projects]]` エントリの中へ移り、**#554 でその `[[projects]]` 自体が `[github]` から出て Orchestrator のトップレベルへ移った**。`deny_unknown_fields` なので、どちらの旧形式も `initialize` の硬い失敗になる（serde が `unknown field` と言う）。

**移行案内は実装していない。** #542 の時点で totsuka はまだ非公開で、設定ファイルは実運用 1 本と live-e2e 1 本しか存在せず、どちらも同じ作業で書き換えた。#554 も同じ理由で同じ扱いにしている。案内のコードを維持する相手がいない。

#554 の書き換えで手が要るのは 2 点:

- `[github].projects` を消し、トップレベルの `[[projects]]` に `name` と `source = "github"` を足して書き直す
- 各要素の `repos` を消し、代わりに `[[repositories]]` の側に `project = "<name>"` を書く

## `token` に必要な権限

**十分条件は実測済み、最小値は未実測**（#514、2026-08-23）。呼んでいるのは `https://api.github.com/graphql` への 4 操作だけ — Project アイテム取得、Project/フィールド/アイテムの id 解決、`updateProjectV2ItemFieldValue`、`viewer`。REST も Contents API も使わない。**Issue への書き込みは無い**（#398 で `result/publish` とともに `addComment` が消えた）。実測の内容と根拠は [task-source-github](/components/task-source-github.md)。

**まず種別を選ぶ。ここを間違えると権限の表を読んでも解決しない**:

| Project の所有者 | 使えるトークン |
|---|---|
| **org** 所有 | fine-grained PAT（Organization permissions の Projects）または classic PAT |
| **user** 所有 | **scope を持つトークン**（classic PAT の `project`、または `gh auth token` の OAuth トークン）。fine-grained PAT は Account permissions に **Projects が存在しない**ので到達できない — 効くのはトークンの呼び名ではなく scope 方式かどうかである |

**fine-grained PAT**（org 所有ボードのみ。未実測）:

| 種別 | 権限 |
|---|---|
| Repository | **Metadata: Read**（必須） |
| Repository | **Issues: Read**（write は不要） |
| Organization | **Projects: Read and write**（Organization permissions にしか無い） |

**Contents は不要。** classic PAT なら `project` と、`repo`（private を含む場合）または `public_repo`。private org のボードでは `read:org` も要りうる。

実測できているのは「OAuth トークン（scope `gist, project, read:org, repo, workflow`）で 4 操作すべてが通る」まで。**どれが要らないかは測っていない** — とくに Issue の本文・ラベル・アサイニーは `projectV2` 経由でしか読まないので、`project` だけで返るなら `repo` は不要である。確かめ方は `bash .claude/skills/live-e2e/scripts/github-permissions.sh probe --write`。

**PR 作成はこのトークンの仕事ではない。** `gh pr create` を実行するのはエージェント自身で、ペインの環境にあるあなた自身の `gh` 認証を使う。`gh auth login` は別個の前提条件である。

## `[prompts]`（task-source-github、#398）

組み込みデフォルトは `plugins/task-source-github/src/defaults.toml` にバイナリ埋め込みされており、このテーブルは**キー単位の上書き**である（未指定キーは組み込みのまま）。**キー名がそのまま設定キー**である。

| キー | 用途 |
|---|---|
| `triage_instructions` | ワークフローの profile が `triage` のとき送られる |
| `design_instructions` | 同 `design` |
| `implement_instructions` | 同 `implement` |

# `[notion]`（task-source-notion）

github と並ぶポーリング型 task_source。`poll_interval_secs` がそのままプラグイン内部の fetch 周期になる。

```toml
[plugins.notion]
enabled = true
kind = "task_source"

[notion]
token = "cmd:ntn auth token --plain"
notion_user_id = "8f2c…"                 # 自分（省略すると自己アサイン検知が無効）
property_map = { title = "名前", status = "ステータス", assignee = "担当者" }
in_progress_statuses = ["実装中"]
```

`[notion]` の全キー（`deny_unknown_fields`。**未知キーは `initialize` の硬い失敗になる**ので、タイポは起動時に分かる）:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `token` | string | 必須 | Notion のトークン（オーケストレータが解決して渡す、F-65）。プラグインは bearer として送る以外に触らない。**インテグレーションシークレットのほか、公式 CLI（`ntn`）のログイントークンも使える** —— `cmd:ntn auth token --plain`（[ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)）。プラグインが検証に使うのと同じ `GET /v1/users/me` に `Notion-Version: 2022-06-28` 付きで投げて 200 を返すことを実測済み。`ntn` は資格情報を macOS Keychain（service `notion-cli`、account は workspace id）か、`NOTION_KEYRING=0` のときは `~/.config/notion/auth.json` に持つが、**保存先を直接参照するよりこのコマンド経由が良い**（保存形式は `ntn` の内部都合で変わりうる）。ただし CLI 由来のときは 3 点注意する。**(1) 出力は `ntn` の既定 workspace に従う** —— 別の workspace へ `ntn login` すると、config.toml を 1 文字も変えないまま token が入れ替わり、`database_id` に届かなくなる。固定するなら `cmd:NOTION_WORKSPACE_ID=<workspace id> ntn auth token --plain` と書く。**(2) `cmd:` の解決は起動時 1 回きり**なので、CLI のログインセッションが失効するとポーリングが 401 を吐き続ける（`totsuka run` の再起動で復帰する）。失効しない値が要るならインテグレーションシークレットを `op://` に置く。**(3) 401 のときに確認するのは workspace とページの可視性である** —— CLI トークンに「integration をデータベースに共有」という設定は無い。エラーメッセージは token の種類ごとに次の一手を出し分ける。 |
| `notion_user_id` | string? | なし | 自分の Notion user id。`trigger.assignee` の `@me` がこれと突き合わせる。**省略すると `@me` が誰にも一致しない** —— 既定のトリガー（`["@me", "@none"]`）は「未アサインのタスクだけ取り込む」になり、`@me` を明示したワークフローは `initialize` で落ちる（#572）。github の `github_login` が必須なのと**非対称**なので注意。**さらに `property_map.assignee` が未設定だと、この「未アサインだけ」も成立しない** —— assignee を読む先が無いので全ページが未アサインに見え、**assignee による絞り込みが消えてデータベースの全ページが取り込まれる**（既定のトリガーは明示されていないので警告もエラーも出ない）。assignee でタスクを分けている DB では、`property_map.assignee` を必ずマップすること。挙動そのものをどうするかは #582 |
| `property_map` | テーブル | 下記 | 共通スキーマ ↔ Notion のプロパティ名の対応（F-03） |
| `body_source` | enum | `none` | 本文の取得元。`none` / `property`（`property_map.body` の `rich_text`）/ `page`（ページ本文のブロックを Markdown 化） |
| `in_progress_statuses` | string[] | `[]` | 「進行中」とみなして ingest から除外するステータス option 名（F-08）。**全データベース共通** |
| `priority_map` | テーブル | `{}` | 優先度の option 名 → 数値。大きいほど先に走る。`number` 型の優先度プロパティはこの表を無視して値をそのまま使う |
| `source_name` | string | `notion` | `Task.source` に刻印するソース名 |
| `api_url` | string | `https://api.notion.com/v1` | REST のベース URL（テスト用の上書き） |
| `api_version` | string | `2022-06-28` | `Notion-Version` ヘッダに送る API 版 |
| `max_retries` | int | 3 | リトライ可能な API 失敗の最大再試行回数 |
| `poll_interval_secs` | int? | 60 | fetch 周期。**`0` はポーリングを止めない** —— ビジースピンになるので警告を 1 行出して既定の 60 秒へフォールバックする。止めたいならワークフローを消すか `[plugins.notion] enabled = false` にする（github も同じ挙動）。**同じファイルの `[[workflows]].timeout_secs` は `0` が opt-out を意味するので、逆である** |
| `rate_limit_rps` | int | 3 | クライアント側のリクエスト毎秒上限。Notion の公開上限が約 3 rps なのでそれに合わせてある |
| `[prompts]` | テーブル | — | このプラグインが送るプロンプト文の上書き（下記） |

## `property_map` — 共通スキーマ ↔ Notion のプロパティ名（F-03）

**`title` だけが必須**で、未設定の任意フィールドは単に抽出されない。これにより単一のプラグインで任意の DB 構造を正規化できる。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `title` | string | `Name` | タイトルを持つプロパティ名（Notion の既定が `Name`） |
| `status` | string? | なし | ステータスを持つプロパティ名。`trigger.status` と `on_*.status` の両方がこれを読み書きする |
| `status_kind` | enum | `status` | `status`（Notion 専用のステータス型）/ `select`。書き戻しの本体形状と option 解決を切り替える |
| `assignee` | string? | なし | assignee を持つ `people` プロパティ名。**`trigger.assignee` を書くなら必須**（未設定だと全ページが未アサインに見え、条件が何もしなくなるので `initialize` で落ちる、#572） **未設定のまま `trigger.assignee` を書かない場合も無害ではない** —— 既定のトリガーから見ると全ページが未アサインなので、assignee による絞り込みが丸ごと消える（上の `notion_user_id` の行を参照） |
| `priority` | string? | なし | 優先度を持つ `number` / `select` / `status` プロパティ名 |
| `repo_hint` | string? | なし | リポジトリのヒントを持つ `rich_text` / `select` / `url` プロパティ名（F-10） |
| `body` | string? | なし | `body_source = "property"` のときに本文を読む `rich_text` プロパティ名 |

**`config/validate` は全データベースを見る。** `property_map` は全 DB 共通なので、あるデータベースだけがマップ先プロパティを欠いていると**そこ由来のタスクだけが壊れる** —— 1 つ目だけ見て緑にするのが一番静かな壊れ方になる。

データベースは `[notion]` ではなく **Orchestrator の `[[projects]]`** に書く（#554）。`source = "notion"` の要素がそのプラグインのデータベースになる:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | core のキー。`[[repositories]].project` が指す |
| `source` | string | 必須 | core のキー。`"notion"` |
| `database_id` | string | 必須 | 対象データベースの id |
| `triage_status` | string? | なし | triage 起票時に付ける Status option 名。**省略すると Status なしで作成される**（人間のトリアージゲートが残る）。polling trigger と同じ値を書くとそのゲートは消え、起票が即・無人実装へ流れる。`property_map.status` が未設定のまま書くと `config/validate` がエラーにする（埋める列を名指しできない指示文になるため）—— **ただしこの検査は `initialize` では走らない**ので、未 validate の設定は起動し、status 指示が黙って落ちるだけになる |

```toml
[[projects]]
name = "design-db"
source = "notion"
database_id = "…"

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "design-db"
```

## 再実行はできない（#573）

**notion のタスクは、どのトリガーでも 1 回しか実行されない。** 配送に lane identity が無い（`message_key` が常に空）ので、ステータスを戻しても再配送は重複として捨てられる。github は**`trigger.status` を持つワークフローに限り**ステータスセルの更新時刻を刻むので差し戻しで再実行できる（`label` 単独・`assignee` 単独のトリガーは github でも at-most-once である）。Notion API にはプロパティ単位の更新時刻が無いので、同じ方法が取れない。

したがって **`trigger.status` を足しても再実行可能にはならない**。この非対称は [ADR-0064](/decisions/adr-0064-notion-at-most-once.md) で決定として確定させた（#573）。

## `[prompts]`（task-source-notion、#398）

組み込みデフォルトは `plugins/task-source-notion/src/defaults.toml` にバイナリ埋め込みされており、このテーブルは**キー単位の上書き**である（未指定キーは組み込みのまま）。**キー名がそのまま設定キー**である。

| キー | 用途 | プレースホルダ |
|---|---|---|
| `triage_instructions` | ワークフローの profile が `triage` のとき送られる | `{page_url}` `{title}` |
| `design_instructions` | 同 `design` | `{page_url}` `{title}` |
| `implement_instructions` | 同 `implement` | `{page_url}` `{title}` |

# `[slack]`（task-source-slack）

config.toml 側の推奨設定。task-source-slack は Socket Mode で受けたイベントを即座に `task/submit` で push するイベント駆動ソースで、`poll_interval_secs` は使わない（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)。旧: プラグイン内バッファに積み `tasks/fetch` で吸い上げていたため短周期ポーリングを推奨していたが、#187 の push 移行で不要になった）:

```toml
[plugins.slack]
enabled = true
kind = "task_source"
```

`[slack]` の全キー（`deny_unknown_fields`。導入手順は [Slack セットアップ Quickstart](/operations/slack-quickstart.md)、トークンの扱いは [取り扱いポリシー](/security/slack-user-token.md)）:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `app_token` | string | 必須 | App-Level Token（`xapp-`、Socket Mode 用）。`op://` 参照推奨 |
| `user_token` | string | 必須 | User OAuth Token（`xoxp-`、本人名義の読み書き）。`op://` 参照推奨 |
| `bot_token` | string? | なし | Bot User OAuth Token（`xoxb-`、[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)・#305）。設定すると返信案・ピッカー到着時に bot が本人へ通知 DM（ナッジ）を送る。**未設定なら機能 off**（起動時 warn 1 回）。設定時は TokenGuard が `auth.test` で probe。`op://` 参照推奨 |
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
| `reply_instructions` | 返信案作成の指示（`Task.instructions` として帯域外配送される）。**profile 既定**: `answer`、および kind 不明・不在のときのフォールバック。**このキーは `answer` の既定であると同時に、このプラグインが指示文を持たない kind のフォールバック**でもある（`design` を指定した Slack workflow、profile 無しの workflow）。そのため**ツールの可否を主張してはいけない** — `answer` はファイル編集もシェルも拒否されるが、`design` はどちらも拒否されず、profile 無しは deny が 1 つも付かない。書けるのは「このタスクの成果物は何か」だけである。変更・コミット・PR 作成を求める文を書くと、`answer` のエージェントが試みて失敗する（#527。返信が 1 つも出ないまま `FAILED` になった） | — |
| `implement_instructions` | **profile 既定**: `implement`。実装して PR を作り、その URL を報告文に含めさせる | — |
| `triage_instructions` | **profile 既定**: `triage`（`:books:` の起票フロー、#450）。Issue を起票し、その URL を報告文に含めさせる | — |
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
- 未知のプレースホルダはそのまま出力され、`initialize` 時に警告としてログに出る。**エラーにしないのは意図的である**（このプラグインは `config/validate` フックを持っているのでエラーにもできる）— 未知キーはそのまま描画されるので症状はドラフト中に見える `{token}` である。core 側の `rubric` がこれをエラーにするのは、あちらが llm 検収の判定条件で、壊れた症状が「検収が緩くなる」だけだからである。
- ここは **LLM 向けのプロンプトのみ**である。悪い上書きは分類の劣化（スレッド内ピッカーへフォールバックする）や返信案の質低下に留まり、完了検知は壊せない。この危険度の違いが、core 側の上書き面を #465 で削除しつつ**このテーブルを残した**理由である。

# `[herdr]`（agent-ide-herdr）

全キー（`deny_unknown_fields`。ネストした `[layout]` にも効く）:

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `socket_path` | string? | なし | herdr ソケットの明示パス。解決順の最上位 |
| `session` | string? | なし | 名前付きセッション（`~/.config/herdr/sessions/{name}/herdr.sock` に解決）。`socket_path` 未設定時に使う |
| `[layout]` | テーブル | 下記の既定 | dispatch した pane の配置（#356、下記） |
| `[kind_map]` | テーブル | `{}` | 実行ファイル名 → herdr の `kind` の写像（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-1、下記） |
| `[identity]` | テーブル | `{ enabled = true }` | dispatch が「どのリポジトリの・どのタスクか」を herdr へ報告するか（#417、[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md)、下記） |
| `request_timeout_secs` | int | 30 | herdr socket 呼び出し 1 本あたりのタイムアウト |

ソケットの解決順: `socket_path` > `session` 名 > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定 `~/.config/herdr/herdr.sock`。

**herdr は 0.7.5 以降が必要。** それより古い herdr に対しては `initialize` が
`CONFIG_INVALID` で初期化を拒否し、`totsuka config validate` / `doctor` がバージョンを名指しで報告する
（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-6）。判定は **`ping` の `version` の semver
比較**で、`protocol` 整数は見ない（#520）。**上限は無い** — 新しい herdr を拒否することはない。

## `[herdr.identity]` — サイドバーに出す identity の報告（#417）

```toml
[herdr.identity]
enabled = true   # 既定
```

dispatch が `workspace.create` の直後（`agent.start` の**前**）に、workspace と root pane の**両方**へ
metadata token を報告する。`$name` の解決先が spaces 行では workspace、agents 行では pane なので、
片方だけでは片方のパネルしか直らない。

| token | 値 |
|---|---|
| `totsuka_task` | `Task.id` を**そのまま**（**表示しない**機械識別子で、比較に使うので整形も切り詰めもしない。herdr の上限 80 文字に収まらない id は**送らない** — 切れた識別子は無い識別子より悪く、label 経路が正しいフォールバックになる） |
| `repo` | `[[repositories]].name`（プロトコル 0.4.1 未満のオーケストレータからは届かないので省かれる） |
| `task` | タスクのタイトル（**表示用**なので空白を畳んで 79 文字 + `…`） |
| `mode` | `plan` / `implement` |

**サイドバーの行構成は totsuka が書き換えない。** `~/.config/herdr/config.toml` は herdr と運用者のもので、
貼るスニペットは [herdr サイドバーに repo / タスクを出す](/operations/herdr-sidebar-setup.md)、
書ける値は [herdr サイドバー設定](/references/herdr-sidebar-config.md) にある
（[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D6）。
**スニペットを入れていない環境では、報告しても行は増えない**（label だけは変わる）。

報告が**両方成功したときだけ**、workspace の label を `{repo}: {タイトル}` に rename する（D4）。
`workspace.create` が書く `totsuka {task.id}` は #417 以前とバイト同一なので、
所有マーカーは workspace の最初の瞬間から存在し、rename に失敗しても機械 label が残る。
`repo_name` が無ければ rename しない。

**報告の失敗は dispatch を落とさない**（`tracing::warn!` のみ）。identity は装飾で、
herdr が一瞬詰まっただけで走れるタスクを失うほうが高くつく。

`enabled = false` で報告が止まり、**#417 以前と完全に同じ挙動に戻る**。

## `[herdr.kind_map]`（実行ファイル名 → herdr の `kind`）

protocol 17 の `agent.start` は**実行ファイルを `kind`（21 値の enum）から決める**ため、プラグインは
`[tools]` が解決した `program` をそのまま起動できず、**ファイル名**を herdr の語彙へ翻訳する。
`claude` / `codex` / `opencode` はそのまま通るので、通常このテーブルは要らない。

必要になるのは**ラッパースクリプト**のように herdr が知らない名前のときだけ:

```toml
[herdr.kind_map]
my-claude = "claude"
```

- キーは**ファイル名**と比較する（パスではない）。`/opt/bin/my-claude` は `my-claude` で引く
- 値の検証はしない。未知の `kind` は herdr が `agent.start` で拒否する。21 値の enum をこちら側に
  複製すると、上流が増やしたときに黙って食い違うため
- `[tools]` レジストリ側には置かない。`[tools]` は agent_ide 非依存の共有設定で、herdr 固有の語彙を
  そこへ持ち込むと orca しか使わない構成にも漏れる

## `[herdr.layout]`（pane の配置、#356）

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `shell` | bool | `true` | 併設シェル pane を出すか。`false` ならエージェント全画面になり、`direction` / `ratio` は無視される |
| `direction` | `"down"` \| `"right"` | `"down"` | 分割方向。herdr の `SplitDirection` そのままで、**他の値は `initialize` でエラー**（`up` / `left` は herdr に存在しない） |
| `ratio` | float | `0.8` | **エージェント側**の取り分。**範囲検査はせず** herdr へそのまま送る |

- 既定は「エージェントを上 80% / シェルを下 20%」。#356 以前は herdr の既定（右分割 0.5）が漏れており、
  エージェント pane の幅は実測 123 桁だった。**この変更で既存ユーザの画面は変わる**（視覚のみ。データ・フロー・完了検知に影響なし）。
- **併設シェルには hook 環境変数が渡らない**（`TOTSUKA_HOOK_TOKEN` を含む）。人間が直接叩くシェルに
  ベアラトークンを常駐させないため（[hook のセキュリティ](/security/hook-security.md)）。エージェント pane には従来どおり載る。
- **レイアウト適用の失敗は dispatch を落とさない**。警告を出して続行し、シェルなし（またはherdr の既定配置）に落ちる。
  `ratio` が不正で herdr が拒否した場合もこの経路になる。

# 例

`[Spec §4.6/§4.9](/product/orchestrator-spec.ja.md)` の例が `totsuka init` の雛形にコメントアウトで含まれる。設計→実装ハンドオフの典型:

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { status = "Ready to design" }
profile = "design"
agent = "herdr"
on_success = { status = "Design review" }

[[workflows]]
name = "implement"
source = "github"
trigger = { status = "Ready to implement" }
profile = "implement"
agent = "herdr"
on_success = { status = "In review" }
```
