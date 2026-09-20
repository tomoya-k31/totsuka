---
type: Decision
title: ADR-0085 ソースが既存ブランチを名指しできる Task.branch_hint を足し、どう使うかは core がモードで決める
description: GitHub Project 上の PR をタスクにして既存 PR のブランチ上で設計・追加修正させるために、Task に branch_hint（protocol 0.7.5）を足した決定。ソースはブランチ名を言うだけで、writable なステージはそのブランチ上に、plan のステージはその先頭 commit に detached で worktree を作るという使い分けは core が持つ。ヒントは助言ではなく、見つからない・分岐している・別の worktree が掴んでいる場合はフォールバックせずタスクを失敗させること、残っている worktree も dispatch のたびにヒントへ同期すること、ブランチの状態を語る不可視文面は core が持ちソースプラグインには書かせないこと、PR の取り込みに opt-in キーを設けないことを記録する。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/worktree/mod.rs
tags: [decision, adr, protocol, worktree, branch, github, pull-request, prompts, profile]
generated: { by: claude-code/fable-5-1, at: 2026-09-21T01:15:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-734
    resource: https://github.com/tomoya-k31/totsuka/issues/734
    title: "GitHub Project 上の PR item をタスクとして取り込み、既存 PR のブランチ上で設計・追加修正させる — #734"
  - id: adr-0045
    resource: /decisions/adr-0045-read-only-is-not-guaranteed.md
    title: "read-only は保証ではない — ADR-0045"
  - id: adr-0015
    resource: /decisions/adr-0015-conversation-task-identity.md
    title: "タスク同一性が会話同一性 — ADR-0015"
  - id: adr-0024
    resource: /decisions/adr-0024-agent-instruction-layers.md
    title: "エージェントへの指示の層と所有者 — ADR-0024"
  - id: renovate-rebasing
    resource: https://docs.renovatebot.com/updating-rebasing/
    title: "Renovate — Updating and rebasing branches"
---

# Status

**採択（stable）。** #734 を 2 本の PR で実装する。1 本目（この ADR を含む）が plugin-protocol と orchestrator-core で、`branch_hint` を埋めるソースがまだ無いため、マージしても挙動は変わらない。2 本目が task-source-github で、PR item の取り込みが入った時点で挙動が変わる。決定 6〜8 は 2 本目で実装される。

# Context

GitHub Project のボードに載せた PR を、totsuka はタスクにできなかった。github プラグインが取り込みの入口で `__typename != "Issue"` の item を捨てていたためである。[^issue-734]

やりたいのは、totsuka のタスクから生まれていない PR（依存更新ボットの bump、人間が開いた PR）に追加修正が要るとき、その PR のブランチ上で続きをやらせることだった。なお totsuka が issue から作った PR への追加修正は、この決定の前からできている。issue のカードを trigger 列へ戻せば、同じタスクが記録済みのブランチで再開される（[ADR-0015](/decisions/adr-0015-conversation-task-identity.md)）。

回避策（issue を別に立てて本文に「既存 PR のブランチに積め」と書く）は確実に動かなかった。worktree は detached HEAD で渡され、core の不可視文面 `branch_convention` が「`git switch -c` で新しいブランチを作れ」と指示するので、本文の指示と競合する。結果は 2 本目の PR か、何もできずに止まるかだった。

調べて分かった制約が 3 つある。

- **read-only プロファイルの worktree はブランチに乗せられない。** 完了時に名前付きブランチ上だと、core は「エージェントが git を実行した」と解釈して成功としての確定を拒否し、pane を閉じる（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md) の `read_only_side_effect`）。design のステージを PR のブランチに乗せると、毎回この検査に落ちる。
- **dispatch は、残っている worktree を作り直さない。** 記録されたディレクトリがあればそのまま再利用し、`WorktreeManager::create` を呼ばない。開始位置を `create` の中だけで決めると、design から implement へ渡ったタスクは、design が見た古い commit に detached なまま implement を始める。
- **ソースプラグインは profile を知らない。** `[[workflows]].profile` は core のスキーマで、プラグインに届くのは `instructions_kind` という文字列だけである。

# Decision

## 1. `Task.branch_hint` を足す。ソースはブランチ名を言うだけ（protocol 0.7.5）

`Task` に `branch_hint: Option<String>` を足した。`repo_hint` と対になるフィールドで、「どこで」に対する「どのブランチで」を運ぶ。`#[serde(default, skip_serializing_if)]` の加算フィールドなので、0.7.2 の `handle` と同じく patch 上げで済み、マニフェストの要求範囲は 1 つも動かない。

ソースが言うのはブランチ名だけで、それをどう使うかは言わない。フィールドを 2 つ（「このブランチ上で」と「このブランチの先頭を起点に」）に分けてプラグインに選ばせる案は採らなかった。read-only かどうかの判断がプラグインへ漏れ、同じ判断をソースの数だけ複製することになるためである。

名前は `origin` に対してだけ解決する。fork の head のように `origin` に無いブランチについては、ソースがこのフィールドを空にしなければならない。同じ名前が `origin` に別の意味で存在しうるためである（fork の head が `main` であることは珍しくない）。

## 2. どう使うかは core がモードで決める

`acquire_worktree` が、ヒントとステージのモードを突き合わせる唯一の場所である。

| モード | worktree | 理由 |
|---|---|---|
| implement | ヒントのブランチ**上** | そこに commit して push するのが仕事 |
| plan | ヒントのブランチの先頭 commit に **detached** | ブランチ上だと ADR-0045 の検査でタスクが失敗する |

profile ではなくモードで判定する。read-only プロファイルはすべて plan に解決されるうえ、profile を持たない素の `mode = "plan"` も「ブランチを作らない」と約束されている（F-82）。こちらは警告で済むが、core 自身がブランチに乗せると dispatch のたびにその警告が出る。

worktree 層の入口は `CreateRequest.hinted: Option<HintedStart>` の 1 つにまとめた。既存の `existing_branch` と `base_branch` に流し込む案は採らなかった。その 2 つは寛容（無ければフォールバックする）で、ヒントは次の決定のとおり厳格でなければならないからである。

## 3. ヒントは助言ではない。honour できなければタスクを失敗させる

`repo_hint` は解決できなければ分類器へ落ちるが、`branch_hint` はフォールバックしない。

| 状況 | 結果 |
|---|---|
| `origin` にそのブランチが無い | `HintedBranchMissing` |
| ローカルの同名ブランチが `origin` と分岐している | `HintedBranchDiverged` |
| 別の worktree がそのブランチを掴んでいる | `HintedBranchHeld`（掴んでいる worktree のパスを示す） |
| 残っている worktree を移せない（未コミット変更） | `HintSyncBlocked` |

フォールバックした場合に起きるのは、既定ブランチに detached な worktree と「新しいブランチを作れ」という指示で、その先は 2 本目の PR である。これはこの機能が防ごうとしている事故そのものなので、黙って進むより止まるほうが安い。

totsuka 自身が**記録した**ブランチ（`tasks.branch`）が消えていた場合のフォールバックは、今のまま残した。そちらは「記録した名前がもう何も指していないので、エージェントに付け直させる」という意図的な挙動である。

## 4. ローカルの同名ブランチは決して reset しない

ローカルに同名のブランチがあるなら、それは誰かのものである。運用者が以前 `gh pr checkout` したものか、このタスク自身の前回の実行である。

| ローカル vs `origin` | 扱い |
|---|---|
| 無い | `origin` の先頭に作る |
| 同じ、またはローカルが先 | そのまま（先にあるのは未 push の作業） |
| ローカルが遅れている | fast-forward する |
| 分岐 | 失敗 |

依存更新ボットは自分のブランチを日常的に force-push するので、分岐は普通に起きる。どちらかを捨てずに済む選択肢が無いので、人間に渡す。別の worktree が掴んでいるかの検査を先に行うのは、他の worktree の足元で ref を動かすと、その worktree に誰も作っていない差分が現れるためである。

## 5. 残っている worktree も、dispatch のたびにヒントへ同期する

`WorktreeManager::sync_to_hint` を、ヒントを持つタスクの dispatch ごとに呼ぶ。何も動いていなければ fetch 1 回と `rev-parse` 2 回で終わる。

`plan_cleanup = "immediate"` を前提にする案は採らなかった。その設定なら design の完了時に worktree が消え、implement で作り直されるので問題は出ない。しかし他の cleanup 設定や、未コミットのファイルで worktree が残った場合にだけ 2 本目の PR が開く、という失敗は原因を追えない。

同期は何も捨てない。未コミット変更が switch を妨げたら `HintSyncBlocked` で止まる。

## 6. PR は issue とは別のタスクである

PR のタスクの id は PR の node id で、その PR を生んだ issue のタスクとは統合しない。GitHub 上の issue と PR の結びつきは `Closes #N` が無ければ構造的に存在せず、それを当てにした統合は外れたときに黙って壊れる。ブランチ名で既存タスクを探して統合する案も検討したが、1 つのタスクを 2 枚のカードが駆動することになり、`on_success` の書き戻しが運用者の動かしたカードではないほうへ飛ぶ。

代償は決定 3 の `HintedBranchHeld` である。issue 由来のタスクの worktree が保持ポリシーで残っている間、同じブランチの PR タスクは失敗する。totsuka 生まれの PR は issue のカードを戻すのが正規の経路、という整理になる。

## 7. 取り込みに opt-in キーは設けない

`design` と `implement` の profile を持つ workflow は、設定を変えなくても PR を受ける。trigger に `item = "pull_request"` のようなキーを足す案は採らなかった。PR がタスクになるには「ボードに載っている」「trigger の列にいる」「assignee が一致する」の 3 つが要り、意図せず拾う余地は小さい。必要になったら足せる。

これはリリースノートに書くべき挙動変更である。

## 8. ブランチのことは core が語り、PR のことはプラグインが語る

PR タスクへの指示は、知っている者が違う 2 つに分かれる（[ADR-0024](/decisions/adr-0024-agent-instruction-layers.md) の層に従う）。

| 内容 | 語るのは |
|---|---|
| 既存のブランチ上にいる（または先頭に detached）。ブランチを作るな・切り替えるな | core（`hinted_branch_on` / `hinted_branch_detached`） |
| これは PR #N である。こう読め。2 本目の PR を開くな。何の URL を報告せよ | github プラグイン（`design_pr_instructions` / `implement_pr_instructions`） |

プラグインに「あなたはブランチ X にいる」と書かせる案は採らなかった。プラグインは worktree がどう作られたかを知らないので、決定 2 を散文に写すことになり、core を変えた日に文面が嘘になる。

`branch_instruction`（`run/support.rs`）が 3 つの文面から高々 1 つを選ぶ。`hinted_branch_on` は初回だけでなく dispatch のたびに送る。自分で作ったブランチを再開するエージェントに念押しは要らないが、他人が作ったブランチでは「2 本目の PR を開くな」の 1 文に毎回の価値がある。

# Consequences

- **`branch_hint` は github 専用ではない。** どのソースも埋めれば同じ扱いを受ける。Notion が将来ページのプロパティから対象ブランチを引くようになっても、core とプロトコルはそのまま使える。
- **`Task` を構造体リテラルで組むコードには source break。** 同梱プラグインと core の 16 か所を同じコミットで直した。wire 互換は保たれる。
- **ヒントを持つタスクは、dispatch のたびに `git fetch` が 1 回増える。** これまで fetch するのは worktree を作るときだけだった。
- **依存更新ボットとの相互作用は運用の問題として残る。** エージェントがボットのブランチに push すると、Renovate はそのブランチの更新を止める。その後 rebase ラベルを使うと Renovate は自分の commit でブランチを作り直し、エージェントの commit は消える。[^renovate-rebasing] implement のステージに変更内容を PR コメントとして残させるのは（2 本目の PR で実装）、この場合の記録にもなる。
- **fork からの PR は扱わない。** `refs/pull/N/head` の fetch、fork への push、`maintainerCanModify` の検査が要り、今回の動機には不要だった。

[^issue-734]: GitHub Project 上の PR item をタスクとして取り込み、既存 PR のブランチ上で設計・追加修正させる — #734
[^renovate-rebasing]: Renovate — Updating and rebasing branches
