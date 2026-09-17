---
type: Term
title: エフェメラル承認フロー
description: エージェントの返信案をスレッド内エフェメラルに提示し、承認ボタン押下時のみ本人名義で送信する task-source-slack の仕組み。勝手に送信しないための防波堤。提示面は当初 self-DM 記録との 2 面だったが、押下後の後始末が片方だけ成功しうるため 1 面に減らした。押下後はナッジ DM に ✅/❌ を書き戻してからエフェメラルを削除し、書き戻せない構成では置換にフォールバックする。
tags: [glossary, slack, approval, ephemeral]
generated: { by: claude-code/opus-5, at: 2026-09-17T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# エフェメラル承認フロー

task-source-slack の `result/publish` が、エージェントの返信案（下書き）を即送信せず、メンションスレッド内の **エフェメラルメッセージ**（本人にだけ見える）に Block Kit で提示し、**承認して返信** ボタン（confirm ダイアログ付き）の押下時のみ本人名義（`xoxp-` トークン）でスレッド返信する仕組み。却下は送信せず破棄する。押下時、エフェメラルは `response_url` の **`delete_original`** で**削除**する —— ただし先に、その下書きを知らせたナッジ DM を ✅/❌ に `chat.update` してからである（[ADR-0074](/decisions/adr-0074-single-draft-surface.md) 決定 7）。**消すと却下の痕跡がどこにも残らない**ので、順序が逆になってはいけない。書き戻す先が無い構成（`bot_token` 未設定、bot DM 未解決、ナッジ投稿の失敗、`nudge_ts` を持たない旧下書き、`chat.update` の失敗）では、従来どおり **`replace_original`** での**置換**にフォールバックする。**二重押下も同じく塗り直す**: 2 回目に来たこと自体が「ボタンがまだ出ている」証拠であり、`block_actions` は Event Gateway 経由では at-least-once なので**人が 1 回しか押していなくても**ここに来る。送信失敗時は下書きが Pending のまま残り再押下でリトライできる。下書きが失われた後の押下（再起動・TTL・FIFO 追い出し）は「期限切れ」通知になり、ボタン `value` に埋め込まれたスレッド座標（#121）があれば **元メンションスレッド内のエフェメラル** が第一面、`response_url`（押下面）は座標なし・投稿失敗時のフォールバック。本人名義返信の必須防波堤として [ADR-0003](/decisions/adr-0003-slack-reply-assistant.md) で決定（実装は [task-source-slack](/components/task-source-slack.md) の `draft` / `approval` モジュール、#107）。

**提示面は当初 2 面だった**（スレッド内エフェメラル + self-DM 記録）が、[ADR-0074](/decisions/adr-0074-single-draft-surface.md) で 1 面に減らした。**ボタンのある面が 2 つあると、押下後の後始末が片方だけ成功しうる** —— 押した面は `response_url`、もう一方は `chat.update` という別々の呼び出しで、実機では「却下したのに DM 側のボタンだけ消えない」として出た。同期を堅くするのではなく、面を 1 つにして問題の種類ごと消した。代償は永続的な記録面を失うことで、埋まるのは一部である（bot DM のナッジが返信案の本文をログとして持つので「何を送ろうとしていたか」は残るが、「承認したのか却下したのか」は残らない）。

エフェメラルは **Slack 通知を発生させない**ので、`bot_token` 設定時は提示と同時にナッジ専用 bot が本人へ通知 DM（permalink 付き）を送る（[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)、#305）。ナッジは通知フィードなのでボタンは持たないが、**下書きのナッジだけは承認/却下で 1 度編集される** —— それが上の「削除してよい根拠」であり、ピッカーのナッジには適用されない。
