---
type: Decision
title: ADR-0026 ブランチ命名と push・PR 作成をエージェントの責務にする
description: リポジトリごとのブランチ命名規約に従うため、worktree を detached で引き渡してエージェントに命名させ、あわせて push・PR 作成の責務（F-86）を撤回した記録。取得手段・同期契機・新設したデータ損失経路への蓋・失うものを含む。
tags: [worktree, branch, git, output-policy, adr]
status: stable
generated: { by: claude-code/opus-5, at: 2026-07-31T00:00:00Z }
owner: tomoya-k31
---

# Status

stable。[#338](https://github.com/tomoya-k31/totsuka/pull/338) /
[#339](https://github.com/tomoya-k31/totsuka/pull/339) /
[#340](https://github.com/tomoya-k31/totsuka/pull/340) の 3 PR で実装。
[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md) の掃除判定に分岐を 1 つ足し、
[ADR-0024](/decisions/adr-0024-agent-instruction-layers.md) の安全表を 1 行降格させる。

# Context

worktree のブランチ名は `agent/{source}-{task_id}` を Orchestrator が生成していた
（F-21）。この名前は 3 つを同時に保証していた: **一意性**（task_id が一意）、
**所有権**（`agent/` 名前空間に人間は入ってこない）、**再構築可能性**（`(source,
task_id)` からの純関数）。掃除の `git branch -D` が名前を一切検査せずに済んでいたのも、
`delete_branch_if_published` に他の値が入ってこないという事実に依存していたためである。

問題は、その名前が**タスクの内容を何も語らない**こと、そして**多くのリポジトリが
自前のブランチ命名規約を持つ**ことだった。このリポジトリ自身の
`.claude/rules/git-conventions.md` が `<type>/<slug>`, type ∈
`feat|fix|docs|style|refactor|perf|test|chore|revert` を定めている。**規約は
リポジトリの中に書かれている**以上、Orchestrator が生成した名前がそれに従うことは
原理的にありえない。

# Decision

## 1. ブランチはエージェントが命名し作成する

worktree は `git worktree add --detach <path> <base_commit>` で detached HEAD の
まま引き渡す。Orchestrator は名前を一切生成しない
（`DEFAULT_BRANCH_TEMPLATE` / `render_branch` / `EngineSettings.branch_template` は削除）。
規約を読めるのは worktree の中にいるエージェントだけであり、worktree はリポジトリの
チェックアウトそのものなので、CLAUDE.md / AGENTS.md / CONTRIBUTING.md /
`.claude/rules` はそのまま読める。

## 2. 取得手段は `HEAD` の読み戻し

`WorktreeManager::head_branch`（`git rev-parse --abbrev-ref HEAD`）。

エージェント → totsuka の構造化チャネルは**存在しない** — あるのは
`<<STATUS:...>>` マーカーの 4 値と `last_assistant_message` の不透明な markdown
だけで、結果ファイルも JSON 出力規約も、エージェントが呼べる CLI も無い。新設すると
claude / codex / opencode の 3 実装が要るうえ、「正しく申告する」という信頼に
寄りかかることになる。`HEAD` は構成上そのどちらも要らない。

## 3. 同期の契機は 4 つ

| 契機 | 何を救うか |
|---|---|
| Stop 全種（`run/hooks.rs`） | retry の直前。`NeedsInput` → 人間の返信 → 再ディスパッチは Slack の常道で、未記録だと再作成に回って自分のディレクトリと衝突する |
| `finalize_success` の冒頭 | **hook を持たないエージェントは Stop を一切送らない**。orca やモックは `state/notification` で完了する |
| `cleanup_worktree` の冒頭 | cancel、別プロセスが終わらせたタスクを sweep が拾う経路 |
| 60 秒 worktree sweep | クラッシュ / SIGTERM / `sweep_signal_timeouts` のエスカレーション / heartbeat を持たない codex・opencode |

冗長ではない。**`PostToolUse` フックは存在せず**、エンジンが認識するイベントは
`Stop` / `Notification` / `SessionStart` / `SessionEnd` の 4 つだけで
（`adapters/hook_uds.rs`、それ以外は `Heartbeat` に潰れる）、「エージェントが
`git switch -c` した瞬間」を捉える手段は無い。上のどれか 1 つでも欠けると壊れる
経路がある。

## 4. 名前のゲートは置かない

`agent/` プレフィックス強制も、create 時スナップショット差分も採らない。
プレフィックス強制は**リポジトリの規約に従うという目的そのものを壊す**。

代わりに、掃除の破壊的操作にだけ**由来判定**を置く:
`git merge-base --is-ancestor <base_commit> <branch>` が偽なら削除しない。
「origin のどこにも無いコミットが無い」という既存の条件は*そのブランチが誰のものか*を
何も言っておらず、名前が運用者と同じ名前空間（`feat/`, `fix/`）に入った以上それでは
足りない。古い既定ブランチから切られた人間のブランチはこのタスクの起点を含まない。
`base_commit` は `create` が既に計算して捨てていた値で、state.db v8 で
`tasks.base_commit` に永続化した。記録が無い（v8 以前の）行は削除しない —
**安全だと証明できないことは破壊の許可ではない**。

## 5. F-86 を全面撤回する

push も PR 作成もエージェントの責務にする。規約に沿った push・PR 手順は
リポジトリ側の CLAUDE.md / hooks が既に持っており、Orchestrator が肩代わりする
理由が無くなったため。`OutputPolicy::PullRequest` / `run::output`
（`PrCreator` / `GhPrCreator` / `PrContext`）/ `[output]` の PR テンプレート /
`WorktreeManager::{push_branch, has_commits_to_publish}` /
`Engine::with_pr_creator` を削除した。

`PullRequest` を enum ごと削除したのは意図的で、受理して `source` 相当に降格させる案は
採らなかった。**PR が作られなくなったことに気付かないまま運用される**方が、起動時に
落ちるより悪い。

## 6. 指示は core の `[prompts].branch_convention`

ブランチは source に依存しない **worktree のメカニクス**なので、ADR-0024 の
所有権表では core 側（マーカー規約と同じカテゴリ）。ADR-0023 に従い `[prompts]` /
`[[workflows]].prompts` で上書き可能。

**plan モードでは注入しない。** plan ペインは git を実行できない
（claude `--permission-mode plan` / codex `--sandbox read-only` / opencode の
`bash: deny`）ので実行不能な指示になるうえ、claude では**無人ペインが答えられない
承認プロンプト**を誘発してタイムアウト → F-103 エスカレーションになる。
つまり**出すこと自体が害**である。既にブランチ上のタスク（再開）にも出さない。

# Consequences

## 新設したデータ損失経路と、その蓋

detached HEAD 上の**コミット済み**作業は `git status --porcelain` が**空**なので
F-23 のデータ損失ガード（`has_uncommitted_changes`）をすり抜け、
`git worktree remove` がその唯一の到達性を持ち去る。`git fsck --lost-found` でしか
拾えず、gc で消える。**この経路はこの変更が作った** — 従来は必ずブランチ上にいて、
`delete_branch_if_published` が「origin に無いコミットがあるブランチは残す」と
判断していた。

`decide_cleanup` に detached-with-commits チェックを足し、`CleanupDecision::Dirty`
として Retain に落とす。plan モードは git を実行できずコミットが 0 なので影響を
受けず、従来どおり掃除される。落ちるのは「実装モードでブランチを切らずにコミットした」
異常系だけで、それは人間の目に留めるべき状態である。

## 承知のうえで失うもの

- **ゼロコミット完了の検出**。`has_commits_to_publish`（F-86）は「`COMPLETED` と
  言ったが 1 行もコミットしていない」を明示的 failed にする唯一の関門だった。
  `output = "source"` にこの検査は無い。
- **PR URL の可視性**。Orchestrator は PR を作らないので URL を知らない。Slack 返信に
  載せるにはエージェントの `last_assistant_message` に含めさせる
  （`plugins/task-source-slack/src/defaults.toml` の `reply_instructions` に追記した）
  — プロンプト依存で、機械的な検査は無い。
- **ADR-0024 の安全表の「構造的」**。「リモートが汚れない ← `output = "source"` +
  F-86 + `Bash(git push:*)` deny」は成り立たなくなる。`Bash(git push:*)` deny は
  そもそも実装されていなかった（コードにも同梱 config にも無い）ので、コード上は
  「入れない」だけで済むが、表の主張は降格させた。残る保証は **worktree 隔離**
  （本体リポジトリは汚れない）と、push 先が worktree のブランチであることだけである。

## 破壊的変更

- `[worktree] location` / `worktree_location` の `{branch}` は廃止。
  `{worktree_name}`（`{source}-{task_id}` を git ref 規則で正規化し `/` を潰したもの）
  に置換する。専用のエラーで起動を止める。
- `output = "pull_request"` は serde の `unknown variant` で起動を止める。
  `output = "source"` に変更し、PR 作成手順はリポジトリの規約と `[prompts]` で指示する。
- `[output]` の `pr_title_template` / `pr_body_template` を削除。
- worktree ディレクトリ名が `agent-slack-{task_id}` から `slack-{task_id}` に変わる。
  既存 worktree の移行は不要 — 掃除・孤児検出・`doctor`・再利用ガードはすべて
  state.db に記録済みの path を読み、テンプレートを引き直さない。
- state.db v8（`tasks.base_commit`、純追加）。適用前に `state.db.v7.bak` が作られる。
- plan モードのタスクは `branch` が恒久的に `None` になる。`totsuka status` /
  `task show` のブランチ欄が空になるが、掃除は正常に動く。

## 既知の制限

orca（非 hook エージェント）には core のプロンプトが届かない
（`extra_context = task.instructions` のみ）。マーカー規約も届いていない既存の穴で、
今回も塞いでいない。orca でブランチを作らせたい場合はリポジトリ側の CLAUDE.md に書く。

# 不採用にした案

- **`agent/` プレフィックス強制**（エージェントには配下の名前だけ決めさせる）。
  機械的に検査でき所有権も守れるが、**リポジトリの命名規約に従うという目的そのものを
  壊す**。規約が `feat/<slug>` を求めているのに `agent/feat-...` にするなら、命名権を
  移した意味が無い。
- **create 時スナップショット差分**（`for-each-ref` で既存ブランチ集合を控え、
  そこに無い名前だけ「エージェントが作った」と見なす）。プレフィックス制約なしに
  所有権を判定できるが、state.db にスナップショット列が要り、**並行タスクが途中で
  作ったブランチを誤って自分のものと見なしうる**。由来判定（`merge-base`）は
  同じ問題を追加の状態なしに解く。
- **エージェントにブランチ名を申告させる**（マーカー拡張 / 結果ファイル / 新 RPC）。
  3 ツール分の実装が要るうえ、**申告と実際の `HEAD` がズレても検出できない**。
  ADR-0024 が「見送り」とした core 側 `verification = "pattern"` と同型の話で、
  `HEAD` を読めば済む。
- **detached のまま完了したら totsuka が救出ブランチを切る**（`agent/{source}-{task_id}`
  を fallback として復活させる）。データは救えるが、規約違反を黙って回復してしまい、
  プロンプトが守られていないことが誰の目にも触れない。Retain に落として人間に見せる方を
  採った。
- **掃除のブランチ削除を廃止**（所有権が言えないので消さない）。安全だが
  [#266](https://github.com/tomoya-k31/totsuka/issues/266) の退行（実機で `agent/*` が
  5 本溜まった）。由来判定で掃除機能を維持できるなら、そちらが上。
- **`output = "pull_request"` を非推奨にして `source` 相当に降格**。既存設定が
  動き続けるので移行は滞らないが、**PR が作られなくなったことに気付かないまま
  運用される**。起動時に落ちる方が良い。
