---
type: Library
title: plugin-sdk クレート
description: task_source プラグイン作成用のヘルパークレート。単一 writer タスクの stdio ランタイム・JSON-RPC dispatch ボイラープレート（TaskSourceHandler）・task/submit クライアント（バックオフ再送）・ポーリング型ソース向け poll_loop・trigger キーの未知検査・trigger.assignee 条件の解釈を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-sdk
tags: [rust, crate, plugin, sdk, task-source, push]
generated: { by: claude-code/opus-5, at: 2026-08-27T05:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

サードパーティが task_source プラグインを実装する際の共通機構（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。作者はソース固有ロジック（イベント受信 / API フェッチ / Task 変換）だけを書けばよい。**範囲外**: HTTP クライアント・LLM ヘルパー・config スキーマ（ソース固有のまま）。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `runtime` | stdio NDJSON ランタイム。**単一 writer タスク（mpsc）が stdout を専有**し、返信行とバックグラウンドの `task/submit` リクエスト行が部分行で交錯しないことを構造的に保証（従来の read ループ内 inline 書き込みの恒久修正）。`serve()` は response 行（`id` + result/error、`method` なし）を `SubmitClient` へ、それ以外を `LineHandler` へ配路。`Writer::from_channel` でテスト/カスタムトランスポートにも載る |
| `dispatch` | `Reply` / `request_id` / `parse_params` と、型付き **`TaskSourceHandler`** trait（initialize / config_validate / update_status / result_publish）。`TaskSourceServer` が trait を `LineHandler` に変換し、PARSE_ERROR・notification 無応答・shutdown・METHOD_NOT_FOUND を含む wire protocol 全体を実装。**0.2.0（#190）**: `tasks_fetch` は trait・dispatch とも削除済み — 全 task_source は push（`task/submit`）専用 |
| `submit` | **`SubmitClient`**: `task/submit` を送り persist-before-ack の結果を待つ。ack 3 値（`accepted`/`duplicate`/`rejected`）は**最終**（再送しない）。JSON-RPC error（`NOT_ACCEPTING`/`SUBMIT_OVERLOADED`/`INTERNAL_ERROR`）・writer 喪失・ack timeout（30s）は指数バックオフ（1s→…→30s、最大 5 回）で再送 — submit は冪等なので再送は常に安全（ack 喪失後の再送は `duplicate` で吸収）。5 回で `GaveUp`（ソースシステムが durable origin なので恒久喪失なし）。clone 共有の pending map を `serve()` が解決 |
| `lookup` | **`LookupClient`**: `task/lookup` を送り「この会話は既知か / どのリポジトリか」を得る（0.2.4、#242）。**失敗はエラー条件ではない** — `submit` と違い最終的に通す必要がなく、タイムアウトやエラーは単に「答えが無い」なので、**リトライもバックオフもしない**（1 回・タイムアウト・`Lookup::Unknown`）。再試行しても呼び出し側が同じフォールバックを待たされるだけ。`Lookup::{Known{repo}, New, Unknown{reason}}` の 3 値で、`skips_resolution()` が true になるのは `Known` のみ — **未応答を「既知」と読むと会話がリポジトリ無しでディスパッチされる**ため、`Unknown` は必ず false。orchestrator はエンジンループで応答するので `git fetch` 等で数秒待たされうる（タイムアウト前提の設計） |
| `assignee` | **`AssigneeFilter`** / **`check(...)`**（#572）: `trigger.assignee` の条件（`@me` / `@none` / `@any` / login / 配列の OR）を解釈し、タスクの assignee 一覧と突き合わせる。**省略時の既定は `["@me", "@none"]`** で、これは #572 以前のプラグイン全体のゲート（F-08）と同一 —— つまりこれは旧ゲートの**置き換え**であって前段ではない。二重ゲートにすると `assignee = "teammate"` のような「書けるのに効かない」設定が作れてしまうため、経路を 1 本にしてある。特殊語に `@` を付けるのは衝突回避で、`me` / `none` / `any` はどれも実在しうるログイン名である。`check` は `initialize` 用で、**評価不能な条件を起動時に落とす**（`@me` なのに identity 設定が無い / people プロパティが未マップ）ほか、`status` を伴わない `assignee` 単独トリガーに「1 タスク 1 回になる」warning を返す。**共有しているのはキー名と値の語彙だけ**で、何と突き合わせるか（github は Issue 組み込みの assignee と `github_login`、notion は `property_map.assignee` と `notion_user_id`）は各プラグインが持つ |
| `trigger` | **`unknown_trigger_keys(workflows, valid)`**（#574）: そのソースが読まない `[[workflows]].trigger` キーを 1 件 1 メッセージで返す。呼び出し側は `initialize` でこれを `CONFIG_INVALID` へ倒す。**必要なのは、トリガーの解釈が `.get("…")` だから** —— 誰も読まないキーは黙って捨てられ、条件が 1 つ減る。つまりタイポは trigger を**狭めず広げる**（`assinee` と書くと「条件なし」になり、除外したかったタスクにこそ発火する）。`valid` は呼び出し側がリテラルで渡す（パーサの隣に `TRIGGER_KEYS` として置く規約） —— 導出しないので、キーを足してここを忘れると新しいキーのテストが落ちる。エラー文は有効キー一覧を含み、改名からの移行案内も兼ねる。`trigger = {}`（catch-all、#396）はキーが無いので常に通る |
| `poll` | **`poll_loop`**: `InitializeParams.workflows`（0.6.0 / #554 で `triggers` から改称。`WorkflowInfo` は `workflow` 名も運ぶ）× 各ソースが自分の `[<name>].poll_interval_secs` から読む周期の fetch→submit タイマー（github/notion がプラグイン内部でこの周期を使う唯一の取り込み経路。旧 `tasks/fetch` RPC は 0.2.0 で削除済み）。tick は非重複、間隔は ±10% jitter（SplitMix64、rand 依存なし）。fetch 失敗はその tick のみスキップ。dedup は Orchestrator 側 `duplicate` ack に委譲し seen-set を持たない |

# 利用パターン

- **イベント駆動ソース（slack 型）**: `runtime::stdio()` → パイプラインに `SubmitClient` の clone を渡してイベント→`submit_task(task, workflow)`（**どの `[[workflows]]` に属するかはプラグインが決めて名前で渡す** — 0.6.0 / #554）、`serve(handler, &stdio)` で host リクエストに応答。 会話継続ソースは submit の前に `LookupClient` で既知判定し、既知なら新規会話向けの解決（LLM 呼び出し・リポジトリ選択 UI）を省く。`serve()` は全 response 行を `submit` / `lookup` 両クライアントへ渡し、各自が発行していない id を無視する（id 接頭辞 `submit-` / `lookup-` で分離）。
- **ポーリングソース（github/notion 型）**: `initialize` で受けた `workflows` と、自分の config の `poll_interval_secs`（0.6.0 / #554 で `[<name>]` のキーになり、`InitializeParams` からは消えた）を `poll_loop(workflows, interval, submit, fetch_fn)` に渡して spawn。`fetch_fn` は `WorkflowInfo` を受け取り、その `trigger` の解釈もワークフローの選択もプラグイン側で行う（core に予約語彙は無い、[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）。

# 依存

- `plugin-protocol` / `serde` / `serde_json` / `tokio`（io-std）/ `tracing`

# 関連

- [ADR-0008 task/submit による push 型タスク取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [plugin-protocol クレート](/components/plugin-protocol.md)
- [プラグイン開発ガイド](/development/plugin-dev-guide.md)
