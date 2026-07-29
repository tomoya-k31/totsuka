---
type: Library
title: plugin-sdk クレート
description: task_source プラグイン作成用のヘルパークレート。単一 writer タスクの stdio ランタイム・JSON-RPC dispatch ボイラープレート（TaskSourceHandler）・task/submit クライアント（バックオフ再送）・ポーリング型ソース向け poll_loop を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-sdk
tags: [rust, crate, plugin, sdk, task-source, push]
generated: { by: human:tomoya-k31, at: 2026-07-26T05:00:00+09:00 }
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
| `poll` | **`poll_loop`**: `InitializeParams.triggers` × `poll_interval_secs` の fetch→submit タイマー（github/notion がプラグイン内部でこの周期を使う唯一の取り込み経路。旧 `tasks/fetch` RPC は 0.2.0 で削除済み）。tick は非重複、間隔は ±10% jitter（SplitMix64、rand 依存なし）。fetch 失敗はその tick のみスキップ。dedup は Orchestrator 側 `duplicate` ack に委譲し seen-set を持たない |

# 利用パターン

- **イベント駆動ソース（slack 型）**: `runtime::stdio()` → パイプラインに `SubmitClient` の clone を渡してイベント→`submit_task()`、`serve(handler, &stdio)` で host リクエストに応答。 会話継続ソースは submit の前に `LookupClient` で既知判定し、既知なら新規会話向けの解決（LLM 呼び出し・リポジトリ選択 UI）を省く。`serve()` は全 response 行を `submit` / `lookup` 両クライアントへ渡し、各自が発行していない id を無視する（id 接頭辞 `submit-` / `lookup-` で分離）。
- **ポーリングソース（github/notion 型）**: `initialize` で受けた `triggers`/`poll_interval_secs` を `poll_loop(triggers, interval, submit, fetch_fn)` に渡して spawn。

# 依存

- `plugin-protocol` / `serde` / `serde_json` / `tokio`（io-std）/ `tracing`

# 関連

- [ADR-0008 task/submit による push 型タスク取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [plugin-protocol クレート](/components/plugin-protocol.md)
- [プラグイン開発ガイド](/development/plugin-dev-guide.md)
