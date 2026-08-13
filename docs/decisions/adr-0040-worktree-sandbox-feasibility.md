---
type: Decision
title: ADR-0040 read-only profile の worktree は sandbox-exec で OS レベルに封じられる（が push は止まらない）
description: "read-only profile の worktree を macOS の sandbox-exec（Seatbelt）で書き込み禁止にできるかの調査結果。worktree と元リポジトリの .git を deny すればファイル書き込み・commit・ブランチ作成は実測で止まり、読みと gh は無傷で /tmp も書ける。配線は herdr が pane の PATH からエージェントを解決するのでシムで可能。ただし git push はリモートに届いてしまい、Claude Code 自身のサンドボックスは運用判断で使わない。Linux と sandbox-exec の将来は未解決。"
resource: https://github.com/tomoya-k31/totsuka/issues/418
tags: [decision, security, sandbox, macos, seatbelt, profile, herdr, adr]
generated: { by: claude-code/opus-5, at: 2026-08-13T22:05:00+09:00 }
status: stable
verified: [{ by: claude-code/opus-5, at: 2026-08-11T21:40:00+09:00 }]
owner: tomoya-k31
sources:
  - id: issue-418
    resource: https://github.com/tomoya-k31/totsuka/issues/418
    title: "調査: read-only profile の worktree を OS レベルで書けなくできるか（サンドボックス）"
  - id: sandbox-exec-probe
    resource: 2026-08-11 の実機計測（macOS Darwin 24.6 / herdr 0.7.5 / Claude Code 2.1.227）
    title: sandbox-exec プロファイルと herdr 経由 dispatch の実測
---

# Status

stable（[#418](https://github.com/tomoya-k31/totsuka/issues/418)）。**調査 issue の成果物であって、実装の決定ではない。**
[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) が「本当の境界は効果を封じる方向にしかない」と送った先がここ。

# Context

[ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) / [ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) で、**「コマンド文字列を見て拒否する」形の防御は行き止まり**だと結論した。`answer` は `Bash` をツールごと取り上げて解決したが、`triage` / `design` は `gh issue comment` を実行するのが仕事そのものなので同じ手が使えない。

残る出口は「構文を見て拒む」ではなく「効果を封じる」— worktree を OS レベルで書けなくすることである。この ADR はそれが macOS で現実的かを測った結果を記録する。

# Decision

## D1. `sandbox-exec`（Seatbelt）で封じられる。実測した

macOS の `sandbox-exec` は deprecated と表示されるが **Darwin 24.6 で現に動く**。プロファイルは 5 行で足りる:

```scheme
(version 1)
(allow default)
(deny file-write*
  (subpath "<worktree>")
  (subpath "<元リポジトリ>/.git"))
```

`sandbox-exec -f <profile> <command>` 下での実測:

| 操作 | 結果 |
|---|---|
| `echo x > file`（worktree 内） | **拒否**（`operation not permitted`） |
| `python3 -c "open('f','w').write('x')"` | **拒否** — #410 が実際に使った迂回路 |
| `git switch -c` | **拒否**（`cannot lock ref … Operation not permitted`, rc=128） |
| `git commit` | **拒否**（`Unable to create index.lock`, rc=128） |
| `git log` / `git status` | 通る（読みは無傷） |
| `cat README.md` | 通る |
| `/tmp` への書き込み | 通る |
| `gh --version` | 通る |

**`Bash(...)` パターンでは閉じられなかった集合が、ここでは閉じている。** 「`cat >` も `python3 -` も `tee` も列挙し切れない」という #410 の問題は、列挙をやめて効果を封じることで消える。

## D2. `.git` は worktree の**外**にある。両方を deny しないと意味がない

linked worktree の git ディレクトリとオブジェクトストアは**元リポジトリの `.git` 配下**にある（実測）:

```text
worktree:        <scratch>/wt-probe
git-dir:         <scratch>/verify-repo/.git/worktrees/wt-probe
git-common-dir:  <scratch>/verify-repo/.git
objects:         <scratch>/verify-repo/.git/objects
```

**worktree のパスだけを `deny file-write*` しても `git commit` は完全に通る。** 上の表で commit が止まっているのは、プロファイルが元リポジトリの `.git` も deny しているからである。これを外すと防御はまるごと無効になる。

## D3. 配線は totsuka 側だけでできる — herdr は pane の `PATH` からエージェントを解決する

protocol 17 以降、herdr は `agent.start` の `kind` から**自分で実行ファイルを選ぶ**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-1）。totsuka が `ToolLaunchSpec.program` に何を書いても、その値は `kind` を導くのに使われるだけで、`sandbox-exec` を前に差し込むことはできない。

**しかし `PATH` は totsuka が握っている。** `workspace.create` の `env` は herdr が root pane（＝エージェントが起動する pane）に適用する。そこに shim ディレクトリを先頭に足した `PATH` を渡し、shim に `claude` という名前で

```zsh
#!/bin/zsh
exec sandbox-exec -f "$PROFILE" /path/to/real/claude "$@"
```

を置けばよい。**実測で確認した**（herdr 0.7.5、隔離セッション）:

- shim は呼ばれた（`argv=--permission-mode plan` がログに残った）
- **herdr のエージェント検出は shim 越しでも通る** — `pane.list` は `agent: "claude"` / `agent_status: "idle"` を返した（`exec` で実体が本物の claude に置き換わるため）
- dispatch は 5.1 秒で成功。`agent.start` / `agent.prompt` の readiness レース（#387 / #391）に追加の影響は見られない
- **その pane で走っている本物の claude に書き込みを試させたところ、`git switch -c` は `Operation not permitted` で失敗し、worktree にファイルは 1 つもできなかった**

つまり herdr 側の変更を待たずに配線できる。

## D4. **`git push` は止まらない。** これは境界ではない

`git push` を同じサンドボックス下で走らせた結果:

```text
To <origin>
 * [new branch]      HEAD -> sb-push-test
error: update_ref failed for ref 'refs/remotes/origin/sb-push-test':
  cannot lock ref … Operation not permitted
```

**ブランチはリモートに届いている。** 失敗したのはローカルの remote-tracking ref の更新だけで、それは push が完了した**あと**の記帳である。`gh` も同様にネットワークへ出る。

実害の主経路は「変更を作って push する」なので、**commit が止まる以上 push すべき中身が作れない**という形では守れている。しかし「サンドボックスを入れたのでネットワーク副作用も止まる」とは書けない。ファイルシステムのサンドボックスはファイルシステムしか止めない。

Seatbelt はネットワークも制限できるが、`gh issue comment`（`triage` / `design` の仕事そのもの）と `gh pr create`（止めたいもの）は**同じホストへの同じ HTTPS** なので、ドメイン単位では分離できない。

## D5. Claude Code 自身のサンドボックスは使わない（運用判断）

Claude Code には OS レベルの Bash サンドボックスが組み込まれており（macOS では同じ Seatbelt、`sandbox.filesystem.denyWrite` 等の設定キーを持つ）、`--settings` から設定できる。**採らない** — 運用者の判断による。

客観的な性質だけ記録しておく（将来の再検討用）:

- **ツール固有である。** claude にはあるが opencode には無い。codex は別物として `--sandbox read-only` を持つ。totsuka の層で持てば全ツールに一様にかかる
- **既定で degrade-open する。** 依存が無い / プラットフォーム非対応のときは警告だけ出して**サンドボックス無しで実行**する。`sandbox.failIfUnavailable` を立てない限り、効いているかどうかが静かに変わる
- `allowUnsandboxedCommands` / `excludedCommands` という抜け道が設定側にある

D1〜D3 の機構はこれらの性質を持たない代わりに、shim の配布と `sandbox-exec` の寿命という別のコストを持つ（下記）。

# 不採用案

## read-only の bind mount

Linux の `mount --bind -o ro` に相当するものが macOS に無い。`hdiutil` で read-only イメージを作る案は worktree を作り直すたびにイメージを作ることになり、`git worktree` の運用と噛み合わない。

## 別ユーザー / ACL（`chmod +a`）で書けなくする

worktree を別 uid に持たせる案は、totsuka 自身が worktree を作成・削除できなくなる（あるいは sudo が要る）。ACL は git が作るファイルに継承させる必要があり、`git worktree add` のたびに付け直す運用が要る。**どちらも「エージェントのプロセスだけを制限する」ではなく「ファイルの側を変える」形なので、totsuka 自身の操作まで巻き込む**。

# Consequences

## 分かったこと

- **macOS では現実的に可能で、totsuka 側だけで配線できる。** 「不可能なので事後検出で運用する」という結論にはならなかった
- 実現すれば `triage` / `design` は heredoc でもリダイレクトでも好きに使ってよく、それでもリポジトリを変更できない。`Bash(...)` パターンという弱い層を捨てられる
- `--body-file` 用の書ける場所は `/tmp` がそのまま使える（#418 の調査項目 4）。専用の scratch を掘る必要はない
- `.git` を deny しても `git log` / `git status` は通る（#418 の調査項目 3）

## 引き受けることになるコスト

- **`git push` と `gh` は止まらない**（D4）。「保証」と書けるのはファイル変更と commit までである
- **`sandbox-exec` は Apple が deprecated と表示している。** 現に動いているが、将来の macOS で消える可能性がある。消えたときに degrade-open するか fail-closed にするかは実装時の判断
- **shim をどこに置き、どう配るか**が新しい運用面になる。`totsuka setup` / `doctor` の対象が増える
- **claude 以外のツールでは shim の中身が変わる。** codex は自前の `--sandbox read-only` を持つので二重にかける意味が薄い
- **Linux は未調査。** CI と将来の運用のために bubblewrap 等を別途見る必要がある（#418 の調査項目 6 は未着手）

## 送った先

**実装しないと決めた**（[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md)。#446 として起票したものを、着手せずクローズした）。この ADR は「可能である」「どう配線するか」「どこまでしか守れないか」を確定させた**調査結果としてそのまま有効**で、方針が変われば調査からやり直す必要はない。

[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) の事後検出は「当面」ではなく**最終形**になった。read-only profile の read-only 性は保証されない。

# 検証

- **すべて実機計測**（2026-08-11、macOS Darwin 24.6 / herdr 0.7.5 / Claude Code 2.1.227 / git 2.x）。D1 の表・D2 のパス・D4 の push 出力は実行結果の転記である
- D3 は隔離した herdr named session（`herdr --session totsuka-verify server`）に対し、`main` = `589c810` ビルドの `agent-ide-herdr` プラグインを stdio で駆動して測った。shim が呼ばれたことはログファイルで、サンドボックスが効いていることは pane 内の本物の claude に書き込みを試させて確認した
- **未検証**: Linux での実現性、`sandbox-exec` の将来、codex / opencode での同等機構、ネットワーク制限を併用した場合の挙動、shim を挟んだ状態での長時間運用
