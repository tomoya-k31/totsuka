---
type: Term
title: PR タスク
description: GitHub Project 上の PullRequest item から取り込まれた Task。id は PR の node id で、その PR を生んだ issue のタスクとは別物。branch hint に PR の head ブランチを持ち、design はその先頭 commit に detached な worktree で追加修正を設計し、implement はそのブランチ上で commit を積む。成果物はどちらも PR へのコメント。
tags: [glossary, domain, github, pull-request]
generated: { by: claude-code/fable-5-1, at: 2026-09-21T02:20:00+09:00 }
status: stable
owner: tomoya-k31
---

# PR タスク

GitHub Project のボードに載った **PR の item** から取り込まれた [Task](/glossary/task.md)。issue から取り込まれたタスクと同じ状態機械を通るが、次の 3 点が違う。

| | issue のタスク | PR タスク |
|---|---|---|
| `Task.id` | issue の node id | **PR の node id** |
| 開始位置 | 既定ブランチに detached。ブランチ名はエージェントが付ける | [branch hint](/glossary/branch-hint.md) = PR の head ブランチ。implement はその**上**、design はその先頭に **detached** |
| implement の成果物 | 新しい PR（その URL を報告） | 既存 PR への commit と、変更内容の**コメント**（そのコメントの URL を報告） |

受けるのは `profile = "design"` と `"implement"` の workflow だけで、opt-in の設定は無い。OPEN でない PR と fork からの PR は取り込まれない（[task-source-github](/components/task-source-github.md)）。

## 「item」とは呼ばない

「item」はボード上のカードを指す GitHub の語（`ProjectV2Item`）で、issue も PR も draft も item である。totsuka のタスクを指して「PR item」とは言わない —— item は取り込みの**入力**、PR タスクはその**結果**である。

## その PR を生んだ issue のタスクとは別物である

issue #10 を implement が実装して PR #12（`Closes #10`）を開いたとき、あとから PR #12 をボードに載せると、それは issue #10 のタスクの続きではなく**新しい PR タスク**になる。統合しないのは、GitHub 上の issue と PR の結びつきが `Closes` が無ければ構造的に存在しないためである（[ADR-0085](/decisions/adr-0085-branch-hint.md) 決定 6）。

実用上の帰結が 1 つある。**totsuka が作った PR への追加修正は、issue のカードを trigger 列へ戻すほうが正しい。** 同じタスクが同じブランチ・同じエージェントセッションで再開される（[会話継続](/glossary/conversation-continuity.md)）。PR タスクが意味を持つのは、totsuka のタスクから生まれていない PR —— 依存更新ボットの bump や、人間が開いた PR —— である。

## design の成果物はレビューではない

PR タスクの design が書くのは「この PR をマージ可能にするために、**追加で**何を変えるべきか」の設計で、差分のコードレビューではない。レビューには Copilot・`/code-review`・人間の 3 系統が既にある。設計コメントは、後段の implement が issue の設計コメントを読むのと同じように読む。
