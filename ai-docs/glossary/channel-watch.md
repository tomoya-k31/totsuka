---
type: Term
title: チャンネル監視トリガ（channel watch）
description: "特定チャンネルへのトップレベル投稿そのものをトリガにして 1 投稿 = 1 タスクを起こす仕組み。メンションもリアクションも要らないぶん「投稿できる人」が実行できる人になるため、既定の起動者は操作者本人だけで、trigger.from が唯一の明示的な緩和口になる。会話継続の対象外。"
resource: https://github.com/tomoya-k31/totsuka/issues/615
tags: [glossary, trigger, channel-watch, slack, discord, security]
generated: { by: claude-code/opus-5, at: 2026-09-06T04:41:00+09:00 }
status: stable
owner: tomoya-k31
---

# 定義

チャットソース（Slack / Discord）の**特定チャンネルへのトップレベル投稿**を、そのままタスクの起動ジェスチャとして扱うトリガ。`[[workflows]].trigger` に書く:

```toml
[[workflows]]
name = "clip"
source = "slack"
agent = "herdr"
profile = "implement"
initial_prompt = "/clip-doc 貼られた URL の記事を ai-docs に残してください"
trigger = { channel = "C0123ABC", channel_name = "clip", repo = "my-docs" }
```

代表的な用途が **clip チャンネル**（記事 URL を貼ると、その内容をドキュメントとしてリポジトリに残す）で、`clip` は機能名ではなくこの設定例の名前である。

# 他のトリガとの違い

| | メンション | リアクション | チャンネル監視 |
|---|---|---|---|
| ジェスチャ | `<@自分>` を書く | 絵文字を付ける | **投稿する** |
| 起動できる人 | 自分以外の誰でも | 操作者本人のみ | 既定で操作者本人のみ（`from` で拡張） |
| タスク同一性 | 1 スレッド = 1 会話 = 1 タスク | 反応した投稿ごと | **1 投稿 = 1 タスク** |
| リポジトリ | 3 段階解決（prefix → LLM → 人間） | 同左 | `trigger.repo` で固定 |

**[会話継続](/glossary/conversation-continuity.md) の対象外**である点が最も重要な差。監視対象はトップレベル投稿だけで、スレッド返信は拾わない — 生成されたドキュメントへの「ありがとう」で 2 本目のタスクが走らないようにするため。

# 起動者が境界である理由

投稿することがジェスチャの全部なので、**チャンネルに投稿できる人 = 実行できる人**になる。これは「操作者本人のリアクションしか受け付けない」というリアクショントリガの不変条件（[ADR-0025](/decisions/adr-0025-reaction-task-trigger.md)）を正面から破るため、[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md) で明示的に扱った:

- 既定は**操作者本人の投稿だけ**
- 緩和は `trigger.from = ["<user id>", …]` という**明示的な allowlist だけ**（「チャンネル参加者全員」を意味する設定は存在しない）
- 操作者は `from` の内容に関わらず常に許可される

# 付随する 3 つの決定

- **`channel` は id が正、`channel_name` の併記が必須**。名前は改名で黙って別チャンネルを指しうるので、起動時に実名と照合して不一致を警告する
- **結果は bot 名義で投稿**する（承認ゲートを通さない）。監視は「誰かが投稿したこと」で発火するので、その結果を操作者の名前で出すのは、承認ゲートが本来防いでいる形になる
- **起動時に backfill する**。落ちている間の投稿は取りこぼされ、しかも貼った本人は「残ったつもり」でいる。台帳が重複を無害化するので、カーソルを持たず「直近 N 件かつ年齢上限以内」を毎回投げ直す（→ [起動時バックフィル](/glossary/startup-backfill.md)）

# 関連

- [ADR-0068](/decisions/adr-0068-channel-watch-trigger.md) — 決定と不採用案
- [Workflow](/glossary/workflow.md) — トリガを含む束ね
- [task-source-slack](/components/task-source-slack.md) / [plugin-sdk](/components/plugin-sdk.md) — 実装
