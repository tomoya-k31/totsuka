---
type: Component
title: task-source-slack プラグイン
description: 自分宛の Slack メンションをタスク化し本人名義で代理返信するための公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。設定スキーマ・TokenGuard・Web API クライアント・Socket Mode クライアントに加え、メンション検知（判定表 + 冪等化）・スレッド文脈付き Task 正規化・バッファ + tasks/fetch 排出まで実装済み。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [rust, crate, plugin, task-source, slack, socket-mode, token-guard]
timestamp: 2026-07-15T15:00:00Z
status: active
owner: tomoya-k31
---

# 責務

自分（`target_user_id`）宛の Slack メンションを検知して totsuka のタスクへ正規化し、承認後に **本人名義**（ユーザートークン `xoxp-`、Bot なし）でスレッド返信するための公式プラグイン。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。構造は [task-source-notion](/components/task-source-notion.md) / [task-source-github](/components/task-source-github.md) に準拠。orchestrator-core / plugin-protocol への変更なしで成立する（エピック #102 の設計判断）。

現在の実装範囲: crate 構成・設定スキーマ・stdio JSON-RPC ディスパッチ・TokenGuard（#103）、**Slack Web API の型付きクライアント層** と **Socket Mode WebSocket クライアント**（#104）、**メンション検知 → Task 正規化 → バッファ → `tasks/fetch` 排出のパイプライン**（#105）まで。`task/update_status` / `result/publish`（受理して no-op）はまだスタブ。リポジトリ解決（#106）、承認フロー（#107）が後続で載る。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/slack.toml`（= `InitializeParams.config`）を型付け。`app_token`（`xapp-`, Socket Mode 用）/ `user_token`（`xoxp-`, 本人名義）/ `target_user_id` / `thread_context_limit`（既定 6）/ `reply_style` / `source_name`（既定 `slack`）/ `[llm]`（リポジトリ選択用 OpenAI 互換 LLM: base_url / model / api_key / confidence_threshold 既定 0.6）/ `[[channel_groups]]`（prefix→候補 repos, 定義順 first-match）/ `[[repos]]`（候補リポジトリ: name / summary? / path?）/ `api_url`（テスト用上書き）/ `max_retries`。`deny_unknown_fields`。静的検証 `static_config_errors`: トークン prefix 形式、`[[repos]]` 非空・重複、repos 2 件以上での `[llm]` 必須、channel_groups→repos の参照整合、confidence_threshold 範囲 |
| `transport` | `SlackTransport` trait（`call(token_kind, method, body, idempotent)` + `post_url`＝`response_url` への POST）＋ reqwest 実装 `ReqwestTransport`。引数は **フォームエンコード** 送信（Slack の read 系 API は JSON body 非対応。`blocks` 等のネスト値は JSON 文字列）。リトライ規律: 通常の一過性エラー（5xx・ネットワーク）は冪等呼び出しのみ指数バックオフ（上限 60s）、**HTTP 429 は `Retry-After` を正確に尊重し非冪等でもリトライ**（429 は拒否済みの保証があるため安全）。コール毎のリトライ総待機バジェット（既定 90s）を超える待機は即時失敗（initialize が無応答に見えないため）。`response_url` POST はリトライなし（発行 30 分・5 回制限）。`expect_ok` で Slack の `{"ok": bool}` エンベロープを解釈。録画レスポンスでテストするための seam |
| `slack_api` | `SlackApi<T: SlackTransport>` — 上位ロジックが Slack と通信する唯一の窓口となる型付きラッパ: `auth.test` / `apps.connections.open`（App トークン）/ `conversations.replies` / `conversations.open`（self-DM 解決）/ `users.info`（display_name→real_name→name フォールバック）/ `chat.getPermalink` / `chat.postMessage`・`chat.postEphemeral`（非冪等＝自動リトライなし）/ `chat.update`（冪等）/ `response_url` POST。共通エラーハンドラが失効系（`invalid_auth` / `token_revoked` / `account_inactive`）をトークン種別に応じたガイダンス付き `Auth` へ昇格し tracing へ出力。`auth.test` のみ **全** API エラーを credential 扱い（引数がなく失敗要因はトークンのみのため） |
| `error` | `SlackError`（Auth / IdentityMismatch / Api / RateLimited / Http / Transport / Timeout / InvalidResponse / InvalidRequest）。`is_retryable`（RateLimited・5xx・ネットワーク）/ `is_rejected`（429＝未適用の保証、非冪等でも再送可）/ `is_credential`（TokenGuard 系）を分類。`auth_failure`（user トークン）と `app_auth_failure`（App-Level トークン＝xapp 再生成手順）が原因別の回復手順付きメッセージへ写像 |
| `socket_mode` | Socket Mode WebSocket クライアント（tokio-tungstenite）。ライフサイクル: `apps.connections.open`（App トークン）→ WSS 接続 → `hello` → envelope 読み取り。**ack 規律**: `events_api` / `interactive` envelope は受信後すぐ ack を送信し（Slack は約 3 秒で再配送するため下流処理を待たせない）、正規化済みイベント `SocketEvent::Message`（message イベントのみ）/ `SocketEvent::BlockActions` を ack 後に **unbounded** チャネルへ配送（有界だと遅い消費者が読み取りループを塞ぎ後続 ack が遅延するため）。`disconnect` / Close / クリーン EOF は `hello` 済みセッションのみ即時再接続（`hello` 前に死んだ接続は失敗としてバックオフ）、`idle_timeout`（既定 60s）無通信はサイレント切断とみなし再接続。接続失敗は上限付き指数バックオフ（transport と共有の `capped_backoff`）、連続失敗閾値超で warn。`apps.connections.open` の API エラーは全て設定不備（missing_scope / not_allowed_token_type 含む）として xapp 再生成ガイダンス付きでループを停止。Receiver drop で待機中でも即終了 |
| `mention` | メンション判定表（この順で first-hit）: ①`subtype`/`bot_id` あり ②送信者が本人 ③self-DM チャンネル ④`<@target_user_id>`（`<@U…|label>` 形式も可）不含 ⑤`channel:ts` 処理済み（上限 1024 の LRU、再起動で消失は許容 — orchestrator 側 ingest も冪等）→ すべて通過で `Mention`（task_id=`{channel}:{ts}`、reply_ts=`thread_ts ?? ts`） |
| `pipeline` | Socket Mode イベントの常駐消費タスク。self-DM チャンネルを起動時に解決（失敗は警告のみ、判定表②が防波堤）。fresh mention を users.info / conversations.info（ラン内キャッシュ）・conversations.replies（直近 `thread_context_limit` 件、話者名付き）・chat.getPermalink で強化し、共通 Task スキーマへ正規化（body=指示文 + reply_style + メンション + スレッド文脈の構造化 Markdown、repo_hint は `[[repos]]` 単一時のみ #106 まで）。強化の失敗は劣化（生 ID・文脈欠落注記・url なし）で吸収しタスクは捨てない。`SharedState` = タスクバッファ + pending index（task_id → `PendingMention` = channel/reply_ts/送信者名/permalink。issue の PendingTaskIndex に相当、上限 1024 の FIFO 束縛、`result/publish` が take して消費） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。initialize（TokenGuard → Socket Mode + パイプラインの常駐ランタイム起動。`without_runtime()` はプロトコルテスト用）/ config·validate（**静的検証のみ・ネットワーク不要**）/ shutdown / tasks·fetch（バッファ全排出、trigger は v1 未解釈、2 回目は同一 Task を返さない）/ task·update_status / result·publish（スタブ）。未初期化メソッドは拒否。Session drop で常駐タスクを abort |
| `main` | `#[tokio::main]` の stdio ループ。`ReqwestFactory` を配線。ログは stderr |

# TokenGuard（起動時検証）

`initialize` 内で `auth.test`（user_token）を実行し、失敗時は原因別ガイダンス付きエラーで初期化を拒否する:

- `invalid_auth` / `token_revoked` / `account_inactive` → トークン再発行・アプリ再インストール・Keychain 更新手順を含むメッセージ（`CONFIG_INVALID`）
- 成功時は `user_id == target_user_id` を検証。不一致は **なりすまし防止** のためエラー（両 ID と修正手順を明示、`CONFIG_INVALID`）
- ネットワーク障害などクレデンシャル以外の失敗は `INTERNAL_ERROR` として区別

`config/validate` は意図的にオフライン（静的検証のみ）。ホストのプローブ（`totsuka config validate` / `doctor`）は launch ハンドシェイクで `initialize` も呼ぶため、TokenGuard はそこで実行される。

# capabilities（F-83）

manifest（`plugins/task-source-slack/plugin.toml`）と `initialize` 応答で `kind = task_source`・`outputs = ["source"]` を宣言（返信は `result/publish` でソース＝Slack スレッドへ書き戻す）。

# テスト

`SlackTransport` を録画レスポンスの fake（`tests/common/`、各テストクレートで共有）に差し替えた 3 系統 + トランスポート実挙動の 1 系統: `tests/slack_api.rs`（全ラッパのリクエスト形状＝メソッド・トークン種別・引数・冪等性クラス、応答パース、失効系の共通ガイダンス、auth.test の全エラー credential 扱い）、`tests/web_api_http.rs`（raw TCP の実 HTTP モック・依存追加なしで、フォームエンコード・bearer 切替・429/Retry-After 尊重・冪等/非冪等リトライ規律・バジェット即時失敗・不正 Retry-After の保守的フォールバックを検証）、および JSON-RPC 境界越しの結合テスト（`tests/integration.rs`）: TokenGuard 成功→capabilities 宣言、auth 失敗 3 種の原因別ガイダンス、user_id 不一致拒否、ネットワーク障害の INTERNAL_ERROR 区別、オフライン config/validate（正常系はトランスポート無呼び出しを検証）、未初期化拒否、スタブ応答、shutdown・パースエラー・通知の各プロトコル配管。実バイナリでも `plugin install` / `enable` / `config validate`（正常＋未知キー・トークン形式・repos 不整合）/ `doctor` プローブ、およびローカル偽 Slack API と実 Slack API（invalid_auth）双方に対する TokenGuard を確認済み。

# 依存

- `plugin-protocol`（プラグイン境界）、`reqwest`（Web API）、`tokio-tungstenite` + `futures-util`（Socket Mode WebSocket。TLS は reqwest の `default-tls` に合わせ native-tls）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [task-source-github](/components/task-source-github.md)
- [task-source-notion](/components/task-source-notion.md)
- [Spec §4.2 タスクソース / F-01・F-07・F-51・F-59・F-64・F-65](/product/orchestrator-spec.ja.md)
