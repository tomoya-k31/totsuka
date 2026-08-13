---
type: Decision
title: ADR-0036 triage / design はシェルを検査せず、リポジトリを触ったら成功として扱わない
description: "gh issue comment に複数行 Markdown を渡すにはシェル構文が要るため triage / design から Bash を取り上げられない。コマンド文字列を検査するフックは、引用符の内外を見分けるパーサが要るうえ取りこぼしに強い名前が付くので不採用。代わりに全 read-only profile から plan ゲートを外して無人ハングを消し、read-only profile のタスクがブランチ上にあったら fail_publish で失敗させる。同じ検査を worktree sweep からも回し、走行中に見つけたら pane を閉じる。止まるのは成功報告と on_success で、triage / design の成果物はエージェントが直接書く（#398）ため既に公開済みで取り消せない。防止ではなく検出である。本当の境界はサンドボックスだが、実装しないと決めた（ADR-0045）のでこの検出が最終形になった。"
resource: https://github.com/tomoya-k31/totsuka/issues/409
tags: [decision, security, permissions, claude-code, plan-mode, profile, adr]
generated: { by: claude-code/opus-5, at: 2026-08-13T22:05:00+09:00 }
status: stable
verified:
  - { by: human:tomoya-k31, at: 2026-08-11T04:05:00+09:00 }
  - { by: claude-code/opus-5, at: 2026-08-11T23:10:00+09:00 }
owner: tomoya-k31
sources:
  - id: issue-409
    resource: https://github.com/tomoya-k31/totsuka/issues/409
    title: "fix(core): design profile のタスクが plan 承認ゲートで無人完走できない"
  - id: issue-410-investigation
    resource: https://github.com/tomoya-k31/totsuka/issues/410#issuecomment-5231026517
    title: plan 承認ゲートの原因調査（セッション記録 372 本）
---

# Status

stable（[#409](https://github.com/tomoya-k31/totsuka/issues/409)）。[ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) の続きで、`triage` / `design` を扱う。

# Context

## `answer` の解き方が使えない

[ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) は `answer` から `Bash` をツールごと取り上げて解決した。実機で機能した機構がそれだけだったからである。

**`triage` / `design` には同じ手が使えない。** `gh issue comment` を実行するのがこの profile の仕事そのもので、複数行の Markdown 本文を渡す方法はどれもシェル構文を要する:

```bash
gh issue comment 31 --body 'line1
line2'                                            # 引用符内の改行
gh issue comment 31 --body "$(cat <<'EOF' … EOF)" # コマンド置換 + heredoc
cat > /tmp/x && gh issue comment 31 --body-file /tmp/x   # 実機がやったのはこれ
```

## plan ゲートは確実に壊す

[#410 の調査](https://github.com/tomoya-k31/totsuka/issues/410#issuecomment-5231026517)で、`--permission-mode plan` が残す実質は `ExitPlanMode` の承認ゲートだけだと分かった。そのゲートは無人 pane で非決定的に振る舞い、実機の `design` タスクは **858 秒（14 分 18 秒）人間の承認を待って**から進んだ。これが #409 の実体である。

# Decision

## D1. コマンド検査フックは作らない

`PreToolUse` フックでコマンド文字列を検査する案を検討し、**不採用**とした。

見分けが必要なのはこの 2 つである:

```bash
gh issue comment 31 --body 'A なら && B する設計にします'   # 無害。&& は投稿する文章の一部
gh issue comment 31 --body x && git push                    # 危険
```

**どちらも `&&` を含む。** 区別するには引用符の内外を理解するパーサが要る。書けなくはないが、heredoc・入れ子・コマンド置換で取りこぼしが出る種類のもので、**取りこぼしのある判定器に「コマンド安全検査」という名前が付く**。

それは #410 が示した最悪の形である。#410 の被害は deny が弱かったことではなく、**弱い deny が「保証」として文書化されていた**ことだった。同じ形を三度作らない。

## D2. すべての read-only profile から plan フラグを外す

これは [ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) D2 の**判断を覆す**。あちらは「書き込みツールが全部消えている profile だけ」に限り、`triage` / `design` は「plan モードが持っている唯一の制限だから残す」とした。

覆す理由は会計が合わないからである:

- plan モードの**強制力**は未実測 — `permissionMode` が `plan` のまま `cat >` が成功している
- plan モードの**破壊力**は実測済み — 無人 pane を 14 分止めた

確実な停止を、**推測上の抑止と引き換えにはできない**。

判定は `permissions::plan_mode_only_adds_the_gate` に移した。profile 無しの workflow は引き続き対象外（deny 注入自体が無いので、外すと何も残らない）。

## D3. read-only profile がブランチ上にあったら、成功として扱わない

`finalize_success` が出力ポリシーを実行する直前に検査する。ブランチがあれば `fail_publish` で失敗させる — **worktree とコミットは保持される**ので、人間が何が起きたか見られる。

何が止まるかは **profile で違う**（実機 2026-08-11 で確認）。

| | `answer` | `triage` / `design` |
|---|---|---|
| 成果物そのもの | **止まる**（返信はオーケストレータが出力ポリシーで publish するため） | **止まらない**。エージェントが `gh issue comment` で直接書く（#398）ので、検査が走る時点で公開済み |
| 成功として報告すること | 止まる | 止まる |
| `on_success` の状態遷移 | 止まる | 止まる |

実測では、エージェントが 19:02:57 に設計コメントを投稿し、タスクは 19:03:27 に失敗した。**30 秒の差で、成果物は既に外に出ていた。** #398 で「成果物はエージェントが書く」と決めた以上、`finalize_success` の検査は原理的にそれを取り消せない。この節の初版は「成果物を公開せず失敗させる」と書いていたが、`triage` / `design` については誤りだった。

**これは防止ではない。** ブランチが存在する時点で push は済んでいるかもしれず、そこから取り返す手段は無い。しかし「証拠を残して大きな音で失敗した」は「完了と報告された」とは別の運用状態であり、#410 が生んだのは後者だった。

**profile で門を作る。`record.mode` では作らない。** 素の `mode = "plan"` の workflow は deny 注入も受けておらず、ブランチについて何も約束していない。ここで失敗させると**既存の構成がアップグレードで黙ってタスクを失うようになる** — [ADR-0033](/decisions/adr-0033-workflow-profile.md) D4 が deny 注入を profile 限定にしたのと同じ理由である。

ブランチを信号に使うのは、それがオーケストレータ側から見える唯一のものだから。worktree は detached で渡すので、名前付き `HEAD` は「エージェントが git を実行した」を意味する。detached head 上のコミットは取りこぼすが、その形は ref に名前を付けずに push できない。

**読むのは worktree の生きた `HEAD` で、`record.branch` ではない。** 記録列は書き込み専用に近い — `sync_branch` は detached を「クリアせず未記録のまま残す」設計で、`retry_task` も触らない。そこを門にすると**この失敗から復帰できなくなる**: `totsuka task retry` のたびにここへ来て同じ古い値を読み、永久に落ち続ける。`HEAD` を読めば検査は worktree の現状を述べるものになり、**detach という操作が実際に効く救済手段になる**（失敗メッセージにその手順を書いてある）。

## D4. 同じ検査を走行中にも回し、発火したら pane を閉じる

D3 は `finalize_success`、つまり**タスクが publish に到達したときだけ**走る。[#410](https://github.com/tomoya-k31/totsuka/issues/410) の最後の未了項目として、worktree sweep からも同じ検査を回す（`Engine::enforce_read_only`）。

**publish に到達しないタスクが取りこぼされていた。** `WaitingInput` / `Escalated` で終わった、あるいは pane を殺されたタスクは、ブランチを切って（場合によっては push して）**非失敗の状態のままログ 1 行だけを残して**止まる。[#422](https://github.com/tomoya-k31/totsuka/issues/422) の実機事例がまさにその形で、`answer` タスクが `NEEDS_INPUT` で park している。

**もう半分は pane を閉じることである。** `finalize_success` がブランチを見るのはエージェントが仕事を終えたあとなので、そこで分かっても止めるものが残っていない。走行中に見つけたなら pane を閉じられる — **走っているエージェントに対してこちら側が持つ唯一のレバー**がそれである。失敗を記録する前に試みる（生きている 1 秒ごとに、この検査では取り消せない push の機会が増えるため）。

**ただし pane を閉じるのは best-effort で、失敗の記録はどちらにせよ行う。** 「閉じたと確認できるまでタスクを in-flight に留める」案は採らない — 信頼できる側（記録）を信頼できない側（RPC）に人質に取る形になり、herdr に届かないときに**違反が記録されないまま残る**。それはこの検査が塞ごうとしている穴そのものである。閉じられなかった pane は設計上 `doctor` の担当で（#211）、pane には `session/list` が拾う所有マーカーが残っている。確認できなかった場合は `tracing::error!` でそう言う。

**これは防止ではない。間隔がそう言っている。** sweep は 60 秒間隔（`WORKTREE_SWEEP_INTERVAL`）で、`git switch -c` から `git push` までの数秒という窓を安定して取れる速さではない。**保証できるのは「違反したタスクは失敗で終わり、そのエージェントは止まっている」までで、「違反が起きない」ではない。** そして**防止は来ない** — サンドボックスは実装しないと決めた（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md)）。

門は D3 と同じ `read_only_side_effect` を共有する（profile で門を作り、生きた `HEAD` を読む）。素の `mode = "plan"` は D3 と同じく警告のみで、失敗しない。

# 不採用案

## `PreToolUse` フックでコマンドを検査する（D1 の詳細）

判定を shell script ではなく totsuka 本体（Rust）に置き、引用を理解するトークナイザをユニットテストで固める案まで具体化した。**採らない** — 理由は D1 のとおり。厳しすぎれば design が壊れ、緩ければ穴が残り、どちらでも「検査している」という名前だけが残る。

## `Write` を許して `--body-file` を使わせる

パス限定の `Write(path)` は**受理されて参照されない**（Claude Code の既知の挙動、`permissions.rs` に記録済み）。許すなら worktree 全体が書ける。read-only profile の意味が消える。

## `ExitPlanMode` を deny して plan モードに固定する

無人承認も無人ハングも消える。**採らない**: plan モードに留まったエージェントは `gh issue comment` を「副作用だから承認待ちにすべきもの」と扱う可能性が高く（実機の design タスクは承認後に初めて投稿した）、**確実に止まる形へ倒すおそれがある**。#409 は「止まる」ことが問題なので、それを直すのに止まりやすい方へ倒すのは筋が通らない。

# Consequences

## 良くなること

- `design` / `triage` が無人で完走できるようになる（#409 の主訴）
- read-only profile が黙って成功しなくなる。worktree とコミットが残るので事後調査ができる
- **保証していないことを保証していると書かない状態が保てる**

## 引き受けたコスト

- **`triage` / `design` に境界は無い。** `Bash(...)` パターンは #410 が不十分だと示したままで、シェル経由の書き込みも `&&` による回避も可能である。**防げるとは書かない**
- **push / PR は取り返せない。** 検出は事後で、外部に出たものは戻らない。**`triage` / `design` では成果物そのものも同じ**（上の表）— 止められるのは成功報告と `on_success` だけである
- **検出はブランチだけを見る。** detached head 上のコミットは通る（push には ref 名が要るので、実害の主経路は押さえている）
- **救済は `task retry` 単体ではない。** worktree がブランチ上にある限り検査は再び落とすので、operator は先に detach するか cancel する必要がある。メッセージにその 2 択を書いた
- plan フラグを外したことによる未実測の縁は [ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) と同じ — pane は既定モードで起動し、`WebFetch` / `mcp__*` に対する plan の網は代替されていない

## 送った先

本当の境界は「構文を見て拒む」ではなく「効果を封じる」方向にしかない。worktree を OS レベルで read-only にできるかの調査を [#418](https://github.com/tomoya-k31/totsuka/issues/418) に切り、[ADR-0040](/decisions/adr-0040-worktree-sandbox-feasibility.md) が **`sandbox-exec` で実現できる**と実測で確定させた。

**ただしその実装は行わないと決めた**（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md)、#446 をクローズ）。したがって **`Bash(...)` パターンという弱い層は捨てられず、ここに書いた検出が最終形である**。read-only profile の read-only 性は保証ではない。

# 検証

- `cargo test --workspace --all-features` — 全 read-only profile が plan フラグを落とすこと（`implement` と profile 無しは落とさないこと）、`read_only_side_effect` が profile で門を作りブランチ・profile 名・「push は取り返せない」旨・**救済手順（detach / cancel）**をメッセージに含むこと、**detached では発火しないこと**（救済が実際に効くことの固定）、`implement` / profile 無しでも発火しないこと
- **D4 も実機検収済み（2026-08-11、task 52）。** 走行中の `github-design` タスクの worktree が現れた瞬間にブランチを注入したところ:

  | 観測 | 結果 |
  |---|---|
  | 状態遷移 | `dispatched → failed` — **`publishing` を経ていない**（D3 は `publishing → failed` だった） |
  | 注入から失敗まで | **23 秒** |
  | pane | 閉じた（`w6V:p1` が消滅、ログに `pane released`） |
  | worktree | 残った（`HEAD = feat/d4-injected`） |
  | 失敗理由 | ブランチ名・profile 名・「push は取り返せない」・救済手順すべて含む |

  **D3 と D4 は同じ違反に対して別の段階で発火する**ことが、この 2 回の実機検収で分離して確認できた。
  結合テスト `a_read_only_task_that_branches_mid_run_is_failed_and_its_pane_closed`（`run_loop.rs`）も
  同じ性質を固定しており、`enforce_read_only` の呼び出しを外すと 60 秒待って落ちる（確認済み）。
- **この検収で表示バグを 1 つ見つけて直した。** `fail_publish` が publish 専用だった頃のログ文言
  （`output policy failed:`）と `detail.kind = "publish"` をハードコードしていたため、**出力ポリシーが
  1 度も走っていない失敗を publish の失敗として報告していた**。`kind` を引数にし、走行中の検知は
  `read_only_violation` と名乗る。`release_pane` の `pane released before worktree removal` も、
  worktree を保持したまま閉じる経路が増えたので `pane released` にした。**どちらも新しい呼び出し元を
  足したことで既存の文言が嘘になった形**である。
- **実機検収済み（2026-08-11）。** `design` タスク（`github-design`）が**人間の承認を待たず約 2 分で完走**した（従来は 858 秒待ち）。セッション記録の `permissionMode` は `auto` のみで **`ExitPlanMode` の呼び出しは 0 回**、拒否も 0 件。成果物を本人名義で issue へ投稿し、`on_success` で `Design Review` へ遷移した
- **違反時の失敗も実機で発火させた。** 走行中の `design` タスクの worktree にブランチを注入したところ、状態遷移は `dispatched → running → publishing → failed` となり、`finalize_success` に入ってから `fail_publish` で落ちた。worktree とコミットは保持され、失敗理由にブランチ名・profile 名・「push は取り返せない」・救済手順（detach / cancel）がすべて入っていた。**負の対照**も取れている: 同時期の `implement` タスクはブランチ `feat/logtool-stddev` を持ったまま `done` で完走し、PR まで作った（免除が効いている）
- **ただしこの検収は「承認プロンプトを 1 つも出さずに完走」を証明していない。** pane は `permissionMode: auto` で動いており、検証機のグローバル設定にある広い `allow` が効いていた。[#420](https://github.com/tomoya-k31/totsuka/issues/420) は open のまま
- **検収では「承認プロンプトを 1 つも出さずに完走したか」を明示的に見る。** plan フラグを外した pane は環境の既定モードで起動し、totsuka が配る settings には `deny` しか無い（`allow` も `defaultMode` も書かない）。`triage` / `design` は仕事が丸ごと `Bash`（`gh issue comment`）なので、**既定モードが Bash の承認を求める環境では、#409 と同種の停止が場所を変えて戻る**。開発機のグローバル設定には `Bash(gh:*)` 等の allow があるため**そこでは再現しない**点に注意 — クリーンな環境で確かめる必要がある。追跡は [#420](https://github.com/tomoya-k31/totsuka/issues/420)
