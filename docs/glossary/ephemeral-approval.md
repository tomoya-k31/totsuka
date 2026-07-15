---
type: Term
title: エフェメラル承認フロー
description: エージェントの返信案をスレッド内エフェメラル + self-DM 記録の 2 面に提示し、承認ボタン押下時のみ本人名義で送信する task-source-slack の仕組み。勝手に送信しないための防波堤。
tags: [glossary, slack, approval, ephemeral]
timestamp: 2026-07-15T15:00:00Z
status: active
owner: tomoya-k31
---

# エフェメラル承認フロー

task-source-slack の `result/publish` が、エージェントの返信案（下書き）を即送信せず、①メンションスレッド内の **エフェメラルメッセージ**（本人にだけ見える）と ② **self-DM への記録**（再起動後もテキストが残る永続面）の 2 面に Block Kit で提示し、**承認して返信** ボタン（confirm ダイアログ付き）の押下時のみ本人名義（`xoxp-` トークン）でスレッド返信する仕組み。却下は送信せず破棄し、両面が ✅/❌ の最終状態に更新される。二重押下は「処理済み」通知で弾かれ、送信失敗時は下書きが Pending のまま残り再押下でリトライできる。本人名義返信の必須防波堤として [ADR-0003](/decisions/adr-0003-slack-reply-assistant.md) で決定（実装は [task-source-slack](/components/task-source-slack.md) の `draft` / `approval` モジュール、#107）。
