---
type: Decision
title: ADR-0115 task retry --repo でリポジトリを差し替えてやり直す
description: リポジトリ選択を誤ったタスクは、retry しても同じリポジトリで動き、取り込み直しも Duplicate になる。task retry に --repo を足し、終端状態のタスクを指定の [[repositories]] へ移して再キューする決定。旧 worktree は cleanup と同じガードで消し（汚れていれば拒否）、ブランチ・base commit・tool session id を忘れ、移動を EventDetail::Moved として記録して次の dispatch が旧 pane へ再接続しないようにする。--repo なしの retry は変えない。
resource: https://github.com/tomoya-k31/totsuka/issues/896
tags: [decision, cli, retry, repository, worktree, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-11T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-896
    resource: https://github.com/tomoya-k31/totsuka/issues/896
    title: "Issue #896"
---

# Status

stable（[#896](https://github.com/tomoya-k31/totsuka/issues/896)）。CLI の `--repo` が先で、メニューバーアプリの「Retry in…」はその上に載せる（後続 PR）。

# Context

ソース側の対象リポジトリのヒントが漏れたタスクが LLM 分類で別のリポジトリに決まると、正しいリポジトリでやり直す手段が無かった。

- `task retry` は記録済みの worktree・ブランチ・セッションを再利用する（F-44）。リポジトリは変わらない
- 取り込みはソース側の id で重複判定するので、`cancelled` の行が残っている間はポーラーが再 submit しても `Duplicate`
- 回避策は状態 DB の行の直接削除だけだった

調べて分かった事実:

- 再 dispatch は `tasks.worktree_path` を再利用し、`branch` があればその上に worktree を作り直し、`base_commit` は一度しか書かない（`COALESCE`）。最新セッションの `tool_session_id` があれば会話を resume する
- 再 dispatch 前の旧 pane の解放（#481）は `worktree_path` を `expect_cwd`（identity guard）に使う。herdr の pane id は位置ベースなので、guard なしの close は別の pane を閉じうる
- `worktree_path` と最新セッションがあり、未送信メッセージが無いと、dispatch は旧 pane へ再接続する（`reusable_session`）。リポジトリを移したタスクでは、旧リポジトリの会話を抱えた pane を渡すことになる
- `task_control` の CLI 側の切り替え（[ADR-0094](/decisions/adr-0094-task-control-endpoints.md) の PR 2）は入っておらず、`task retry` は今も DB を直接書く

# Decision

1. **`totsuka task retry <ID> --repo <name>`。** 受け付ける状態は `retry` と同じ（`failed` / `cancelled` / `skipped`）。実行中のタスクには許さない — `cancel` してから。規則は `task_control::retry_in_repo` に置き、Engine の `/task/retry` にも同じ関数を使える形にする（ルートへの `repo` の追加は ADR-0094 の PR 2 と一緒に行う）
2. **`[[repositories]]` に無い名前は拒否**し、設定済みの名前を列挙して案内する。タスク自身のリポジトリを指定したら、ただの `retry`
3. **旧 worktree は retry の時点で消す。** `WorktreeManager::remove`（cleanup と同じ関数）を使うので、未コミットの変更や detached HEAD 上の未 push コミットがあれば消さずに拒否し、手で消すコマンドを案内する。ブランチは cleanup と同じ条件（このタスクのもので、`origin` に全部ある）でだけ消える。旧リポジトリが設定から消えていて `git` を当てる先が無いときも拒否する。拒否したときタスクは元のまま
4. **旧リポジトリのものを忘れる。** 再キューと同じトランザクションで `repo` を書き換え、`branch` と `base_commit` を NULL にし、そのタスクの全セッションの `tool_session_id` を NULL にする（旧リポジトリの文脈の会話を resume しない）。**`worktree_path` は残す** — 次の dispatch が旧 pane を identity guard つきで閉じるのに使い、ディスクに無いパスは新しいリポジトリで作り直される
5. **移動は `EventDetail::Moved { command, from_repo, to_repo }` として記録する。** 監査ログにどこから移ったかが残る。dispatch はこれを読み戻し（`StateDb::moved_since_dispatch` — 最新の `dispatched` 遷移より新しい `moved` 行があるか）、そのあいだは旧 pane へ再接続せず、閉じて新規に dispatch する。読み戻しは `auto_retry_streak` と同じく Rust で `EventDetail` に deserialize して判定し、SQL の JSON 検索は使わない
6. **`--repo` なしの `retry` は変えない。** 再接続の抑止は `moved` のときだけ効く

# Alternatives

- **旧 worktree をディスクに残し、行から外すだけ**: 掃除の対象から外れて黙って溜まる。不採用
- **旧 worktree が残っていれば `--repo` を拒否する**: 直せるものを人間に押し戻すだけ。不採用
- **再接続の抑止を「worktree がディスクに無ければ再接続しない」で判定する**: 実装は 1 行だが、手で worktree を消した後の通常の `retry` の挙動も変わる。通常の `retry` は今までどおりに保つと決めたので、移動の明示的な記録で判定する
- **`worktree_path` を NULL にする**: 旧 pane の解放が identity guard なしになる。不採用
- **専用の列を足す**: マイグレーションが要り、クリアし忘れの経路が増える。監査ログに残す必要もあるので、イベント行に寄せた

# Consequences

- 旧 pane は、次の dispatch まで旧 worktree のディレクトリ（消えている）に残る。cancel の後に retry するまでは今までも pane が残っていたので、新しい状態ではない
- 旧 worktree の削除は、再キューのトランザクションより先に行う（未コミット変更での拒否を、何も書き換えないうちに返すため）。その間に同じタスクが別の書き手に動かされると（新しいメッセージでの reopen など）、再キューは競合で拒否されるが、worktree はもう消えている。消すのは cleanup と同じガードを通ったもの（汚れておらず、detached HEAD に未 push コミットも無い）だけで、ディスクに無い worktree は次の dispatch が記録済みのブランチから作り直すので、作業は失われない
- 人間の `task retry --repo` も `queued` への遷移なので、`auto_retry_streak` の予算をリセットする（通常の `retry` と同じ）
- メニューバーアプリは `task retry <ID> --repo <name>` をそのまま呼べる
