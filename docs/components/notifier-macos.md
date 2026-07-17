---
type: Component
title: notifier-macos プラグイン
description: Orchestrator のイベント（waiting_input / done / failed / pending / escalated / verification_pending）を macOS 通知センターへ配送する公式 notifier プラグイン。osascript ラップ、ワークフロー×イベント別フィルタ、fire-and-forget 配送。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/notifier-macos
tags: [rust, crate, plugin, notifier, macos, osascript, hook, escalation, verification]
timestamp: 2026-07-18T12:00:00Z
status: active
owner: tomoya-k31
---

# 責務

Orchestrator からのイベント（`waiting_input` / `done` / `failed` / `pending`、および #131 で追加の `escalated` / `verification_pending`）を macOS 通知センターへ配送する公式 Notifier プラグイン（F-90〜F-93）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリ。`notify` は JSON-RPC **notification**（応答不要）で受信し、**fire-and-forget**（F-93）で配送する。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/notifier-macos.toml`（= `InitializeParams.config`）を型付け。`osascript_bin` / `filter`（`[notifier.filter.events]` グローバル on/off ＋ `workflows` 別 override、F-92。トグルキーは `waiting_input` / `done` / `failed` / `pending` に加え **#131 で `escalated` / `verification_pending`**）。`Filter::allows(workflow, event)` は「ワークフロー別 > グローバル > 既定（全 on）」の優先で判定（新イベントも既定 on）。`deny_unknown_fields` |
| `sender` | `NotificationSender` trait（`send(Notice)` / `probe()`）＋ `OsascriptSender`。AppleScript `display notification` を `osascript` で送出。ユーザ文字列は `on run argv` の **argv 経由**で渡し、スクリプト本文へ補間しない（引用符・記号によるインジェクション防止）。将来 UNUserNotificationCenter 化できるよう trait 化 |
| `server` | JSON-RPC ディスパッチ `Server<F: SenderFactory>`。initialize / config·validate（`probe` で非表示疎通確認）/ shutdown は応答、`notify` は応答せず配送タスクを spawn（fire-and-forget、失敗はログのみ）。イベント→絵文字＋日本語ラベル＋タスク/ワークフロー/本文へ整形（`escalated` = 🚨 エスカレーション、`verification_pending` = 🔍 検収待ちの title/body テンプレートを含む #131） |
| `main` | `#[tokio::main]` の stdio ループ。`OsascriptFactory` を配線 |

# 配送とフィルタ（F-92/F-93）

`notify` 受信時、`filter.allows(workflow, event)` が真のイベントのみ整形して `osascript` へ配送する。配送は `tokio::spawn` で fire-and-forget。送出失敗・プラグイン不在・フィルタ除外はいずれもタスク実行に影響しない（Orchestrator は応答を待たない・#51 のクラッシュ隔離と併せて成立）。通知本文には `waiting_input` の質問先頭（F-35）や `pending` のリポジトリ確認（F-14）など補足が載る。

# Escalated / VerificationPending の一級対応（#131）

Claude Code フック完了判定（[F-100〜F-107](/product/orchestrator-spec.ja.md)、フロー: [フックシグナルフロー](/architecture/hook-signal-flow.md)）の導入に伴い、`NotifierEvent` へ additive に `Escalated`（wire 値 `"escalated"`）と `VerificationPending`（wire 値 `"verification_pending"`）が追加された（[plugin-protocol](/components/plugin-protocol.md) 0.1.3）。notifier-macos はこれらを**一級イベントとして完全対応**する:

- **title/body テンプレート**: `escalated` = 🚨 エスカレーション（3 回連続 UNKNOWN / タイムアウト / 相関異常での人間対応要求、D-02/D-03）、`verification_pending` = 🔍 検収待ち（`verification = "human"` の workflow で自己申告完了・検収未確定、D-01）。他イベント同様、絵文字 + 日本語ラベル + タスク/ワークフロー/補足本文へ整形する。
- **F-92 フィルタトグル**: `[notifier.filter.events]`（グローバル）と `[[workflows]]` 別 override の両方に `escalated` / `verification_pending` キーを追加（未指定は既定 on）。
- **中間イベントは notifier 専用**: `WaitingInput` / `Escalated` / `VerificationPending` / `Failed` はいずれも **notifier へのみ**配送され、ソーススレッド（Slack 返信等）へは決して投稿されない（R-08/D-07。承認待ち・質問待ち・検収待ちを本人へ push で気づかせるが、ソース側の会話は汚さない）。ソースへの書き戻しは `done` 時の出力ポリシー（`result/publish` 等）だけが担う。

旧 notifier（0.1.3 未満）はこの 2 値のデシリアライズに失敗するが、通知は fire-and-forget（F-93）のため**通知欠落に留まりタスク実行は無影響**（前方互換）。

# capabilities

Notifier は機能 capability を宣言しない（`notify` を受けるのみ）。manifest（`plugins/notifier-macos/plugin.toml`）で `kind = notifier`。

# プロトコル変更（#62 / #131）

ワークフロー別フィルタのため、[plugin-protocol](/components/plugin-protocol.md) の `NotifyParams` に任意フィールド `workflow` を追加した（#62、後方互換・`serde(default)`）。Orchestrator 側の配送配線（#63）で populate される。#131 で `NotifierEvent` に `escalated` / `verification_pending` が additive 追加された（上記）。

# テスト

- フィルタ優先順位（グローバル / ワークフロー override / 既定全 on）・通知整形・osascript argv のインジェクション安全性は単体テスト。
- 録画 fake sender に対して initialize→`notify`（4 イベント種別すべて配送）→フィルタ抑制→配送失敗の握り潰し（fire-and-forget で後続リクエストが継続）→`config/validate` を結合テスト（`tests/integration.rs`）。
- 実バイナリを stdio で駆動し、実 `osascript` で通知センターへ実際に表示されることを手動確認済み（初期化→config/validate 非表示疎通→notify 配送→shutdown）。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`process`/`io-std`）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [Spec §4.10 通知 / F-90〜F-93・F-35・F-14](/product/orchestrator-spec.ja.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
