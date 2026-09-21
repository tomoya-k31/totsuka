---
type: Term
title: Trigger（トリガー）
description: ワークフローがどのタスクを取り込むかを決める条件（[[workflows]].trigger）。解釈するのはソースプラグインで、github / notion では取り込み条件（キー同士は AND、配列は OR）と除外条件 exclude（どれか 1 つに一致したら取り込まない）の 2 つからなる。
tags: [glossary, domain, trigger, workflow]
generated: { by: claude-code/opus-5, at: 2026-09-21T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Trigger（トリガー）

[ワークフロー](/glossary/workflow.md)が**どのタスクを取り込むか**を決める条件。`[[workflows]].trigger` に書く。意味を与えるのは[タスクソース](/glossary/task-source.md)のプラグインで、Orchestrator は中身を解釈しない（#554。例外は閉路検査のために読む `status` だけ → [ADR-0062](/decisions/adr-0062-status-vocabulary.md)）。

github / notion のトリガーは 2 つの部分からなる:

- **取り込み条件** —— タスクが満たすべきこと。キー同士は AND、1 つのキーの配列は OR。例: `status = "Todo"`、`label = ["bug", "chore"]`、`assignee = "@none"`
- **除外条件（`exclude`）** —— 満たしたら取り込まないこと。取り込み条件と同じ語彙で、**どれか 1 つに一致したら除外**する（[ADR-0091](/decisions/adr-0091-trigger-exclude.md)）。例: `exclude = { label = "waiting" }`

どちらも**取り込みの時点**で評価する。取り込んだ後に条件が変わっても、実行中のタスクには影響しない。

トリガーの外にも取り込みを弾くものがある —— ボードの「実行中」列（`in_progress_statuses`、F-08）とボードに紐づかないリポジトリである。どちらもボードの事実で、workflow が書く条件ではないので、トリガーには含めない。

slack / discord のトリガーは起動の種別そのもの（メンション・リアクション・[チャンネル監視](/glossary/channel-watch.md)）を選ぶもので、取り込み条件・除外条件という形を取らない。

語彙の一覧は [設定リファレンス](/development/config-reference.md) の「`trigger` の語彙」節にある。
