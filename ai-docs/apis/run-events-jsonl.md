---
type: Event
title: run --events-jsonl（通知を stdout の JSON 行で受け取る）
description: "メニューバーアプリが子プロセスとして起動した run から通知を受け取る経路（ADR-0113）。--events-jsonl を付けると stdout の各行が type 付きの JSON 1 つになり、notifier プラグインへ送るのと同じ notify の中身（type=notify）と最後の要約（type=summary）が流れる。notifier プラグインは起動しない。--json と --dry-run とは併用できない。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/run/mod.rs
tags: [api, event, cli, notifier, menubar, macos, jsonl]
generated: { by: claude-code/opus-5.5, at: 2026-10-01T22:41:00+09:00 }
status: stable
owner: tomoya-k31
---

# 概要

`totsuka run --events-jsonl` は、通知を **stdout に 1 行 1 JSON** で書く。メニューバーアプリ（[ADR-0113](/decisions/adr-0113-native-menubar-app.md)）は `run` を子プロセスとして起動し、このパイプを読んでアプリ名義の通知を出す。

- **notifier プラグインは起動しない。** 親が通知者なので、起動すると同じ出来事が 2 回通知される。`[macos]` などの絞り込み（F-92）は親が読んで適用する
- **`--json` と `--dry-run` とは併用できない**（clap が拒否する）。`--json` の stdout は要約 1 文書、`--dry-run` の出力は stdout の 1 文という契約だから
- stdout にほかの文は出ない。要約も最後の 1 行として JSON で出る。ログと人向けの文は従来どおり stderr とログファイル

# Schema

各行は JSON オブジェクト 1 つで、`type` で種類を見分ける。知らない `type` は読み飛ばすこと（行の種類は増えうる）。

## `type: "notify"`

notifier プラグインへ送る `notify` の params（`NotifyParams`）と同じフィールド:

| フィールド | 型 | 意味 |
|---|---|---|
| `event` | string | `waiting_input` / `done` / `failed` / `pending` / `escalated` / `verification_pending` |
| `task_id` | string? | タスクの ID（`totsuka focus` や `/focus` に渡すもの）。プラグインの異常など、タスクに紐づかない通知には無い |
| `workflow` | string? | ワークフロー名（絞り込み用）。タスクに紐づかない通知には無い |
| `title` | string | 見出し |
| `body` | string? | 本文 |

```json
{"type":"notify","event":"waiting_input","task_id":"7","workflow":"implement","title":"Fix the login bug","body":"…"}
```

## `type: "summary"`

`run` が終わるときに 1 回だけ、`--json` の要約（`RunSummary`）のフィールドに `type` を足して書く。`--watch` なら停止したとき。

# 呼び出し側の契約

- 1 行ずつ書いてすぐ flush する。行の途中で切れることはない
- 書き込みの失敗（親がパイプを閉じた等）は無視され、タスクの実行に影響しない（F-93 の fire-and-forget と同じ）
- **親は stdout を読み続けること。** 書き込みはブロッキングなので、親が読むのをやめてパイプが詰まると、通知を出した処理がそこで止まる
- 発火する場所は notifier への配送と同じ 1 か所（`deliver_notification`）なので、notifier に届く通知はすべてここにも出る

# 関連

- [ADR-0113](/decisions/adr-0113-native-menubar-app.md)
- [orchestrator-cli](/components/orchestrator-cli.md)（`run`）
