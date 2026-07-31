---
type: Term
title: pane（ペイン）
description: エージェント CLI が実際に動くターミナル区画（herdr の pane）。dispatch 時に worktree を cwd、label を totsuka + source task id として作られ、pane_control capability 越しの session/focus・session/release・session/list で制御され、寿命は worktree の掃除ポリシーに連動する。
tags: [glossary, herdr, pane, pane-control]
generated: { by: claude-code/opus-5, at: 2026-07-31T09:54:29Z }
status: stable
owner: tomoya-k31
---

# pane（ペイン）

[Agent IDE](/glossary/agent-ide.md)（herdr 等）が管理する**ターミナル区画**で、[dispatch](/glossary/dispatch.md) されたタスクの [AI ツール](/glossary/ai-tool.md) が実際に走る場所。人間が「いま何が起きているか」を目で見る唯一の面でもあり、[worktree](/glossary/worktree.md) が「タスクのファイル側の実体」なのに対し pane は「タスクの画面側の実体」にあたる。

herdr プラグインは dispatch 時に `workspace.create`（`cwd` = そのタスクの worktree、`label` = `totsuka {task.id}`）で新しい workspace を作り、その中で `agent.start` によりエージェント CLI を起動する。ここでの `task.id` はプロトコルの `Task.id` = **source task id**（Slack なら `"C1:1.0"` 等）であって状態 DB の行 id ではない。この `totsuka ` 前置が「totsuka が所有する pane」の境界であり、ユーザーが手で開いた無関係な pane と区別する唯一の手掛かりになる（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）。

# 制御と capability

Orchestrator は pane を直接知らない。pane を触る操作はすべて `pane_control` capability を宣言した agent_ide プラグイン越しの RPC で、session id は不透明のまま扱われる（F-37）:

| RPC | protocol | 用途 |
|---|---|---|
| `session/focus` | 0.1.4 | [click-to-focus](/glossary/click-to-focus.md) で該当 pane を前面化する（F-94） |
| `session/release` | 0.2.1 | worktree 削除に連動して pane を閉じる（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)） |
| `session/list` | 0.2.2 | 所有 pane を列挙して孤児を検出する（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)） |

3 つとも専用フラグを新設せず `pane_control` に相乗りしている（focus も release も list も「pane 表面の制御」で分離する意味がない）。宣言しないプラグイン（orca 等）では単に呼ばれず、Orchestrator は pane 操作をスキップして静かに縮退する。

herdr の pane id（`w34:p2`）は**位置ベース**で、閉じた pane の id が別の pane に再利用されうる。そのため release / 解放系の RPC は `expect_cwd`（worktree パス）や `expect_label` を同一性ガードとして渡し、列挙から実行までの間に id が付け替わるレースを弾く。

# 寿命と孤児

pane の寿命は worktree の掃除ポリシーに連動する。掃除は「**判定** → **pane 解放** → **worktree 削除**」の 3 段で、`Remove` と判定されたときだけ pane を閉じる。`Retain` / `Dirty`（未コミット変更あり）では worktree も pane も残す — 人間が中身を確認する導線を壊さないため（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）。

この連動は手動 `git worktree remove`・同一性検証の不一致・プラグインのクラッシュなどで破れる。取り残された pane は**孤児 pane** と呼び、`totsuka doctor` が `session/list` の結果と状態 DB を突き合わせて「DB 未知」「終端タスクかつ worktree 消滅」の 2 基準で検出し、TTY では 1 件ずつ確認して解放を提案する（無人自動削除はしない）。

# pane から取れないもの

pane は**画面のコピーしか返さない**。`pane.read` はスクロールバックを持たず、source（`visible` / `recent` / …）を問わず表示範囲に限られるため、**エージェントの最終出力を pane から回収することはできない**。完了検知は Claude Code のフックシグナル、最終出力はエージェント自身の会話ログ（`agent_session` の session id から辿る）が正になる。pane から best-effort で読むのは `blocked` 時の質問文抽出（`waiting_input`、F-35）などに限られる。詳細は [herdr Socket API リファレンス](/references/herdr-socket-api.md)。
