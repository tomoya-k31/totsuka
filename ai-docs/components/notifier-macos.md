---
type: Component
title: notifier-macos プラグイン
description: Orchestrator のイベント（waiting_input / done / failed / pending / escalated / verification_pending）を macOS 通知センターへ配送する公式 notifier プラグイン。バックエンド選択（osascript / terminal-notifier click-to-focus）、ワークフロー×イベント別フィルタ、fire-and-forget 配送。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/notifier-macos
tags: [rust, crate, plugin, notifier, macos, osascript, terminal-notifier, click-to-focus, hook, escalation, verification]
generated: { by: claude-code/opus-5, at: 2026-08-22T13:30:00Z }
status: stable
owner: tomoya-k31
---

# 責務

Orchestrator からのイベント（`waiting_input` / `done` / `failed` / `pending`、および #131 で追加の `escalated` / `verification_pending`）を macOS 通知センターへ配送する公式 Notifier プラグイン（F-90〜F-93）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリ。`notify` は JSON-RPC **notification**（応答不要）で受信し、**fire-and-forget**（F-93）で配送する。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `[macos]`（= `InitializeParams.config`）を型付け。**`backend`**（`osascript`（既定・後方互換）/ `terminal_notifier`、#155 F-94）/ `osascript_bin` / **`terminal_notifier_bin`** / **`activate_bundle_id`**（クリック時に前面化する GUI アプリの bundle id、例 `org.alacritty`。環境依存・未設定なら `-activate` なし）/ **`click_command`**（`-execute` テンプレート、既定 `totsuka focus {task_id}`。空で無効化）/ `filter`（`[filter.events]` グローバル on/off ＋ `[filter.workflows.<name>]` 別 override、F-92。トグルキーは `waiting_input` / `done` / `failed` / `pending` に加え **#131 で `escalated` / `verification_pending`**）。`Filter::allows(workflow, event)` は「ワークフロー別 > グローバル > 既定（全 on）」の優先で判定（新イベントも既定 on）。`deny_unknown_fields` |
| `sender` | `NotificationSender` trait（`send(Notice)` / `probe()`）＋ 2 バックエンド（`BackendSender` enum が `backend` 設定で選択）。**`OsascriptSender`**: AppleScript `display notification`。ユーザ文字列は `on run argv` の **argv 経由**で渡し、スクリプト本文へ補間しない（インジェクション防止）。クリックは owner（Script Editor）が開くだけで pane へ届かない。**`TerminalNotifierSender`**（#155 F-94、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）: `-title/-subtitle/-message` + `-group totsuka-<task_id>`（タスク別集約）+ `-activate <bundle-id>`（GUI 前面化）+ `-execute '<click_command>'`（`{task_id}` は **シングルクォートでシェル引用**して埋め込み = インジェクション安全）。**Sequoia 15.x+ で `-activate` と併用すると click-to-focus が壊れるため `-sender` は使わない**。バイナリ不在（spawn NotFound）は**送信単位で osascript へ自動フォールバック**（クリック不可だが通知は届く）。`probe()` は `terminal-notifier -help`（フォールバックしない = `config/validate` が「設定済みなのに未導入」を actionable に検出する）。`Notice` は title/subtitle/body に加え `task_id` を運ぶ |
| `server` | JSON-RPC ディスパッチ `Server<F: SenderFactory>`。initialize / config·validate（`probe` で非表示疎通確認）/ shutdown は応答、`notify` は応答せず配送タスクを spawn（fire-and-forget、失敗はログのみ）。イベント→絵文字＋日本語ラベル＋タスク/ワークフロー/本文へ整形（`escalated` = 🚨 エスカレーション、`verification_pending` = 🔍 検収待ちの title/body テンプレートを含む #131）。整形した `Notice` に `NotifyParams.task_id` を載せる（クリック相関、F-94） |
| `main` | `#[tokio::main]` の stdio ループ。`BackendFactory`（`BackendSender::from_config`）を配線 |

# 配送とフィルタ（F-92/F-93）

`notify` 受信時、`filter.allows(workflow, event)` が真のイベントのみ整形して `osascript` へ配送する。配送は `tokio::spawn` で fire-and-forget。送出失敗・プラグイン不在・フィルタ除外はいずれもタスク実行に影響しない（Orchestrator は応答を待たない・#51 のクラッシュ隔離と併せて成立）。通知本文には `waiting_input` の質問先頭（F-35）や `pending` のリポジトリ確認（F-14）など補足が載る。

# Escalated / VerificationPending の一級対応（#131）

Claude Code フック完了判定（[F-100〜F-107](/product/orchestrator-spec.ja.md)、フロー: [フックシグナルフロー](/architecture/hook-signal-flow.md)）の導入に伴い、`NotifierEvent` へ additive に `Escalated`（wire 値 `"escalated"`）と `VerificationPending`（wire 値 `"verification_pending"`）が追加された（[plugin-protocol](/components/plugin-protocol.md) 0.1.3）。notifier-macos はこれらを**一級イベントとして完全対応**する:

- **title/body テンプレート**: `escalated` = 🚨 エスカレーション（3 回連続 UNKNOWN / タイムアウト / 相関異常での人間対応要求、D-02/D-03）、`verification_pending` = 🔍 検収待ち（`verification = "human"` の workflow で自己申告完了・検収未確定、D-01）。他イベント同様、絵文字 + 日本語ラベル + タスク/ワークフロー/補足本文へ整形する。
- **F-92 フィルタトグル**: `[filter.events]`（グローバル）と `[filter.workflows.<name>]`（ワークフロー別 override）の両方に `escalated` / `verification_pending` キーを追加（未指定は既定 on）。
- **中間イベントは notifier 専用**: `WaitingInput` / `Escalated` / `VerificationPending` / `Failed` はいずれも **notifier へのみ**配送され、ソーススレッド（Slack 返信等）へは決して投稿されない（R-08/D-07。承認待ち・質問待ち・検収待ちを本人へ push で気づかせるが、ソース側の会話は汚さない）。ソースへの書き戻しは `done` 時の出力ポリシー（`result/publish` 等）だけが担う。

旧 notifier（0.1.3 未満）はこの 2 値のデシリアライズに失敗するが、通知は fire-and-forget（F-93）のため**通知欠落に留まりタスク実行は無影響**（前方互換）。

# click-to-focus（F-94, #155）

`backend = "terminal_notifier"` のとき、通知クリックは (a) `-activate <activate_bundle_id>` による GUI ターミナルのネイティブ前面化と (b) `-execute` による `totsuka focus <task_id>` 実行（[orchestrator-cli](/components/orchestrator-cli.md) → 制御 UDS [`POST /focus`](/apis/agent-events.md) → [agent-ide-herdr](/components/agent-ide-herdr.md) の `session/focus`）の 2 段で対象 pane を開く。**task_id を知っているのは notifier**（`NotifyParams.task_id`）であり、pane_id をプロトコルへ足す必要はない（F-37 不透明契約の維持、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。縮退: terminal-notifier 未導入は osascript へ自動フォールバック、Orchestrator 停止中のクリックは `totsuka focus` が静かに no-op（アプリ前面化のみ成立）、`activate_bundle_id` 未設定は pane フォーカスのみ。

# capabilities

Notifier は機能 capability を宣言しない（`notify` を受けるのみ）。manifest（`plugins/notifier-macos/plugin.toml`）で `kind = notifier`。

# プロトコル変更（#62 / #131）

ワークフロー別フィルタのため、[plugin-protocol](/components/plugin-protocol.md) の `NotifyParams` に任意フィールド `workflow` を追加した（#62、後方互換・`serde(default)`）。Orchestrator 側の配送配線（#63）で populate される。#131 で `NotifierEvent` に `escalated` / `verification_pending` が additive 追加された（上記）。

# テスト

- フィルタ優先順位（グローバル / ワークフロー override / 既定全 on）・通知整形・osascript argv のインジェクション安全性は単体テスト。**#155: backend 設定のパース（既定 osascript・未知値拒否）、terminal-notifier argv（title/group/activate/execute の構成・`-sender` 不使用・task_id/bundle 無し時の縮退・空 `click_command` での `-execute` 無効化）、`{task_id}` シェル引用のインジェクション安全性**（悪意ある id が `-execute` のシェル文字列を破れないこと）。
- 録画 fake sender に対して initialize→`notify`（4 イベント種別すべて配送）→フィルタ抑制→配送失敗の握り潰し（fire-and-forget で後続リクエストが継続）→`config/validate` を結合テスト（`tests/integration.rs`）。
- 実バイナリを stdio で駆動し、実 `osascript` で通知センターへ実際に表示されることを手動確認済み（初期化→config/validate 非表示疎通→notify 配送→shutdown）。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`process`/`io-std`）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [Spec §4.10 通知 / F-90〜F-93・F-35・F-14](/product/orchestrator-spec.ja.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
