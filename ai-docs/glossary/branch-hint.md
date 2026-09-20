---
type: Term
title: branch hint（ブランチヒント）
description: タスクソースが Task に添える「この仕事が属する既存のブランチ」の名前（Task.branch_hint、protocol 0.7.5）。ソースは名前を言うだけで、implement のステージはそのブランチ上に、plan のステージはその先頭 commit に detached で worktree を作るという使い分けは core が持つ。repo hint と違って助言ではなく、honour できなければタスクは失敗する。
tags: [glossary, git, worktree, protocol]
generated: { by: claude-code/fable-5-1, at: 2026-09-21T01:20:00+09:00 }
status: stable
owner: tomoya-k31
---

# branch hint（ブランチヒント）

タスクソースが `Task.branch_hint` に入れる、**既存のブランチ**の名前。「この仕事は新しいブランチではなく、既にあるこのブランチの続きである」とソースが知っているときにだけ入る。典型は PR の head ブランチである。`None` が通常で、その場合の挙動はこのフィールドができる前と同じ（既定ブランチに detached な [worktree](/glossary/worktree.md) を渡し、ブランチ名はエージェントが付ける）。

対になる語は repo hint（`Task.repo_hint`）で、あちらが「どのリポジトリで」、こちらが「どのブランチで」を言う。ただし性質が 1 つ違う。

| | 解決できなかったとき |
|---|---|
| repo hint | 助言。LLM によるリポジトリ選択へ落ちる |
| **branch hint** | **助言ではない。** タスクを失敗させる |

branch hint をフォールバックさせないのは、別の場所から仕事を始めると、同じ変更に対する 2 本目の PR が開くからである（[ADR-0085](/decisions/adr-0085-branch-hint.md) 決定 3）。

## ソースが言うこと、core が決めること

ソースが言うのはブランチ名だけである。それをどう使うかは、ステージのモードを見て core が決める。

| モード | worktree |
|---|---|
| implement | そのブランチ**上**。そこに commit して push する |
| plan | そのブランチの先頭 commit に **detached**。ブランチには乗らない |

plan で乗せないのは、read-only プロファイルの worktree が名前付きブランチ上で見つかると「エージェントが git を実行した」と解釈されてタスクが失敗するためである（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md)）。ソースプラグインは profile もモードも知らないので、この使い分けをプラグイン側には置けない。

## 記録されたブランチとの違い

`tasks.branch` に入る「記録されたブランチ」は、エージェントが自分で作ったブランチを core が `HEAD` から読み取ったものである。こちらは消えていれば detached へフォールバックする（名前がもう何も指していないので、付け直させる）。branch hint は外から与えられた「これが仕事の対象だ」という主張なので、同じ寛容さを持たせない。

名前は `origin` に対してだけ解決する。fork の head のように `origin` に無いブランチについて、ソースは branch hint を入れてはならない。
