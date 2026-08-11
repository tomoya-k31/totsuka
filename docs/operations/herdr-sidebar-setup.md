---
type: Runbook
title: herdr サイドバーに repo / タスクを出す（一回きりの設定）
description: "totsuka が dispatch 時に報告する repo / task / mode のメタデータトークンをサイドバーに出すための ~/.config/herdr/config.toml スニペットと、反映手順・確認方法・出ないときの切り分け。totsuka はこのファイルを書き換えないので手作業になる。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [herdr, ui, sidebar, setup, 417]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-12T00:30:00+09:00 }
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

# 手順

`~/.config/herdr/config.toml` に追記する。

```toml
[ui.sidebar.spaces]
row_gap = 0
rows = [
  ["state_icon", { token = "$repo", fg = "#89b4fa", bold = true }, "workspace"],
  ["branch", "git_status"],
]

[ui.sidebar.agents]
row_gap = 0
rows = [["state_icon", "workspace", "tab"], ["agent"]]

[ui.sidebar.agents.rows_by_agent]
claude = [
  ["state_icon", { token = "$repo", fg = "#89b4fa", bold = true }, { token = "$mode", fg = "#f9e2af" }],
  [{ token = "terminal_title_stripped", fg = "#a6adc8" }],
]
```

反映（再起動は不要）:

```bash
herdr server reload-config
```

**`rows_by_agent.claude` ＋ `terminal_title_stripped` の 2 行は totsuka 側の実装を待たずに今日入れられる**
（`$repo` / `$mode` が空になるだけ）。`terminal_title_stripped` は Claude Code が OSC で出す作業要約で、
これ自体は totsuka と無関係に動く。

# 確認

dispatch 中に別シェルから:

```bash
herdr workspace list   # label が "{repo}: {タイトル}"、tokens に totsuka_task / repo / task / mode
herdr pane list        # agent pane に同じ token。伴走シェルには無い
```

# 出ないときの切り分け

| 症状 | 見るところ |
|---|---|
| 行が増えない | `herdr server reload-config` を実行したか。`[ui.sidebar.*]` の綴り |
| `$repo` だけ空 | Orchestrator が protocol 0.4.1 以上か（`repo_name` は 0.4.1 の追加。それ以前は報告されない） |
| workspace label が `totsuka {id}` のまま | identity 報告が失敗している。**これは正常な縮退**で、失敗時は rename しない設計（[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D4）。`totsuka logs` に `could not report … identity` が出ているはず |
| すべて出ない | `plugins/herdr.toml` の `[identity] enabled` が `false` になっていないか |
| 自分で開いた space の行が空行だらけ | `rows` はグローバルなので、`$repo` / `$task` だけの行は人間の space で空になる。1 行目に組み込みトークン（`workspace` 等）を混ぜること |

**totsuka を元に戻したいだけなら** `plugins/herdr.toml` に `[identity] enabled = false` を書く。
報告も rename も止まり、#417 以前と同じ挙動になる。herdr 側の設定はそのままでよい
（`$repo` などが空になるだけ）。

# 関連

- [herdr サイドバー設定のトークン語彙](/references/herdr-sidebar-config.md) — `rows` に書ける値の一覧
- [ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) — なぜ totsuka がこのファイルを書き換えないか（D6）
- [config.toml リファレンス](/development/config-reference.md) — `plugins/herdr.toml` の `[identity]`
