---
type: Decision
title: ADR-0082 orca プラグインを herdr と同じ契約で駆動する — totsuka の worktree に端末を開き、tool_launch をそのまま起動する
description: "orca プラグインが worktree を自前で作り（worktree create）、独自の --agent 起動と state dot の poll で完了を判定していたのをやめ、herdr と同じ契約（tool_launch をそのまま起動・hook で完了報告・exit の deadman・pane_control・diagnostics_snapshot）にそろえる決定。orca 端末をセッションとし、Orchestrator が切った worktree に terminal create で開く。起動は exec env … で端末の寿命をエージェントに一致させ、プロンプトは orca がエージェントを認識してから terminal send で送る。所有マーカーは worktree の orca comment（タブタイトルはエージェントに上書きされる）。すべて orca 1.4.205 の実測に基づく。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-orca
tags: [decision, adr, orca, agent-ide, plugin, hooks, pane-control, tool-launch]
generated: { by: claude-code/opus-5, at: 2026-09-19T04:00:00+09:00 }
verified:
  - { by: claude-code/opus-5, at: 2026-09-19T03:46:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: orca-cli
    resource: /references/orca-cli-control.md
    title: orca CLI 制御サーフェス（実測節を含む）
---

# Status

stable。実装と同じ PR で確定した。以下の判断を**上書きする**（各 ADR 本文は当時の記録として残す）:

- [ADR-0071](/decisions/adr-0071-task-identifier-naming.md) の「orca の worktree 名」— orca は worktree を作らなくなったので、名付ける対象が無い
- [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) の「orca は `tool_launch` を読まないので下限を上げない」— 読むようになった（下限はすでに 0.6.0 で、0.2.3 を含意する）
- [ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) の「orca（非 hook エージェント）には core のプロンプトが届かない」— hook 経由で届くようになった
- [ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) の orca `plan_prefix` — 設定キーごと廃止
- [ADR-0005](/decisions/adr-0005-click-to-focus.md) / [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md) / [ADR-0013](/decisions/adr-0013-orphan-pane-detection.md) の「orca は `pane_control` を宣言しないので呼ばれない」— 宣言するようになった

# Context

orca プラグインは herdr プラグインと「プロトコル面は同一」とされながら、中身は別物だった。

| | herdr | orca（変更前） |
|---|---|---|
| worktree | Orchestrator が切ったものを使う | **`orca worktree create` で自前にもう 1 本作る** |
| 起動 | `tool_launch`（hook 設定・env 込み）をそのまま | 独自の `--agent claude --prompt …`。`tool_launch` を読まない |
| 完了 | Claude Code の Stop/SessionEnd hook | `worktree ps` の state dot を poll し `done` で完了 |
| plan | `tool_launch` の plan フラグ | プロンプト前置き文（`plan_prompt_prefix`）だけ |
| cancel | pane を閉じる | **`worktree rm --force`** |
| pane_control / snapshot | 宣言 | 非宣言 |

問題は 3 つあった。

1. **実機で動かない。** `orca --json` の出力は `{"id": <CLI リクエスト id>, "ok": …, "result": {…}}` という
   envelope だが、プラグインはトップレベルの `id` を worktree id として読んでいた。以後の `id:<…>` 参照は全部空振りする。
   fake CLI のテストは envelope の無い形を返していたので、CI は通っていた。
2. **worktree が 2 本になる。** Orchestrator が切った worktree（`worktree_path`）は使われず、orca が別の
   worktree を作る。Orchestrator の cleanup・retry・`doctor` の孤児検出はその存在を知らない。
3. **hook が効かない。** hook 設定は `tool_launch` にしか乗らないので、orca 経由のタスクは deny リスト・
   不可視プロンプト・完了報告のすべてから外れていた（[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) がこれを既知の穴として記録している）。

# Decision

## D-1. セッションは orca の端末。Orchestrator の worktree に開く

`orca terminal create --worktree path:<worktree_path>` で端末を開き、返る handle（`term_<uuid>`）を
`session_id` にする。orca は**登録済みリポジトリの git worktree を自分で見つける**ので、Orchestrator が
`git worktree add` した worktree もそのまま `path:` で引ける（実測）。見つからない（`selector_not_found`）のは
リポジトリが未登録のときで、エラーは `orca repo add --path <repository>` を案内する。

プラグインは worktree を**作らず、消さない**。cancel / release は端末（タブ）を閉じるだけ。

handle は再利用されない（閉じた handle は `terminal_handle_stale`、終了した handle は `connected: false` の
記録として残る）。herdr の位置ベース pane id と違って取り違えが起きないので、`session_id` は handle そのもので足り、
release の `expect_cwd` 照合は保険の意味しか持たない。

## D-2. 起動は `tool_launch` をそのまま。`exec env K=V … program args…` として打ち込む

herdr と同じく `tool_launch` を**解釈せずに**起動し、無ければ dispatch を `INVALID_PARAMS` で失敗させる
（自前の argv で代用すると `--settings` が抜け、完了が報告されない）。

`terminal create --command` は**端末のログインシェルに文字列として打ち込まれる**（exec されない、実測）。そこで:

- 全語を単一引用符でクォートする（sh / bash / zsh / fish が同じに読む形）
- env は `terminal create` が受け取らないので `env K=V` で渡す
- **先頭に `exec` を付ける。** 付けないとコマンドはシェルの子として走り、終了してもシェルが残るので
  `terminal wait --for exit` が永遠に発火しない。`exec` でシェルを置き換えると、端末の寿命がエージェントの寿命になる

これで `hook_completion` を宣言できる。

## D-3. プロンプトは「orca がエージェントを認識してから」送る

`terminal wait --for tui-idle` → `terminal show` が `agentIdentity` を返すまで待つ → `terminal send --text … --enter --wait-submit`。

`tui-idle` だけでは足りない。実測で、起動から約 4 秒で `tui-idle` は満たされたが `agentIdentity` はまだ `null` で、
その瞬間に送ったプロンプトは `provider: "unsupported"`（生のキー入力、bracketed paste 無し）として出て行き、
**Claude に届かなかった**。認識後に送ると `provider: "claude"`・`stages: [input_accepted, turn_started]` で、
複数行のタスク本文が 1 ターンとして届く。

orca が統合を持たないツールは永久に認識されないので、30 秒待って送る（警告のみ）。認識待ちの間に端末が終了したら
起動失敗で、`resume_session_id` 付きなら `SESSION_UNRESUMABLE`（`claude --resume <無効 id>` は終了し、`exec` によって
それが端末の終了になる）。

## D-4. 所有マーカーは worktree の orca comment `totsuka {task_id}`

herdr の workspace ラベルと同じ文字列で、`doctor` はこの接頭辞を剥がして元タスクを引く。dispatch の直後に
`worktree set --comment` で付ける（`[orca.identity]` が有効なら同じ呼び出しで表示名 `{repo}: {title}` も）。
`session/list` は `terminal list` の `worktreeId`（`<repoId>::<path>`）を `worktree list --repo id:<repoId>` の
comment と突き合わせ、comment が `totsuka ` で始まる worktree の端末を 1 worktree 1 行で返す（orca がエージェントを
認識している端末を優先）。`--repo` を付けない `worktree list` は external worktree を返さないので、repo ごとに引く。

**当初はタブタイトルに付けていたが、実機 e2e で崩れた**（2026-09-19）:

- `terminal create --title` は初期値にすぎず、Claude の OSC タイトルに数秒で置き換わる
- `terminal rename` も、**作業中の** Claude には 5 秒以内に上書きされた（`◑ …` → `✳ …`）。
  「rename は保たれる」という最初の実測は、Claude がタイトルを更新していない（アイドルの）ときに取ったもので、
  一般化できなかった
- rename はそのうえ、orca の `agentIdentity`（D-3 の認識待ちが読む）をタイトルと一緒に消していた

worktree のメタデータを書くのは orca の CLI と利用者だけで、worktree は Orchestrator がタスクごとに切るので、
comment はその中で開くすべての端末の持ち主を表す。代償として、**人間が同じ worktree に開いた端末**も
所有端末として数えうる（herdr でも companion shell が同じ扱いになるのと同じ種類の曖昧さ）。
付けられなかったときは警告に留める（`session/list` に出ないだけで、動いているエージェントの dispatch を
失敗させるほうが高くつく）。

## D-5. state stream は exit の deadman。**終了はすべて `failed`**

`terminal wait --for exit` をブロックで繰り返し、満たされたら `failed` を 1 回送って終わる。handle が消えた場合も同じ。

herdr は明示的な exit code 0 を「正常終了」として黙って流すが、orca ではそれができない。**実測で `exit 3` した
プロセスが `exitCode: 0`・`exitCause: unknown` と報告された。** 対話型エージェントは完了しても自分からは終了しないし、
hook が完了させた後のタスクに届いた `failed` は Orchestrator が無視するので、実害は無い。

## D-6. capability は herdr と同じ 4 つ

`pane_control`（`terminal switch` / `close --tab` / `list`）・`state_stream`・`hook_completion`・
`diagnostics_snapshot`（`terminal read --screen`、描画できなければ蓄積出力へ縮退）。

## D-7. 設定は「どこに届くか」と「どう見せるか」だけ

`[orca]` は `orca_bin` / `request_timeout_secs` / `[orca.layout]`（`shell`・`direction`）/
`[orca.identity]`（`enabled`）。起動内容は Orchestrator の `[tools]` が決めるので、`agent` / `setup` /
`repo_selector` / `plan_prompt_prefix` / `poll_interval_ms` は廃止し、残っていれば `initialize` がキー名と
代替を挙げて `CONFIG_INVALID` にする（herdr の #411 と同じ墓標方式）。

`layout.shell` の既定は herdr と逆で **off**。herdr はタスク専用 workspace を作るので分割しないとシェルが無いが、
orca はサイドバーに worktree があり、端末はそこから 1 クリックで開ける。分割は orca のフォーカスも新しいペインへ移す。

# Consequences

- orca 経由のタスクも hook（deny リスト・不可視プロンプト・完了報告・`waiting_input`）に乗る。
  「非 hook エージェント」として orca を特別扱いしていた説明は、mock だけを指すようになる
- worktree が 1 本になり、cleanup・retry・`doctor` が orca のタスクにもそのまま効く
- **リポジトリを orca に登録しておくことが前提になる**（`orca repo add --path …`）。未登録だと dispatch が
  その旨のエラーで失敗する
- 実測に依存した挙動（`--command` が打ち込みであること、`--title` が初期値であること、`agentIdentity` の出方、
  `exitCode` の信頼性）は orca の日次リリースで変わりうる。[リファレンス](/references/orca-cli-control.md) の実測節に
  バージョン付きで記録し、`stale_after` で見直しを強制する

# Alternatives

| 案 | 採らなかった理由 |
|---|---|
| `worktree create` を残し、Orchestrator の worktree を使わない | worktree が 2 本になり、Orchestrator の掃除・再試行・孤児検出がすべて届かない |
| プロンプトを起動引数に入れる（`claude … "<prompt>"`） | シェルに打ち込まれるので、複数行の引数は継続行として解釈される。herdr でも同じ理由で不採用 |
| `exec` を付けず、シェルの子として起動する | エージェントが終了してもシェルが残り、exit の deadman が機能しない |
| 所有マーカーをタブタイトルにする（当初案） | 作業中のエージェントが OSC で上書きし続ける。`rename` も 5 秒保たなかった（D-4） |
| exit code 0 を正常終了として流す（herdr と同じ） | orca の `exitCode` が実際の終了コードを反映しない（実測） |

# 関連

- [agent-ide-orca](/components/agent-ide-orca.md) / [agent-ide-herdr](/components/agent-ide-herdr.md)
- [orca CLI 制御サーフェス](/references/orca-cli-control.md)
- [ADR-0004 フック完了シグナル](/decisions/adr-0004-hook-completion-signal.md) / [ADR-0014 ツール抽象](/decisions/adr-0014-tool-abstraction.md)
