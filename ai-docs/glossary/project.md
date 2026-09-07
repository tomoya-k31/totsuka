---
type: Term
title: Project（プロジェクト / domain）
description: ソースが持つ、名前で指せる管轄単位（config.toml の [[projects]]）。github はボード、notion はデータベース、slack / discord はワークスペース / ギルド。[[workflows]].projects が取り込み元として、[[repositories]].project が起票先として指す。GitHub Projects の Project とは別の概念。
tags: [glossary, config, domain]
generated: { by: claude-code/opus-5, at: 2026-09-07T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Project（プロジェクト / domain）

**1 つのタスクソースが持つ、名前で指せる管轄単位**（`config.toml` の `[[projects]]`）。何が domain になるかはソース次第で、[task-source-github](/components/task-source-github.md) なら GitHub Project のボード、[task-source-notion](/components/task-source-notion.md) ならデータベース、[task-source-slack](/components/task-source-slack.md) / [task-source-discord](/components/task-source-discord.md) ならワークスペース / ギルドである。後者 2 つは今のところ singleton なので、エントリはキーを持たず `name` と `source` だけになる。

core が読むのは `name` と `source` の 2 キーだけで、残りは所有プラグインのものとして**無解釈で** `initialize` へ渡す（#554、[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）。

## 2 方向から指される

| 指す側 | 意味 | arity |
|---|---|---|
| `[[workflows]].projects` | **取り込み元**。この workflow が走査する domain（#626、[ADR-0069](/decisions/adr-0069-workflow-projects.md)） | 配列（必須・空不可） |
| `[[repositories]].project` | **起票先**。このリポジトリのタスクを起票するトラッカー（#554） | スカラー（任意） |

arity が違うのは意図的である。リポジトリ側のスカラー性は「2 枚のボードが 1 つのリポジトリを主張する」状態を表現不能にしており、`ClaimConflict` の検出機構ごと削除できた根拠になっている。workflow 側の配列は「**これらの domain は同じレーン語彙を共有する**」という主張で、`source` が全ボードを暗黙に含んでいたのを列挙に変えたものである。同じ `project` という語で arity が違うと読み間違えるので、キー名を複数形にして arity を名前に出している。

## GitHub Projects の Project ではない

キー名の語源はそこだが、指す対象は広い。`[[projects]]` に `source = "slack"` のエントリが並ぶのは正常な状態である。[ADR-0062](/decisions/adr-0062-status-vocabulary.md) は `trigger.project_status` を `status` へ改名する際に `project` を「GitHub Projects の語」と認定したが、`[[projects]]` 自体の改名は ADR-0069 で見送った（`[[repositories]].project` まで含めた改名の diff に見合わないため）。**語源ではなく上の定義で読むこと。**

## workflow の `source` は導出値である

`[[workflows]]` に `source` は無い（#626 で廃止）。タスクソースは名指された domain の所有者で、`projects → [[projects]].source` の 1 ホップで解ける。`[[workflows]].projects` が複数の source をまたぐのはエラー —— 未知キーの引き取り（#554）が「source と agent のちょうど 1 つが引き取る」規則なので、source が 2 つあると claimant が一意に決まらない。

参照連鎖 `[[workflows]].projects` → `[[projects]].name` → `[plugins.<source>]` は**プラグインを起動せずに辿れる**ので、壊れた参照は `totsuka config validate --offline` で落ちる。
