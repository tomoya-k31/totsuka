---
type: Decision
title: ADR-0021 Slack 返信案・ピッカーの通知は「ナッジ専用 bot」の DM で行う
description: エフェメラルと自分名義 self-DM は Slack 通知を一切発生させず、オペレーターが返信案の到着に気づけない問題（#305）に対し、通知ナッジ専用の bot user を追加して bot→本人 DM で push 通知を出す決定。投稿主体は user token のまま不変で、ADR-0003 の「Bot なし」前提を部分改訂する。reminders.add ハックと macOS 通知強化のみの案は不採用。
tags: [slack, plugin, task-source, notification, bot, token, approval]
timestamp: 2026-07-28T00:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-28（issue [#305](https://github.com/tomoya-k31/totsuka/issues/305)）

[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md) Decision §3 の「Bot ユーザーを持たない」前提を**部分改訂**する: bot user は追加するが、その用途は通知ナッジ DM のみで、**会話に見える投稿の主体（承認済み返信・self-DM 記録）は従来どおり user token（本人名義）**。承認フロー・TokenGuard の防波堤構造は不変。

# Context

task-source-slack は返信案とリポジトリピッカーを「スレッド内エフェメラル + self-DM 記録」の 2 面で提示する（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）が、どちらも **Slack 通知が原理的に発生しない**:

- `chat.postEphemeral` は Slack 仕様として通知・バッジ・未読を一切発生させない
- self-DM 記録は user token（`xoxp-`）による「自分名義」投稿であり、自分の発言扱いのため通知も未読バッジも付かない

結果、オペレーターは Slack を能動的に見ていない限り返信案・ピッカーの到着に気づけない。macOS 側では `result/publish` 成功後に `Done` イベントが [notifier-macos](/components/notifier-macos.md) へ届くが、Mac の前にいないと届かない（モバイル非対応）。制約として、スレッドの相手に承認前のドラフト内容・確認 UI を見せてはならない。

# Decision

## ナッジ専用 bot user の追加

Slack アプリに bot user を追加し（manifest: bot scopes は `chat:write` + `im:write` のみ）、次の 2 タイミングで bot がオペレーターへ短い DM ナッジ（🔔 + スレッド permalink リンク）を送る:

1. 返信案ドラフト到着時（`approval::publish_draft` の 2 面投稿後。両面とも失敗した場合はボタンがどこにも無いため送らない）
2. リポジトリピッカー投稿成功時（`pipeline::post_selection_ephemeral` の成功後。投稿失敗時は hint なし提出に縮退しており、答えるべき UI が無いため送らない）

bot DM は Slack ネイティブの push・バッジがデスクトップ+モバイル両方に届き、スレッドの相手には見えない。permalink は enrich 時に解決済みの値を再利用し、`chat.getPermalink` の追加呼び出しはしない。

## 設計上の要点

- **opt-in**: `plugins/slack.toml` の任意フィールド `bot_token`（`xoxb-`）。未設定なら機能 off + 起動時 warn 1 回（後方互換 — 既存設定はそのまま動く）。設定済みで無効なトークンは TokenGuard の bot `auth.test` probe が `CONFIG_INVALID` で落とし `doctor` で可視化する（xapp と同じ扱い: 明示的に有効化した機能の死んだトークンを黙って握り潰さない）。bot に identity 照合は無い（bot は自分自身が identity）。
- **transport**: `TokenKind::Bot` を追加。`bot_token` 未設定での Bot 呼び出しは `InvalidRequest`（プラグインバグ級 — 呼び出し側が設定でゲートする契約）。
- **bot↔operator DM** は起動時に `conversations.open`（bot token）で 1 回解決し `SharedState` に保持（`self_dm` と同型）。解決失敗は warn のみの非致命（以後ナッジをスキップ、提示面は無傷）。
- **fire-and-forget**: ナッジ送信失敗は warn で握り潰し、draft/picker フローを決してブロックしない（`notify::send_nudge`）。
- **ナッジは approve/reject 後に更新・削除しない**: nudge の `ts` を永続化する（= `drafts.json` スキーマ bump）価値が無い。bot DM は通知フィードであり、記録・監査面は従来どおり self-DM 記録が担う。
- **ループ安全は構造で担保**: `event_subscriptions` は変更しない（`message.im` 非購読のまま）。bot DM への投稿はそもそもパイプラインに入らない。

# 不採用案

- **`reminders.add` ハック（Slackbot リマインド）**: user scope `reminders:write` だけで bot なしを維持できるが、①リマインド時刻は未来必須で通知が最大 1 分遅れる ②リマインダーの後始末（complete/delete）が要る ③API 自体が新世代 Slack で縮退方向。スコープ変更＝再インストールが必要な点は bot 案と同じで、維持できるのは「bot なし」の見た目だけ。
- **macOS 通知強化のみ**（notifier-macos の文言改善 + クリックでスレッドを開く）: Slack アプリ無変更で最も軽いが、モバイルに届かず根本解決にならない。bot 案と排他ではなく、将来の追加改善としては引き続き有効。

# Consequences

- トークンが 3 本になる（`xoxp-` / `xapp-` / `xoxb-`）。**最大の運用罠**: 既存アプリへの bot user 追加は再インストール必須で、その再インストールは **`xoxp-` も再発行する**。`slack-bot` の Keychain エントリを足すだけだと user token が死ぬ（`doctor` が検出する）。[Quickstart](/operations/slack-quickstart.md) とmanifest コメントに「xoxp/xoxb 両方更新」を明記した。
- アプリの DM をユーザーがミュートしていると push は出ない（コードで解決不能。Quickstart のトラブルシュートに記載）。
- `publish_draft` の RPC 応答前に Slack 呼び出しが 1 本増える（既存の inline 提示面 post と同じ 30s timeout 境界。問題化したら `tokio::spawn` へ逃がす余地を残す）。
- xoxb の影響半径は bot 名義投稿 + IM 開始のみで、xoxp より小さい（[トークンポリシー](/security/slack-user-token.md)）。

# Citations

[1] [Issue #305](https://github.com/tomoya-k31/totsuka/issues/305)
[2] [ADR-0003 Slack メンション代理返信アシスタントの設計](/decisions/adr-0003-slack-reply-assistant.md)
[3] [Slack: chat.postEphemeral — エフェメラルは通知を発生させない](https://docs.slack.dev/reference/methods/chat.postEphemeral)
