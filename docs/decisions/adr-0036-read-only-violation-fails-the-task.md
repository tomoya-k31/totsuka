---
type: Decision
title: ADR-0036 triage / design はシェルを検査せず、リポジトリを触ったら公開せず失敗させる
description: "gh issue comment に複数行 Markdown を渡すにはシェル構文が要るため triage / design から Bash を取り上げられない。コマンド文字列を検査するフックは、引用符の内外を見分けるパーサが要るうえ取りこぼしに強い名前が付くので不採用。代わりに全 read-only profile から plan ゲートを外して無人ハングを消し、read-only profile のタスクがブランチ上にあったら成功として公開せず fail_publish で失敗させる。防止ではなく検出で、本当の境界はサンドボックス調査（#418）に送る。"
resource: https://github.com/tomoya-k31/totsuka/issues/409
tags: [decision, security, permissions, claude-code, plan-mode, profile, adr]
generated: { by: claude-code/opus-5, at: 2026-08-10T00:40:00+09:00 }
status: stable
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

## D3. read-only profile がブランチ上にあったら、公開せず失敗させる

`finalize_success` が成果物を公開する直前に検査する。ブランチがあれば `fail_publish` で失敗させる — **worktree とコミットは保持される**ので、人間が何が起きたか見られる。

**これは防止ではない。** ブランチが存在する時点で push は済んでいるかもしれず、そこから取り返す手段は無い。しかし「証拠を残して大きな音で失敗した」は「完了と報告された」とは別の運用状態であり、#410 が生んだのは後者だった。

**profile で門を作る。`record.mode` では作らない。** 素の `mode = "plan"` の workflow は deny 注入も受けておらず、ブランチについて何も約束していない。ここで失敗させると**既存の構成がアップグレードで黙ってタスクを失うようになる** — [ADR-0033](/decisions/adr-0033-workflow-profile.md) D4 が deny 注入を profile 限定にしたのと同じ理由である。

ブランチを信号に使うのは、それがオーケストレータ側から見える唯一のものだから。worktree は detached で渡すので、名前付き `HEAD` は「エージェントが git を実行した」を意味する。detached head 上のコミットは取りこぼすが、その形は ref に名前を付けずに push できない。

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
- **push / PR は取り返せない。** 検出は事後で、外部に出たものは戻らない
- **検出はブランチだけを見る。** detached head 上のコミットは通る（push には ref 名が要るので、実害の主経路は押さえている）
- plan フラグを外したことによる未実測の縁は [ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) と同じ — pane は既定モードで起動し、`WebFetch` / `mcp__*` に対する plan の網は代替されていない

## 送った先

本当の境界は「構文を見て拒む」ではなく「効果を封じる」方向にしかない。worktree を OS レベルで read-only にできるかの調査を [#418](https://github.com/tomoya-k31/totsuka/issues/418) に切った。**実現できれば `Bash(...)` パターンという弱い層ごと捨てられる。**

# 検証

- `cargo test --workspace --all-features` — 全 read-only profile が plan フラグを落とすこと（`implement` と profile 無しは落とさないこと）、`read_only_side_effect` が profile で門を作りブランチ・profile 名・「push は取り返せない」旨をメッセージに含むこと、detached / `implement` / profile 無しでは発火しないこと
- **実機検収は未了。** `design` タスクが人間の承認を待たずに完走すること、および read-only profile がブランチを切った場合にタスクが失敗して worktree が残ることを実機で確認するまで `verified` は付けない
