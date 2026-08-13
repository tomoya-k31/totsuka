---
type: Decision
title: ADR-0045 read-only profile は保証しない — 検出で止め、サンドボックスは実装しない
description: "read-only profile（answer / triage / design）の worktree を sandbox-exec で封じる案（ADR-0040 で実現可能と実測済み）を実装しないと決めた記録。read-only は deny による多層防御と事後検出（D3: publish 直前に即時 / D4: 走行中の 60 秒 sweep）までで、保証ではない。エージェントが Bash 経由でファイルを書き、commit し、push し、PR を作ることは今も可能である。サンドボックスを入れても git push は止まらず、shim の配布・macOS 限定・sandbox-exec の deprecated という費用が残る点が判断材料。"
tags: [decision, security, profile, read-only, sandbox, adr]
generated: { by: claude-code/opus-5, at: 2026-08-13T22:20:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-446
    resource: https://github.com/tomoya-k31/totsuka/issues/446
    title: "feat(core): read-only profile の worktree を sandbox-exec で封じる（ADR-0040 の実装）"
  - id: adr-0040
    resource: /decisions/adr-0040-worktree-sandbox-feasibility.md
    title: ADR-0040 サンドボックス実現可能性の調査
---

# Status

stable。[#446](https://github.com/tomoya-k31/totsuka/issues/446) を実装せずクローズする決定。[ADR-0040](/decisions/adr-0040-worktree-sandbox-feasibility.md) の「送った先」と [ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) の「送った先」が指していた実装は、行われない。

# Context

`answer` / `triage` / `design` は「worktree を書かない」profile として設計されている（[ADR-0033](/decisions/adr-0033-workflow-profile.md) D5）。その強制力として何が実在するかは、すべて実測で分かっている。

**効いているもの**:

- 裸ツール名 deny（`Edit` / `Write` / `NotebookEdit`）— ツールごと消えるので、エージェントがツールとして書こうとすると失敗する
- `Bash(git push *)` 等のパターン deny — リテラルなコマンド文字列への前方一致
- 事後検出（[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md)）— worktree がブランチ上に出たらタスクを失敗させ `on_success` を撃たない。**2 段ある**: D3 は `finalize_success`（publish 到達時に即時）、D4 は走行中の worktree sweep（`WORKTREE_SWEEP_INTERVAL` = 60 秒）で、こちらは pane も閉じる

**効いていないもの**（2026-08-09 の実機 E2E で `answer` profile のタスクが branch → commit → push → PR まで到達した）:

- `cat >` / `cat >>` / `python3 - <<EOF` でのファイル書き込みは、どの deny 規則にも掛からない。書く手段は閉じた集合ではないので、`Bash(...)` の列挙では原理的に覆えない
- `git add -A && git commit -m …` / `git push … | tail -5` / `gh pr create --fill | tail -5` は、**規則が存在する状態で実際に走った**

**観測されたのはそこまでで、迂回の機構は計測されていない。** 前方一致がコマンド文字列全体に当たるため `git add` で始まる文字列が `Bash(git commit *)` に照合されない、というのが自然な説明だが、Claude Code が連結を分解して各部を評価したうえで別の理由で通した可能性も同じだけ残る。どちらかは誰も測っていない（`hooks::permissions` のモジュールドキュメントに同じ但し書きがある）。**結果から機構を断定するのは、この ADR が撤回している過ちと同じ形**なので、対策をこの説明の上に建てないこと。

[ADR-0040](/decisions/adr-0040-worktree-sandbox-feasibility.md)（#418）は、この穴を「効果を封じる」方向で塞げるかを実測し、**塞げる**と結論した — `sandbox-exec` で worktree と元リポジトリの `.git` を deny すると、ファイル書き込み・commit・ブランチ作成は実際に止まる。配線も herdr が pane の PATH からエージェントを解決するのでシムで可能だと確認済みである。

# Decision

**サンドボックスは実装しない。read-only profile の read-only 性は保証しない。** 強制力は現状のまま — deny による多層防御（迂回可能）と、D3 / D4 の事後検出（ブランチ単位。publish 経路は即時、走行中の検知は最大 60 秒）である。

判断材料として ADR-0040 が確定させた費用と限界:

- **`git push` は sandbox でも止まらない。** リモートに届く。最も取り返しがつかない副作用がちょうど射程外にある
- **macOS 限定。** Linux は未調査（bubblewrap 等）で、`sandbox-exec` 自体が deprecated（動作はする）
- **shim の生成・配置・配布が新しい運用面になる。** `totsuka setup` / `doctor` の対象が増え、ツールごとに中身が変わる（codex は自前の `--sandbox read-only` を持つので二重がけの意味が薄い）

# Consequences

- **read-only profile のタスクは、その気になれば worktree を書き、commit し、push し、PR を作れる。** これは既知の受容済みリスクであり、バグとして報告する対象ではない
- 起きたことは検出される — ブランチが現れれば失敗として終わり、worktree とコミットが証拠として残る。publish に到達した経路は D3 が即時に捕まえ、走行中のものは D4 が最大 60 秒で捕まえる。**ただし push は検出時点で済んでいる可能性があり、取り返せない**
- deny は「エージェントが本気で書こうとしない限り」でしか保たない層として残る。捨てはしない（穏当な文面のタスクでは実際に read-only が保たれている）が、保証と呼ばない
- **この決定は覆せる。** ADR-0040 の実測（何が止まり、何が止まらず、どう配線するか）はそのまま有効なので、方針が変われば調査からやり直す必要はない

# 不採用案

- **`Bash` をツールごと deny する**（`answer` では実施済み、[ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md)）: `triage` / `design` には適用できない。`gh issue comment` に複数行 Markdown を渡すにはシェル構文が要るので、Bash を取り上げると profile の仕事そのものができなくなる（[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md)）
- **コマンド文字列を検査するフック**: 引用符の内外を見分けるパーサが要るうえ、取りこぼしに「検査した」という強い名前が付く。ADR-0036 で既に退けている
