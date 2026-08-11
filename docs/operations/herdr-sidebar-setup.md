---
type: Runbook
title: herdr サイドバーに repo / タスクを出す（一回きりの設定）
description: "totsuka が dispatch 時に報告する repo / task / mode のメタデータトークンをサイドバーに出すための ~/.config/herdr/config.toml スニペットと、反映手順・確認方法・出ないときの切り分け。totsuka はこのファイルを書き換えないので手作業になる。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [herdr, ui, sidebar, setup, 417]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T15:10:00+09:00 }
owner: tomoya-k31
---

# これは何のための手順か

totsuka は dispatch のたびに、herdr の workspace と pane へ「どのリポジトリの・どのタスクを・どのモードで」を
メタデータトークンとして報告する（#417、[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md)）。
**それを画面に出すかどうかは herdr 側の設定**で、totsuka は `~/.config/herdr/config.toml` を
一切書き換えない — herdr と運用者のファイルだからである（[click-to-focus セットアップ](/operations/click-to-focus-setup.md) と同じ扱い）。

つまり**このスニペットを手で入れるまで、報告しても表示は変わらない。**

やらない場合の見え方: spaces / agents の両パネルに `totsuka: サイドバーを直す` という
workspace label は出る（#417 D4 の rename）。行が増えないだけである。

**サイドバーの幅も足りないことが多い。** 既定 36 桁では `totsuka: {タイトル}` が切れる。
`[ui].sidebar_width`（`sidebar_min_width` / `sidebar_max_width` も同じ層）で変えられるが、
実行中セッションは `session.json` に永続化した値を持つので、境界をマウスでドラッグする方が早い。

# 手順

`~/.config/herdr/config.toml` に追記する。

```toml
[ui]
# 既定 36 では "totsuka: {タイトル}" が切れる。好みで調整する。
sidebar_width = 44

# branch / git_status は **space 単位**のトークン。1 つの space に別リポジトリの
# tab をぶら下げられる以上、「どれの branch か」が言えないので出さない。
# $repo も出さない — rename 後の label が repo で始まるので重複する。
[ui.sidebar.spaces]
row_gap = 0
rows = [["state_icon", "workspace"]]

[ui.sidebar.agents]
row_gap = 0
rows = [["state_icon", "workspace", "tab"], ["agent"]]

# 1 行目の主語は **pane が居るリポジトリ**であって space ではない。
# workspace label を主語にすると、totsuka の space に足した別リポジトリの tab で
# 動いている agent が「totsuka」と名乗ってしまう。workspace は 2 行目へ dim で落とす。
#
# 1 行目に **常に非空のトークンを 1 つ以上**混ぜること。$repo / $mode は
# メタデータを報告した pane にしか載らないので、報告されないトークンだけで
# 組むと手で起動した agent の行が state icon だけになる。
# ここでは terminal_title_stripped（Claude Code の OSC 作業要約）がその役。
[ui.sidebar.agents.rows_by_agent]
claude = [
  [
    "state_icon",
    { token = "$repo", fg = "#89b4fa", bold = true },
    { token = "$mode", fg = "#f9e2af" },
    { token = "terminal_title_stripped", fg = "#a6adc8" },
  ],
  [{ token = "workspace", dim = true }],
]
```

反映（再起動は不要）:

```bash
herdr config check          # 未知キーはここで名指しで報告される
herdr server reload-config
```

**`terminal_title_stripped` は totsuka 側の実装を待たずに今日入れられる**（`$repo` / `$mode` が
空になるだけ）。Claude Code が OSC で出す作業要約で、これ自体は totsuka と無関係に動く。

## 手で起動した agent にもリポジトリ名を出す（任意）

**herdr に「リポジトリ名」の組み込みトークンは無い。** pane のレコードは `cwd` を持っているが、
それを描画するトークンが無いので、値を出す唯一の手段はメタデータの報告である。totsuka は自分が
dispatch した pane にしか報告しないため、**手で起動した agent の `$repo` は空のまま**になる。

シェル側から報告すれば埋まる。zsh なら:

```zsh
# totsuka が dispatch した pane では走らせないこと（下記）
if [[ -n $HERDR_ENV && -n $HERDR_PANE_ID && -z $TOTSUKA_JOB_ID ]]; then
  autoload -U add-zsh-hook
  _herdr_report_repo() {
    local root
    root=$(command git rev-parse --show-toplevel 2>/dev/null) || root=''
    # 同じ repo なら herdr を叩かない。ただし「まだ一度も報告していない」と
    # 「repo の外に居る」はどちらも空文字なので ${+set} で区別する — 同一視すると、
    # pane に前のシェルが残したトークンがある状態で非 repo から起動したときに
    # 下の clear が走らず、古い repo 名が残る。
    if [[ ${_herdr_reported_repo_root+set} == set && $root == $_herdr_reported_repo_root ]]; then
      return
    fi
    _herdr_reported_repo_root=$root
    if [[ -n $root ]]; then
      command herdr pane report-metadata "$HERDR_PANE_ID" \
        --source shell --token repo="${root:t}" &>/dev/null
    else
      command herdr pane report-metadata "$HERDR_PANE_ID" \
        --source shell --clear-token repo &>/dev/null   # repo の外では消す
    fi
  }
  add-zsh-hook chpwd _herdr_report_repo
  _herdr_report_repo
fi
```

`HERDR_ENV` / `HERDR_PANE_ID` は herdr が pane の環境に入れる（実測）ので、pane を引くための
API 呼び出しは要らない。

**`TOTSUKA_JOB_ID` のガードは必須。** トークン名はコンテナごとにグローバルで、`--source` は
名前空間ではない（[herdr Socket API](/references/herdr-socket-api.md) 参照）ので、totsuka が
dispatch した pane で走らせると totsuka の `repo` を上書き・削除してしまう。しかも totsuka の
worktree では `${root:t}` は **worktree 名**（`github-42` 等）であって `[[repositories]].name` では
ない。伴走シェルには `TOTSUKA_JOB_ID` が届かないが、あちらは別 pane なのでエージェントの
トークンには触れない。

# 確認

dispatch 中に別シェルから:

```bash
herdr workspace list   # label が "{repo}: {タイトル}"、tokens に totsuka_task / repo / task / mode
herdr pane list        # agent pane に同じ token。伴走シェルには無い
```

**rename 後は所有判定が token 単独になる**（label が `totsuka ` で始まらなくなるため）。したがって
**herdr の再起動やセッション復元をまたいで `tokens` が残るかは、実機で確かめる必要がある**。
消えるなら、その worktree の pane は `tt session list` からも `tt doctor` の孤児検出からも消える:

```bash
herdr workspace list | grep totsuka_task   # 再起動の前後で比較する
```

消えることが分かった場合の対処は「rename しない」（`[identity] enabled = false`）か、
identity を再報告する経路を足すかで、[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) に
追記して決め直すこと。

# 出ないときの切り分け

| 症状 | 見るところ |
|---|---|
| 行が増えない | `herdr server reload-config` を実行したか。`herdr config check` は**未知キーを名指しで報告する**（`unknown config key ui.…; ignoring key`）ので、綴りはそれで確かめる |
| `$repo` だけ空 | Orchestrator が protocol 0.4.1 以上か（`repo_name` は 0.4.1 の追加。それ以前は報告されない） |
| workspace label が `totsuka {id}` のまま | **rename しない条件が 4 つある。いずれも正常な縮退**（[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D4）。`totsuka logs` の文言で切り分ける: ①identity 報告の失敗 → `could not report … identity` ②rename 自体の失敗 → `could not rename the workspace` ③`repo_name` が届いていない（Orchestrator が 0.4.1 未満）→ ログ無し ④タスク ID が長すぎて（80 文字超）マーカー token を載せられない → `task id exceeds herdr's token limit`（`--debug` が要る） |
| すべて出ない | `plugins/herdr.toml` の `[identity] enabled` が `false` になっていないか |
| 自分で開いた space / 手起動の agent の行が空になる | `rows` はグローバルなので、報告されたトークンだけで組んだ行は空になる。**1 行目に常に非空のトークンを 1 つ以上**混ぜること（spaces なら `workspace`、agents なら `terminal_title_stripped` 等）。上のスニペットはそうしてある |
| agent が居るリポジトリではなく space 名が出る | 1 つの space に別リポジトリの tab を足すとこうなる。`workspace` を 1 行目の主語にしないこと。手起動の agent に repo を出すには上の shell フックが要る |
| `$mode` が長い label に押し出される | 可変長の `workspace` は行の**最後**に置く。固定長のトークンを先に |

**totsuka を元に戻したいだけなら** `plugins/herdr.toml` に `[identity] enabled = false` を書く。
報告も rename も止まり、#417 以前と同じ挙動になる。herdr 側の設定はそのままでよい
（`$repo` などが空になるだけ）。

# 関連

- [herdr サイドバー設定のトークン語彙](/references/herdr-sidebar-config.md) — `rows` に書ける値の一覧
- [ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) — なぜ totsuka がこのファイルを書き換えないか（D6）
- [config.toml リファレンス](/development/config-reference.md) — `plugins/herdr.toml` の `[identity]`
