---
type: Decision
title: ADR-0109 Slack のメンション経路で bot と人間を区別しない
description: bot 投稿の個人メンション・グループメンションも人間の投稿と同じくタスクにする決定。判定表①は「bot_message 以外の subtype」と「自アプリの bot」だけを落とし、user の無い古典的 bot 投稿は bot_id を送信者にする。ADR-0079 が退けた「メンション経路を緩める」案を採り直すもので、Gateway も同じ緩和を入れるが wire schema は据え置く。
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/mention.rs
tags: [decision, slack, mention, bot, gateway, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-03T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[ADR-0079](/decisions/adr-0079-reaction-on-bot-posts.md) の「検討した選択肢 — bot 投稿を自動でタスク化する（メンション経路を緩める）」の不採用を**メンション経路について**覆す。ADR-0079 の決定（リアクション経路の `from_bot`）はそのまま有効で、チャンネル監視経路は本 ADR でも変えない。

# Context

承認フローなどの bot が `<!subteam^S…>` で操作者の所属グループを名指しして投稿しても、Workflow が起動しなかった。メンション判定表①が `subtype` / `bot_id` を持つ投稿を一律に捨てており、同じ除外が Gateway（`services/slack-event-gateway/src/project.rs`）と契約モジュール（`gateway_contract.rs`）にも重複して掛かっていたためである。

ADR-0079 はこれを意図した挙動とし、bot 投稿を拾う手段をリアクション（操作者が 1 件ずつ選ぶ）に限った。運用者は「メンション元が人か bot かで区別しない」ことを選んだ。人間が書いたメンションと bot が書いたメンションは、名指しされた側から見て同じ依頼だからである。

# Decision

## 1. 判定表①が落とすのは 2 種類だけ

- **`bot_message` 以外の `subtype`**（編集・削除・システム投稿）。広がるのは投稿者であってイベント種別ではない（ADR-0079 の `from_bot` と同じ線引き）
- **自アプリの bot の投稿**。承認カードや監視結果はメンション本文を引用して bot 名義で投稿されるので、通すとループする

それ以外の bot は人間と同じ扱いで、判定表②以降（本人の投稿・self-DM・名指し判定・workflow・dedup）をそのまま通る。

## 2. 自アプリの `bot_id` は TokenGuard の bot `auth.test` から取る

`bot_token` があるときだけ `initialize` が bot トークンで `auth.test` を呼んでいたので、その応答の `bot_id` を `MentionFilter` に渡す。API 呼び出しは増えない。`bot_token` が無ければ bot 名義の投稿自体が無いので、除外も要らない。

## 3. `user` の無い古典的 bot 投稿は `bot_id` を送信者にする

`subtype: bot_message` の投稿は `user` を持たない。ADR-0079 決定 4 と同じく `bot_id` を `Mention.user` に入れ、表示名は解決しない（pane には `B…` が出る）。アプリ bot の投稿は `user`（bot ユーザー）を持つので、そちらを使う。

## 4. Gateway も同じ緩和を入れる。wire schema は据え置く

Gateway が publish しなければプラグインは何も受け取れないので、`project_message` に同じ規則を入れ、適合テスト（`contracts/slack-event-gateway/cases/`）を更新する。レコードの形は変わらない（`user` に `B…` が入りうるだけ）ので `SCHEMA_VERSION` は上げない。古いプラグインが新しい Gateway のレコードを受けても、再取得した本文の `bot_id` で捨てるだけで壊れない。

Gateway は自アプリの `bot_id` を知らないので、自アプリの投稿も publish する。プラグインが再取得後に判定表①で落とす（1 件ぶん余分に `fetch_message` が走るだけ）。

**`event_source = "gateway"` で効かせるには Gateway の再デプロイが要る。** Socket Mode はプラグインの変更だけで効く。

# Consequences

- ADR-0079 が挙げた代償を受け入れる: bot 投稿で pane が無人で立ち上がる。第三者（bot の投稿元）が書いた本文がエージェントの入力になる。止めたいときは、その bot がメンションするグループの `to_group` route を外すか、グループから抜ける
- bot 同士の応酬（別の totsuka の bot が互いにメンションし合う等）は、`channel:ts` の dedup と workflow の承認ゲートに任せる
- チャンネル監視経路は bot 投稿を引き続き落とす。監視結果は自アプリの bot が投稿するので、その除外が監視のループ防止そのものだからである（ADR-0068）
