---
type: Decision
title: ADR-0013 孤児 pane の検出は session/list（label 所有権フィルタ）+ doctor の対話的解放で行う
description: doctor の孤児 pane 検出（#211）のため protocol 0.2.2 で列挙 RPC session/list を追加し、所有権はプラグイン側の label 前置（totsuka {task_id}）フィルタで絞る決定。孤児判定は「DB 未知」+「終端タスクかつ worktree 消滅」の 2 基準、解放は session/release を列挙した label を expect_label に詰めて呼ぶ。生存確認方式（DB 既知 id の照会）は DB 未知 pane を原理的に見つけられないため不採用。
tags: [protocol, pane, doctor, orphan, herdr, pane-control]
timestamp: 2026-07-23T14:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-23（[#211](https://github.com/tomoya-k31/totsuka/issues/211)。[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md) の後続）

# Context

#210（ADR-0010）で「worktree を削除するときに pane も閉じる」連動を導入したが、この連動は破れる経路が複数ある: 運用ガイドが案内する手動 `git worktree remove`（totsuka が関与せず pane が残る）、`session/release` の同一性検証不一致による skip（degrade 規則）、プラグインのクラッシュ、#210 以前に完了したタスクの残骸（実機で複数確認済み）。

worktree 側には `doctor` の `check_orphans`（F-24）という受け皿があるが、pane には無い。そして worktree と違い「ファイルシステム側の真実」に相当する列挙手段がプロトコルに存在しなかった — orchestrator は agent プラグインに「持っている pane を全部教えろ」と聞けない。一方、調査の結果 herdr 自体は `pane.list` / `workspace.list` を公開していることが確認済みで（[herdr-socket-api](/references/herdr-socket-api.md)、0.7.4 実機）、プラグインが呼んでいないだけだった。

# Decision

1. **protocol 0.2.2 で列挙 RPC `session/list` を追加する**（additive）。params は空、result は `{ sessions: [{ session_id, label?, cwd? }] }`。`session_id` は `task/dispatch` が返すのと同じ不透明形式で、そのまま `session/release` に渡せる。capability は ADR-0010 の前例に従い **`pane_control` 相乗り**（新フラグなし — 列挙も解放も「pane 表面の制御」で分離する意味がない）。
2. **所有権フィルタはプラグイン側の label 前置で行う**。herdr 実装は `pane.list` を呼び、`label` が `totsuka ` で始まる pane（dispatch 時に `workspace.create` へ設定する `totsuka {task.id}` — この `task.id` はプロトコル `Task.id` = **source task id**（Slack の `"C1:1.0"` 等、DB 行 id ではない）である点に注意）だけを返す。herdr はユーザーが手で開いた無関係な pane も持つため、**このフィルタが列挙の安全境界**であり、label 規約を知るプラグイン側に置く（orchestrator に herdr の label 形式を漏らさない — ADR-0010 §3 と同じ層の判断）。返す `session_id` は pane_id + 空 agent_session の縮退形（`pane.list` は中の Claude セッションを知らないが、`session/release` は pane さえ分かれば良い）。
3. **代替案「生存確認方式」は不採用**: orchestrator が DB の既知 session_id 群を渡してプラグインが生死を返す設計は、**DB に無い pane を原理的に見つけられない**（クラッシュした dispatch・消えた行・#210 以前の残骸が正に検出したい対象）ため、列挙方式を採る。
4. **孤児判定は doctor 側で 2 基準**（`classify_orphan_panes`、純関数）。突き合わせは label の source task id と `TaskRecord.source_task_id` の**文字列一致**（source_task_id は source 内でのみ一意のため、一致する**全**タスクを見て保守側に倒す — 1 つでも非終端があれば保持）:
   - **DB 未知** — label の source task id がどのタスクにも対応しない（真の孤児）。
   - **終端タスクかつ worktree 消滅** — 一致するタスクがすべて terminal（Done/Failed/Cancelled）で、live な worktree を持つものが無い（#210 の連動が破れたケースの回収経路）。
   - 候補に**しない**もの: 非終端タスクの pane（使用中）と、終端でも worktree が保持ポリシー（`keep_7d` 等）で残っている pane（pane の寿命は worktree に連動 — ADR-0010）。
5. **解放は `session/release` を `expect_label` 付きで呼ぶ**。列挙で得た label をそのまま同一性ガードに使う（ADR-0010 §3 で予約されていた拡張点の初活用）。列挙→対話確認→解放の間に位置ベースの pane id が別 pane に付け替わるレースを弾く。worktree は消滅済みのことが多いため `expect_cwd` は送らない。
6. **doctor は提案するだけ**（孤児 worktree の既存方針踏襲）: TTY のときのみ 1 件ずつ `[y/N]` で確認し、`--json` / 非 TTY では `panes` チェックの fail として検出のみ報告（無人自動削除はしない）。`pane_control` な agent_ide プラグインが 1 つも無い構成ではチェック自体を出さない（orca 構成にノイズを出さない）。列挙の失敗は warning に留め、他のチェックを止めない。

# Consequences

- #210 の連動が破れて残った pane（手動 worktree 削除・解放拒否・クラッシュ・既存残骸）を `totsuka doctor` で発見・対話的に回収できる。孤児 worktree と対称の運用導線になる。
- herdr プラグインに `pane.list` の呼び出しが初めて入る（herdr 本体の変更は不要）。mock plugin は `list_sessions` 設定でテスト用の pane 一覧を staging できる。
- `session/list` は additive のため既存 `<0.3` manifest はそのまま互換。orca は `pane_control` を宣言しないため呼ばれない（プラグイン変更なし）。
- label 前置 `totsuka ` がプラグインの所有権境界として意味論を持つようになった。label 形式を変える場合は dispatch（設定側）と `session/list`（フィルタ側）を同時に変える必要がある（どちらも herdr プラグイン内で閉じる）。
- doctor は agent プラグインを `check_plugins` の validate probe とは別にもう一度起動する（launch → `session/list` → shutdown）。診断コマンドのため許容。
