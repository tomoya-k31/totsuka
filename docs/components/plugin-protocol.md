---
type: Component
title: plugin-protocol クレート
description: プラグイン開発者向けに公開する型定義クレート。JSON-RPC 2.0（NDJSON）エンベロープ・plugin.toml マニフェスト・capabilities・§11 メソッド型・Task 共通スキーマ・プロトコルバージョニングを提供する、プラグイン境界の単一の正。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol
tags: [rust, crate, plugin, protocol, json-rpc]
timestamp: 2026-07-19T12:00:00Z
status: active
owner: tomoya-k31
---

# 責務

Orchestrator と別プロセスのプラグイン（`task_source` / `agent_ide` / `notifier`）が JSON-RPC 2.0 over stdio でやり取りするための公開型定義。**プラグイン境界の単一の正**であり、プラグイン開発者はこのクレートに依存して実装する。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `jsonrpc` | JSON-RPC 2.0 の Request / Response / Notification / Error。フレーミングは **NDJSON**（1 行 1 メッセージ、`to_line` で符号化）。標準＋独自エラーコード（protocol version mismatch / capability 未対応 / config 不正） |
| `manifest` | `plugin.toml`（name / kind / version / protocol_version(semver 要件) / capabilities）。`PluginKind`、`Capabilities`（plan_mode / design_preview / pane_control / state_stream / outputs に加え、0.1.3 で **resume_session** / **diagnostics_snapshot** を追加。構造体レベル `serde(default)` なので未宣言のプラグイン（orca / mock 等）は自動 false） |
| `task` | Task 共通スキーマ（F-01: id/source/title/body/repo_hint/labels/priority/status/url/assignee）。0.1.3 で **`thread_key: Option<String>`** を追加（会話継続の相関キー。Slack は `"{channel}:{thread_ts}"`、同一 thread_key の後続タスクは先行タスクのセッションを resume できる。他ソースは None）。0.1.5 で **`instructions: Option<String>`** を追加（task_source 所有のエージェント向け指示文。人間可視の `body` から分離し、ホストが帯域外配送（不可視プロンプト注入）できるようにする。理解しないエージェントには単に不在に見える） |
| `methods` | §11 各メソッドの params/result 型と `method::*` 名前定数。共通（initialize/shutdown/config·validate）、task_source（tasks·fetch/task·update_status/result·publish）、agent_ide（task·dispatch/task·cancel/session·attach/state·subscribe→notification）、notifier（notify）。`NotifyParams` は event/task_id/**workflow**（任意・ワークフロー別フィルタ用 F-92, #62 で追加）/title/body。`InitializeParams` は protocol_version / config に加え **`repositories: Vec<RepoInfo>`**（#109、0.1.1 で追加。orchestrator の `[[repositories]]` を task_source プラグインへ供給し、ソース側リポジトリ解決の設定重複を解消。`serde(default)` + 空なら省略 = 追加的変更で、使わないプラグインは無視するだけ。task_source 以外へは常に空）と **`llm: Option<LlmInfo>`**（#119、0.1.2 で追加。orchestrator の `[llm]` を task_source プラグインへ分類用 **default** として供給 — base_url / model / 解決済み api_key（F-65）。プラグイン自身の LLM 設定が常に優先（default + override）。契約は `repositories` と同じ: 未設定なら省略・使わないプラグインは無視・task_source 以外へは常に `None`）。**0.1.3（#132）の追加**: `TaskDispatchParams` に `job_id`（Orchestrator 発番の相関キー。plugin が env `TOTSUKA_JOB_ID` として注入）/ `resume_session_id`（`claude --resume <id>` 用のエージェント側ネイティブ session id。Slack スレッド会話継続用）/ `hook: Option<HookLaunchSpec>`（`--settings <settings_path>` の付与と env 注入の起動仕様。フック機構の知識は Orchestrator 側に閉じ、plugin は内容を解釈しない）。新規 RPC **`diagnostics/snapshot`**（O→P、R-10: タイムアウト診断用のペイン画面スナップショット。取得不能は `text: None` でエラーにしない）。`NotifierEvent` に **`escalated`**（block 超過/timeout/相関異常）/ **`verification_pending`**（human 検収待ち）を追加（旧 notifier はデシリアライズに失敗するが notify は fire-and-forget（F-93）のため通知欠落 + エラーログに留まる）。**0.1.4（#155）の追加**: 新規 RPC **`session/focus`**（O→P、F-94: 通知 click-to-focus。session_id の pane 復号はプラグイン内に閉じる（F-37）。pane 消失は `focused: false` でエラーにしない）。capability は新設せず既存 **`pane_control`** 宣言プラグインにのみ送る（[ADR-0005](/decisions/adr-0005-click-to-focus.md)） |
| `version` | `PROTOCOL_VERSION`（アプリ本体と独立、§10.2）と互換判定 `is_compatible`（F-54）。現行 **0.1.5** |

# 責務境界（F-86）

`agent_ide` プラグインの成果は**コミットまで**。push / PR 作成は **Orchestrator** の責務であり、プラグインは push・PR 作成を行わない。`task/dispatch` の doc comment に明記。

# バージョニング（§10.2）

プロトコルは独立した SemVer（`PROTOCOL_VERSION`）を持つ。プラグインは manifest で対応範囲を semver 要件として宣言し、Orchestrator は範囲外のプラグインを拒否する（破壊的変更でメジャーを上げ旧世代を 1 世代サポート）。0.x 世代では caret 意味論（`^0.1` = `<0.2.0`）に合わせ、**後方互換の追加はパッチレベル**で刻む（例: 0.1.1 の `InitializeParams.repositories`、0.1.2 の `InitializeParams.llm`、0.1.3 のフック起動・resume・診断スナップショット一式（#132）、0.1.4 の `session/focus`（#155）、0.1.5 の `Task.instructions`。0.2 に上げると全プラグインの `^0.1` manifest が採用不要の変更で弾かれてしまうため）。追加を前提とするプラグインは `>=0.1.5, <0.2` のように宣言できる。

# 依存

- `serde` / `serde_json` / `toml` / `semver` / `thiserror`

# 関連

- [Spec §4.6 プラグインシステム / §11 プロトコル最小メソッドセット](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
