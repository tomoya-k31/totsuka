---
type: Component
title: agent-ide-herdr プラグイン
description: herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/設計プレビューを担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [rust, crate, plugin, agent-ide, herdr, socket-api, streaming, hook, deadman]
timestamp: 2026-07-18T12:00:00Z
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
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`done→done`・`unknown→前値維持`, F-32。**`session/attach` 専用**の写像で、タスク完了はもはやここを通らない — 完了検知はフックが担う）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）、`squash_ws`（`submit_prompt` の着弾確認に使う折り返し非依存の照合ヘルパ）。**質問/回答の画面抽出（旧 `extract_question` / `extract_answer`）は完了判定のフック移行に伴い削除**（#131） |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送）。broadcast はプロセス内の全購読で共有されるため、EOF 時の合成 close イベントは**購読対象 pane ごとに `data.pane_id` 付きで**発行し、他タスクを巻き込まない。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`→`agent.start`（プロンプトなし）→`submit_prompt`→ハンドル返却, F-31/F-37。**0.1.3: `hook` 指定時に env を `workspace.create`/`agent.start` へ注入し、argv に `--settings <settings_path>` を付与。`resume_session_id` 指定時は `--resume <id>` も付与**）/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`→**タスクの workspace も close**（pane id `w1:p2` の接頭辞が workspace id。dispatch が workspace を作る以上、pane だけ閉じると空の workspace が残る）, 冪等 — Done 時の pane 自動 close は Orchestrator がこの冪等 cancel を呼んで実現する, D-10）/ `snapshot`（**0.1.3: `diagnostics/snapshot`**。`pane.read`（`recent`）で画面テキストを返す。pane 消失は `text: None` でエラーにしない, R-10）/ `start_state_stream`（`events.subscribe`→**`pane.exited` デッドマン専用**に縮退。異常終了→`Failed`, F-38） |

| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# プロンプト投入（`submit_prompt`, #124）

**プロンプトを argv で渡す方式は使えない**（複数行だと CLI が投入しない = タスク本文は常に複数行なので必ずハングする）。`agent.send` でテキストを入力欄へ書き、`pane.send_keys ["enter"]` で送信する。ただし CLI は「入力を受け取れる状態」と「入力に反応できる状態」がずれており、早すぎる送信はテキストが失われ、早すぎる Enter は飲み込まれるため、**どちらも撃ちっぱなしにせず確認する**:

1. 着弾を画面で確認（プロンプト**末尾**の空白除去マッチ — 入力欄はカーソル側を表示し、CJK が語中で折り返されるため）し、未着弾なら再送
2. `agent_status ∈ {working, blocked, done}` になるまで Enter を再押下（空入力への Enter は no-op なので冪等）

どちらも確定できなければ**エラーで dispatch を失敗させる**（無言で永久ハングするセッションを作らない）。

# dispatch のフック起動（0.1.3, #131）

`TaskDispatchParams.hook`（`HookLaunchSpec = { settings_path, env }`）が Some のとき、dispatch は完了判定フックを載せた Claude Code を起動する:

- `workspace.create` / `agent.start` の params に `env` を付与（herdr 0.7.1+ は両メソッドとも `env?` 対応。フック環境変数 `TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` 等をプロセスへ注入）
- argv に `--settings <settings_path>` を付与（workflow ごとの orchestrator-*.json を読ませ、Stop/SessionEnd フックを有効化）
- `resume_session_id` が Some なら argv に `--resume <id>` も付与（Slack スレッド会話継続。`--resume` はフックを引き継がないため `--settings` は resume でも必須, H-03）

env 注入・フックの内容は**プラグインにとって不透明**（Orchestrator 側が生成・解釈する。プラグインは値を配線するだけ）。`hook` が None（旧 Orchestrator）でも dispatch は動くが、その場合 env・`--settings` は付かず**完了検知が働かない**（後述）。

# 状態ストリーム — デッドマン縮退（F-38, #131）

**完了検知はフック機構へ全面移行した**（R-07）。Claude Code の Stop/SessionEnd フックが UDS 経由で Orchestrator へ決定的に完了を通知するため、herdr の screen-manifest（画面パターン認識）由来の完了判定は**廃止**した。旧実装の「`working → idle` 確定」「2 秒デバウンス + `pane.get` 再確認」「`done` 導出」「scrollback からの質問抽出（旧 F-35）」「transcript / detection からの回答回収」は**すべて削除**。

`state/subscribe` は ACK 後に `events.subscribe` を **`pane.exited` のみ**（+ 購読断の合成 close イベント）へ縮退購読する。デッドマンとして働き、**異常終了→`Failed`** を通知して終端する:

- `pane.exited` の `exit_code` が非 0、または**コード無し**（herdr 0.7.x は exit_code を運ばないため clean と確認できない。対話モードの Claude は完了で終了しないので、説明のつかない exit は異常）→ `Failed`
- `exit_code == 0`（clean exit）→ **通知なし**でストリームを終える（正常終了はフック SessionEnd が既報）
- 購読接続の EOF（当該 pane 向け合成 close イベント）→ `Failed`（pane 自体は生きている可能性があるため復旧は `session/attach` 側に委ねる）

`data.pane_id` の自衛フィルタ・イベント区切り文字の正規化（`pane.agent_status_changed` はドット、`pane_exited` はアンダースコア）は継続。この縮退は**無条件**（`hook` None でも同じ）: 旧 Orchestrator + 新プラグインの組合せは `^0.1` 互換上は成立するが完了を検知しなくなるため、`initialize` の `protocol_version` が 0.1.3 未満なら**警告ログ**を出す（orchestrator 側 0.1.3 以上必須）。

# diagnostics/snapshot（R-10, 0.1.3）

`diagnostics/snapshot`（O→P、`diagnostics_snapshot` capability）はタイムアウト/エスカレーション診断のために pane 画面をキャプチャする。`pane.read`（`source = recent`）で画面テキストを返し、pane 消失（や読み取り失敗）は `text: None` で返す — **取得失敗はエラーにしない**ため、Orchestrator のエスカレーション経路がスナップショット不能で失敗することはない。

# capabilities（F-33）

manifest（`plugins/agent-ide-herdr/plugin.toml`）と `initialize` 応答で `kind = agent_ide`・`plan_mode` / `design_preview` / `pane_control` / `state_stream` に加え、**0.1.3 で `resume_session`（`--resume` セッション再開）/ `diagnostics_snapshot`（`diagnostics/snapshot`）**を宣言する（両者は一致させる）。

# Claude Code 固有の制約

対象エージェント Claude Code は **Lifecycle Authority を持たない**（herdr の screen manifest 由来の状態は遅延・取りこぼし・誤検知が構造的に避けられない, #124/#130）。そのため完了判定は**フック（Stop/SessionEnd の command 型 + curl で UDS へ POST）へ移行**し、このプラグインの状態ストリームは `pane.exited` デッドマンに縮退した。plan モードは herdr socket の機能ではなく、CLI 側 permission-mode を pane 起動時に付与して実現する（F-36）。詳細は [herdr Socket API リファレンス](/references/herdr-socket-api.md) 参照。

# テスト

- 状態写像・復帰ハンドル・`squash_ws`・プロンプト末尾マーカー・exit 分類は純関数として単体テスト。
- **実 Unix ソケットの fake herdr サーバ**に対する結合テスト（`tests/integration.rs`）。fake は実機を模す: **応答後に接続を閉じる**接続モデル、`{event, data}` 封筒（**ドット/アンダースコア混在**の実イベント名）、そして**入力に反応できるまで `agent.send` / Enter を落とす CLI**（= 実機の起動レース）。dispatch がその race を自己修正して完走すること・始動しない CLI では**エラーで失敗する**こと・**フック env が `workspace.create`/`agent.start` に乗り `--settings`/`--resume` が argv に入ること**・**`pane.agent_status_changed` を送っても通知が出ないこと（縮退の固定化）**・`pane.exited` 非 0/コード無し→`Failed`・clean exit（0）は通知なし・`diagnostics/snapshot` の正常/pane 消失（`text: null`）両応答・他 pane の replay と close 通知を無視すること・`id:""` エラーの即時相関・session/attach の成功と pane 消失（`pane_not_found`→`attached:false`）・`config/validate` の疎通（ping）を検証。
- **実機手動チェック**（受け入れ #2）: 実 herdr + 実 Claude Code で `--settings` 付き pane 起動 → フック発火 → env（`TOTSUKA_JOB_ID`）がフックスクリプトから見えること（#123 検収環境）は issue #139 のコメントにチェックリストとして整理。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`net`/`io-std` 追加）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](/references/herdr-socket-api.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38・§4.11 F-100〜F-107](/product/orchestrator-spec.ja.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md) / [フックシグナルフロー](/architecture/hook-signal-flow.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
