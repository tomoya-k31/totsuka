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
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`done→done`・`unknown→前値維持`, F-32。`done` が来ない試行があるため完了は stream 側で `working→idle` 確定からも導出、`failed` は完了前の `pane_exited` 由来 — herdr 0.7.x に exit_code は無い）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）、画面テキストのヘルパ（`squash_ws` = 折り返し非依存の照合、`extract_question` = F-35 の質問抽出、`extract_answer` = detection ビューからの回答抽出フォールバック） |
| `transcript` | **エージェント自身の会話ログから最終回答を読む層**（#124）。`TranscriptReader` trait + `agent_session.agent` をキーにしたレジストリで、herdr が統合する複数エージェントに開かれた seam。現状の実装は Claude Code のみ（`~/.claude/projects/<cwd エンコード>/<session id>.jsonl` の最後の assistant テキスト。`CLAUDE_CONFIG_DIR` 対応、規約変更に備え session id での探索をフォールバックに持つ）。**CLI 自身が assistant として書く合成エントリ（`isApiErrorMessage` / `isMeta` / `model: "<synthetic>"`）は除外**する — レート制限は `{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your session limit …"}]}}` として記録され、CLI はその後 idle に戻る（= 完了検知の発火条件そのもの）ため、素通しすると**エラー文が回答として publish される**（#130）。未対応エージェントは reader 不在として画面抽出へ縮退する |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送）。broadcast はプロセス内の全購読で共有されるため、EOF 時の合成 close イベントは**購読対象 pane ごとに `data.pane_id` 付きで**発行し、他タスクを巻き込まない。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`→`agent.start`（プロンプトなし）→`submit_prompt`→ハンドル返却, F-31/F-37）/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`→**タスクの workspace も close**（pane id `w1:p2` の接頭辞が workspace id。dispatch が workspace を作る以上、pane だけ閉じると空の workspace が残る）, 冪等）/ `start_state_stream`（`events.subscribe`→pane イベントを `StateNotification` へ写像し mpsc で返す, F-38） |

| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# プロンプト投入（`submit_prompt`, #124）

**プロンプトを argv で渡す方式は使えない**（複数行だと CLI が投入しない = タスク本文は常に複数行なので必ずハングする）。`agent.send` でテキストを入力欄へ書き、`pane.send_keys ["enter"]` で送信する。ただし CLI は「入力を受け取れる状態」と「入力に反応できる状態」がずれており、早すぎる送信はテキストが失われ、早すぎる Enter は飲み込まれるため、**どちらも撃ちっぱなしにせず確認する**:

1. 着弾を画面で確認（プロンプト**末尾**の空白除去マッチ — 入力欄はカーソル側を表示し、CJK が語中で折り返されるため）し、未着弾なら再送
2. `agent_status ∈ {working, blocked, done}` になるまで Enter を再押下（空入力への Enter は no-op なので冪等）

どちらも確定できなければ**エラーで dispatch を失敗させる**（無言で永久ハングするセッションを作らない）。

# 状態ストリーム（F-38）

`state/subscribe` は ACK を返した後、herdr の `events.subscribe`（`pane.agent_status_changed` / `pane.exited`）を購読し、各イベント封筒を totsuka 正規化状態へ写像して `state/notification`（P→O）として push する。イベント購読前に broadcast 受信器を確保し、ACK 直後に push されるイベントの取りこぼしを防ぐ。**`event` の区切り文字は種別によって混在する**（`pane.agent_status_changed` はドット、`pane_exited` はアンダースコア）ため正規化して比較する — 片方だけに合わせると状態変化を全て無言で取りこぼす。購読直後に他 pane の履歴イベントが replay されるため `data.pane_id` で自衛フィルタする（合成 close イベントも同じフィルタを通る）。`done`/`failed` の終端状態でストリームを終える。

購読直後に `pane.get` で状態をシードするため、dispatch と subscribe の間に完了した高速な回答も取りこぼさない（`dispatch` はエージェント始動を確認してから返るため、ここで観測した状態は本物の進行を意味する）。

完了検知と出力回収（#124）: 終端は `done` のことも `working → idle` のこともあるため両方を扱い、idle 経路は 2 秒のデバウンス + `pane.get` 再確認で確定する（screen manifest 由来のちらつき対策）。終端 `done` の `log_chunk` に**最終回答の全文**を載せ、orchestrator の `output = source` publish 本文（= Slack 返信下書き）を供給する — orchestrator は publish 本文を log_chunk の蓄積から作るため、これが下書きの唯一の供給経路。回答は [`transcript`](#モジュール構成) 層から取り、取れない場合のみ `pane.read(detection)` の画面抽出へ縮退する（画面には scrollback が無く長文は先頭が欠落するため、これは劣化パス）。`waiting_input` の質問本文は `pane.read`（`visible`）からの best-effort 抜粋。購読接続の EOF は当該 pane 向けの合成 close イベントとして通知され `failed` で終端する（pane 自体は生きている可能性があるため、復旧は `session/attach` 側に委ねる）。

# capabilities（F-33）

manifest（`plugins/agent-ide-herdr/plugin.toml`）と `initialize` 応答で `kind = agent_ide`・`plan_mode` / `design_preview` / `pane_control` / `state_stream` を宣言。

# Claude Code 固有の制約

対象エージェント Claude Code は **Lifecycle Authority を持たない**（状態は herdr の screen manifest 由来で信頼性が低く、終端が `done` と `idle` で揺れる）。hook は session identity のみ報告し、`waiting_input` の構造化 native シグナルは無い。詳細は [herdr Socket API リファレンス](/references/herdr-socket-api.md) 参照。plan モードは herdr socket の機能ではなく、CLI 側 permission-mode を pane 起動時に付与して実現する（F-36）。

なお `transcript` 層は Claude Code 固有の実装を持つが、**エージェント固定ではない**: herdr が統合する各エージェントに対し `agent_session.agent` をキーに reader を足せる（未対応エージェントは画面抽出へ縮退）。

# テスト

- 状態写像・復帰ハンドル・質問/回答抽出・transcript 解析・プロンプト末尾マーカーは純関数として単体テスト。
- **実 Unix ソケットの fake herdr サーバ**に対する結合テスト（`tests/integration.rs`）。fake は実機を模す: **応答後に接続を閉じる**接続モデル、`{event, data}` 封筒（**ドット/アンダースコア混在**の実イベント名）、そして**入力に反応できるまで `agent.send` / Enter を落とす CLI**（= 実機の起動レース）。dispatch がその race を自己修正して完走すること・始動しない CLI では**エラーで失敗する**こと・状態ストリーム（`waiting_input`（質問付き）→`running`→`done`（回答付き））・subscribe 前に完了した高速回答の `done`・完了前 exit の `failed`・他 pane の replay と**他 pane の close 通知**を無視すること・`id:""` エラーの即時相関・session/attach の成功と pane 消失（`pane_not_found`→`attached:false`）・`config/validate` の疎通（ping）を検証。
- **実機ライブ検証済み**（#124/#123）: 実 herdr 0.7.4 + 実 Claude Code に対し dispatch（4.4s）→ 状態ストリーム → `done`（8.4s）まで完走し、`log_chunk` に装飾なし・欠落なしの回答本文が載ることを確認。
- **実機との手動疎通チェックリスト（§9）は issue #60 のコメントに整理**（状態が screen manifest 由来である前提での遅延・取りこぼし許容、waiting_input 抽出精度の観点を含む）。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`net`/`io-std` 追加）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](/references/herdr-socket-api.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
