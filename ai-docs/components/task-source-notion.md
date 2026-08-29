---
type: Component
title: task-source-notion プラグイン
description: Notion データベースをタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。プロパティマッピングで任意の DB 構造を Task へ正規化し、ステータス書き戻しとページ本文への結果追記を行う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-notion
tags: [rust, crate, plugin, task-source, notion, rest, property-mapping]
generated: { by: claude-code/opus-5, at: 2026-08-30T02:20:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

Notion データベースを totsuka のタスクソースとして接続する公式プラグイン（F-02/F-03）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。[task-source-github](/components/task-source-github.md) と同じ構造を Notion REST API へ適用したもの。#189（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md) Phase B）で protocol 0.1.6 の **push 型**へ移行 — [plugin-sdk](/components/plugin-sdk.md) の `poll_loop` が `initialize` 供給の workflows を内部 cadence（`[notion].poll_interval_secs`、既定 60s — 0.6.0 / #554 で `[plugins.notion]` から移動）で fetch し、各タスクを `task/submit` で push する。orchestrator 側のポーリングは行われない。

トークンは `initialize` の config で解決済みのものを受領し（F-65）、プラグイン自身は Keychain に触れない。JSON-RPC は stdout、診断ログは stderr（ホストがログへ転送）。GitHub と異なり、任意の DB 構造を扱うため **プロパティマッピング**（F-03）を設定で受け取り、共通 [`Task`](/components/plugin-protocol.md) スキーマ（F-01）へ正規化する。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `[notion]`（= `InitializeParams.config`）を型付け。`token` / `notion_user_id`（F-08 の自己判定）/ `property_map`（title / status(+`status_kind` status\|select) / assignee / priority / repo_hint / body ↔ Notion プロパティ名, F-03）/ `body_source`（none\|property\|page）/ `in_progress_statuses`/ `priority_map`（option 名→数値）/ `source_name` / `api_url` / `api_version` / `max_retries` / `rate_limit_rps`。`deny_unknown_fields`。**データベースはここに無い**（#554）: Orchestrator の `[[projects]]`（`source = "notion"` の要素）から `initialize` で届き、`DatabaseConfig::resolve` が `RepoInfo.project` の紐付けと突き合わせて組み立てる。要素のキーは `database_id` / `triage_status`（`DatabaseOptions`、こちらも `deny_unknown_fields`）。`claimed_repos()` はそこと `property_map` から `initialize` 応答の claim を組み立てる。`triage_status`（任意、#548 派生）を書くと destination の status 列に「set it to `値`」が入る。`property_map.status` 未設定との組は `config/validate` がエラー（埋める列を名指しできない指示になるため）。**この検査は `initialize` では走らない**（`project_number` と同じ分離）— 未 validate の設定は起動し、status 指示が黙って落ちるだけになる |
| `transport` | `NotionTransport` trait（`request(method, path, body, idempotent)`）＋ reqwest 実装 `ReqwestTransport`（bearer 認証・`Notion-Version` ヘッダ固定・タイムアウト・指数バックオフ §5.3・3rps スロットリング）。ロジックを録画レスポンスでテストするための seam。**HTTP ステータス → エラー変種の写像もここが持つ**: 401 → `Unauthorized`、Notion 自身が `code: "object_not_found"` を返した 404 → `ObjectNotFound`、それ以外の失敗 → `Http { status, body }`。404 をコードで絞るのは `api_url` が設定可能で、**base URL の打ち間違いやプロキシ由来の 404 に共有漏れの案内を出さない**ため |
| `blocks` | Notion ブロック ↔ Markdown 変換。読み（`blocks_to_markdown`, ページ本文→body）は主要ブロック型（heading/paragraph/bullet/numbered/to_do/quote/code）対応・未対応型はプレーンテキスト化。書き（`markdown_to_blocks`, F-07）は heading/bullet/quote/paragraph を生成し、2000 文字/リッチテキストの上限で分割（マルチバイト境界安全） |
| `client` | `NotionClient<T: NotionTransport>`。`fetch`（databases query をページング取得→property_map で `Task` 正規化→トリガー絞り込み→取り込み制御 F-08。body=page 時のみ生存タスクのブロックを取得）/ `update_status`（DB スキーマから option を検証、未知 option はエラー→ページ property を PATCH, F-84）/ `publish`（Markdown→blocks 変換、100 件バッチで追記, F-07）/ `validate`（users/me 疎通＋マップ先プロパティ存在確認 F-59） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。`Server::new(factory, SubmitClient)`（#189: SDK の stdio ランタイム[単一 writer タスク]で駆動され、`LineHandler` 実装経由で serve される）。initialize（config 型付け → client 構築 → triggers があれば SDK `poll_loop` を常駐 spawn — 各 tick で全 trigger を fetch し `task/submit` push。triggers 空なら poll なし。`poll_interval_secs = 0` は既定 60s へフォールバック[warn ログ]）/ config·validate / task·update_status / result·publish / shutdown。`tasks/fetch` は **0.2.0（#190）で削除済み** — 未初期化メソッドは拒否。Session drop（re-initialize 含む）で poll タスクを abort。`TransportFactory` で録画トランスポートを注入しテスト **#574: `TRIGGER_KEYS`（`assignee` / `filter` / `status`）と突き合わせ、未知の `trigger` キーがあれば `initialize` を `CONFIG_INVALID` で落とす**（`plugin_sdk::unknown_trigger_keys`）。トリガーの解釈は `.get("…")` なので、読まないキーは黙って捨てられ条件が 1 つ減る —— つまりタイポはトリガーを狭めず**広げる**。一覧は `client` のパーサの隣にリテラルで置き導出しない **#572: `trigger.assignee`** —— `plugin_sdk::check_assignee_triggers` で起動時に検証する。notion は前提が 2 つとも任意設定で、**どちらも欠けると黙って効かなくなる**（people プロパティ未マップ → 全ページが未アサインに見える／`notion_user_id` 未設定 → `@me` が誰にも一致しない）。そのため `assignee` を書いたら `property_map.assignee` は必須、`@me` を含むなら `notion_user_id` も必須として `CONFIG_INVALID` で落とす |
| `main` | SDK stdio ランタイム（`plugin_sdk::runtime::stdio` + `serve`）。`ReqwestFactory` を配線。ログは stderr |

# プロパティマッピング（F-03）

`property_map` で共通スキーマの各フィールド ↔ Notion プロパティ名を対応づける。`title` のみ必須、他は任意（未設定フィールドは抽出しない）。status は `status`/`select` 型の双方に対応（`status_kind` で write-back の本体形状と option 解決を切替）。priority は `number` プロパティを直接、または `select`/`status` の option 名を `priority_map` で数値化。body は property（`rich_text`）またはページ本文ブロック（`body_source`）から取得。これにより単一プラグインで任意の DB 構造を正規化できる。

# 取り込み制御（F-08）

fetch（`poll_loop` の各 tick が呼ぶ `NotionClient::fetch`。0.2.0 で `tasks/fetch` RPC 自体は削除されたが、`poll_loop` 内部からは引き続き使う）は **`[[projects]]` の全データベースを設定順に走査し**（#542）、それぞれについて: まずトリガー（`status` / raw `filter` / `assignee`）で候補を絞る（`status` / `filter` は可能なら databases query の server-side filter で削減）。**assignee もこの trigger の一部である**（#572） —— 誰が持っているタスクを取るかは workflow が決め、省略時の既定 `["@me", "@none"]` が #572 以前のプラグイン全体のゲートと同一になる（自分は `notion_user_id` で判定、未設定時は未 assign のみ取り込み）。旧ゲートは削除済みで、これの後ろには残っていない。次に、**workflow が言わないこと**だけを適用する: `in_progress_statuses` のステータスを除外、**そのデータベースに紐づかないリポジトリ**を除外（紐付けは `[[repositories]].project`、#554）。厳密な排他制御はしない。重複 push は orchestrator が `duplicate` ack で安価に破棄するため、プラグイン側に seen-set は持たない。

**この紐付けによるフィルタは github と非対称で、条件付きである。** GitHub の issue は必ずリポジトリを持つが、Notion のページの `repo_hint` は任意プロパティなので、値が無いページは**そのまま取り込む**（Orchestrator が従来どおり F-11 で解決する）。落としてしまうと、`repo_hint` をマップしていない利用者は 1 件も取り込めなくなる。

**1 データベースの失敗は poll 全体の失敗にする**（github と同じ理由: 飛ばすと「取り込むものが無い」と区別できない）。

**`task/update_status` はページの親データベースを Notion に問い合わせる。** PATCH 先はページ id だけで足りるが、その前に「対象 option がそのデータベースに存在するか」を検証しており、どのデータベースかは request が語らない。ingest 時のメモを先に引き、無ければ `GET /pages/{id}` の `parent.database_id` を読む — **各データベースの option を順に試す方式は採らない**。別のデータベースにだけ存在する option を通してしまい、明確な「unknown status」エラーが Notion API 側の分かりにくい失敗に化けるからである。id はハイフンの有無を無視して突き合わせる（Notion は両形式を受け付け、ハイフン付きで返す）。

**`config/validate` は全データベースを見る。** `property_map` は全データベース共通なので、あるデータベースだけがマップ先プロパティを欠いていると、そこ由来のタスクだけが壊れる — 1 つ目だけ見て緑にするのが一番静かな壊れ方になる。

# 再実行はできない（#573 / [ADR-0064](/decisions/adr-0064-notion-at-most-once.md)）

**notion のタスクは、トリガーが何であれ 1 回しか実行されない。** `normalize_page` は `message_key` を**無条件で `None`** にするので、core はそれを `task.id` にフォールバックさせ、`UNIQUE(task_id, message_key)` が以降の再配送をすべて重複として捨てる。ステータスを戻しても再実行されず、`totsuka task retry` も `done` を拒否する。

これは実装漏れではなく**決定である**（ADR-0064）。github は `trigger.status` を持つワークフローに限り `status` セルの `updatedAt` を lane identity に使えるが（#556。`label` 単独・`assignee` 単独のトリガーは **github でも at-most-once** である）、**Notion API にはプロパティ単位の更新時刻が無い**ので同じものが作れない。却下した代替案とその破れ方は ADR-0064 にある。

# capabilities（F-83）

manifest（`plugins/task-source-notion/plugin.toml`、`protocol_version = ">=0.6.0, <0.7"`）と `initialize` 応答で `kind = task_source` を宣言する。**`outputs` は空**（#398）—— 成果物はエージェントが Notion MCP で自分で書くので、このプラグインは何も publish しない。

# テスト

`NotionTransport` を録画レスポンスの fake に差し替え、initialize→poll_loop→`task/submit` push（SubmitHarness で観測・ack 注入）、property_map 正規化→ページ本文取得→update_status の全経路を JSON-RPC 境界越しに結合テスト（`tests/integration.rs`）。取り込み制御（他者 assignee / 実行中 / トリガー不一致）、triggers 空での no-poll、未知 option の update_status 拒否、トークン無効／マップ先プロパティ欠落時の `config/validate`（原因＋次アクション）を検証。実バイナリを stdio で駆動して疎通確認済み。

ただし **fake は `NotionError` の変種を直接返すので、実 transport が HTTP ステータスをその変種へ写像すること自体は結合テストでは検査できない**（写像を丸ごと消しても全部緑になる）。そこは `transport.rs` のユニットテストが持つ —— `TcpListener` で 1 発だけ応答するサーバを立て、401 / Notion の 404 / Notion 以外の 404 / 500 と、本体のある POST を固定する。さらに**応答しないリスナ**（accept せず backlog に任せる。ソケットを閉じると reset ＝ `Transport` になってしまう）に `with_timeout` を短くした transport を当て、`Timeout` へ写ること・**非冪等では再送しないこと**（適用済みで応答だけ失われた可能性がある）・冪等では再送することを固定する。

# 依存

- `plugin-protocol`（プラグイン境界）、[plugin-sdk](/components/plugin-sdk.md)（stdio ランタイム / `poll_loop` / `SubmitClient`）、`reqwest`（REST）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。GitHub プラグインと同一の依存集合。

# 成果物の書き込み（#398 で非推奨）

`design` / `implement` profile の workflow は `output = "none"` になり、成果物はエージェントが Notion MCP で自分で書く。**`result/publish` の実体は削除済み**（#398）。`blocks.rs` は**読み取り方向だけ**が残った（`blocks_to_markdown` / `rich_text_plain`）—— ADR-0033 は「`blocks.rs` の削除」と書いたが、ページ本文をタスク本体に載せる経路が使い続けているので、消えたのは書き込み方向（`markdown_to_blocks` とその補助）だけである。`answer` / `triage` profile をこのソースで使うには **`output = "none"` を明示する**。代わりに `instructions_kind`（コアが `WorkflowInfo` の専用フィールドで送る。0.6.0 までは trigger に焼き込んでいた）から `[prompts]` の指示文を選び、`Task.instructions` に載せる — これが書き込み先をエージェントへ伝える唯一の経路で、**旧プラグインでは無言で欠落する**（capability 宣言が無いので probe できない。コアと同時にリリースすること）。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [plugin-sdk](/components/plugin-sdk.md)
- [task-source-github](/components/task-source-github.md)
- [ADR-0008 task/submit push 取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [Spec §4.2 タスクソース / F-01・F-03・F-07・F-08・F-84](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
