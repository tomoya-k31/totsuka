---
type: Component
title: agent-ide-herdr プラグイン
description: herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/設計プレビューを担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [rust, crate, plugin, agent-ide, herdr, socket-api, streaming]
timestamp: 2026-07-17T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

herdr を totsuka の Agent IDE として接続する公式プラグイン（F-30〜F-38）。v1 の参照実装。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、「**Orchestrator 側 JSON-RPC 2.0（NDJSON, stdio）↔ herdr 側 Socket API（NDJSON, Unix ソケット）**」のアダプタとして機能する。詳細設計は一次情報ミラー [herdr Socket API](/references/herdr-socket-api.md) に準拠する。

herdr socket は **JSON-RPC ではなく NDJSON**（1 行 1 メッセージ・`id` 相関）で、メソッドはドット名（`workspace.create` / `agent.start` / `events.subscribe` / `pane.get` / `pane.read` / `pane.close` 等）。接続モデルは **1 接続 1 リクエスト**（herdr は応答後に接続を閉じる。#124）: 呼び出しごとに接続し、`events.subscribe` だけが持続接続としてイベント封筒 `{event, data}` を push し続ける。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/herdr.toml`（= `InitializeParams.config`）を型付け。`socket_path` / `session`（解決順: `socket_path` > `session` 名 > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定 `~/.config/herdr/herdr.sock`、named session は `sessions/<name>/herdr.sock`）/ `agent_command`（pane で起動する CLI, F-31）/ `plan_args`（plan モードの追加引数, 既定 `--permission-mode plan`）/ `design_preview` / `request_timeout_secs`。`deny_unknown_fields` |
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`unknown→前値維持`, F-32。screen manifest エージェントは `done` を報告しないため完了は stream 側の `working→idle` 確定で導出、`failed` は完了前の `pane_exited` 由来 — herdr 0.7.x に exit_code は無い）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）、`blocked` 時の質問 best-effort 抽出（F-35、`pane.read` の visible テキストから） |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送、EOF は合成 close イベントで通知）。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`→`agent.start`（**プロンプトは argv 末尾**: `agent.send` は Enter を押さず起動レースもあるため不使用）→ハンドル返却, F-31/F-37）/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`, 冪等）/ `start_state_stream`（`events.subscribe`→pane イベントを `StateNotification` へ写像し mpsc で返す。`working→idle` を 2 秒デバウンス + `pane.get` 再確認で `done` に確定し、**終端 `done` の `log_chunk` に `pane.read`（recent, ANSI strip）の最終出力を載せて** `output=source` の publish 本文を供給, F-38/#124） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# 状態ストリーム（F-38）

`state/subscribe` は ACK を返した後、herdr の `events.subscribe`（`pane.agent_status_changed` / `pane.exited`）を購読し、各イベント封筒を totsuka 正規化状態へ写像して `state/notification`（P→O）として push する。イベント購読前に broadcast 受信器を確保し、ACK 直後に push されるイベントの取りこぼしを防ぐ。購読直後に他 pane の履歴イベントが replay されることがあるため `data.pane_id` で自衛フィルタする。`done`/`failed` の終端状態でストリームを終える。

完了検知と出力回収（#124）: screen manifest エージェント（Claude Code）は `done` を報告せず、`pane_exited` に exit_code も無いため、**`working → idle` 遷移を 2 秒のデバウンス + `pane.get` 再確認で `done` に確定**する。終端 `done` の `log_chunk` には pane 生存中に `pane.read`（`recent`・ANSI strip・末尾 400 行）で読んだ**最終出力全文**を載せる — orchestrator は `output = source` の publish 本文を log_chunk の蓄積から作るため、これが返信下書きの供給経路になる。`waiting_input` の質問本文は `pane.read`（`visible`）からの best-effort 抜粋。購読接続の EOF は合成 close イベントとして通知され `failed` で終端する（pane 自体は生きている可能性があるため、復旧は `session/attach` 側に委ねる）。

# capabilities（F-33）

manifest（`plugins/agent-ide-herdr/plugin.toml`）と `initialize` 応答で `kind = agent_ide`・`plan_mode` / `design_preview` / `pane_control` / `state_stream` を宣言。

# Claude Code 固有の制約

対象エージェント Claude Code は **Lifecycle Authority を持たない**（状態は herdr の screen manifest 由来で信頼性が低い）。hook は session identity のみ報告し、`waiting_input` の構造化 native シグナルは無い。詳細は [herdr Socket API リファレンス](/references/herdr-socket-api.md) 参照。plan モードは herdr socket の機能ではなく、CLI 側 permission-mode を pane 起動時に付与して実現する（F-36）。

# テスト

- 状態写像・復帰ハンドル・質問抽出は純関数として単体テスト（`agent_status` 全 5 値、`failed` は完了前 exit 由来）。
- **実 Unix ソケットの fake herdr サーバ**（NDJSON・**応答後に接続を閉じる実機同等の接続モデル**・`{event, data}` 封筒イベント）に対して initialize→dispatch（argv プロンプト・plan フラグの検証込み）→state/subscribe→状態ストリーム（`running`→`waiting_input`（質問付き）→`running`→`done`（最終出力付き））を結合テスト（`tests/integration.rs`）。完了前 exit の `failed`・他 pane replay の無視・`id:""` エラーの即時相関・session/attach の成功・pane 消失（`pane_not_found`→`attached:false`）・`config/validate` の疎通（ping）も検証。
- 実バイナリを stdio で fake herdr ソケットに接続して疎通確認済み。
- **実機との手動疎通チェックリスト（§9）は issue #60 のコメントに整理**（状態が screen manifest 由来である前提での遅延・取りこぼし許容、waiting_input 抽出精度の観点を含む）。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`net`/`io-std` 追加）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](/references/herdr-socket-api.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
