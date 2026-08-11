---
type: Reference
title: herdr サイドバー設定（[ui.sidebar.*] のトークン語彙）
description: "herdr の左サイドバー（spaces / agents）の行構成を決める [ui.sidebar.*].rows の書き方。組み込みトークンの一覧、$name によるメタデータ参照、rows_by_agent によるエージェント種別ごとの差し替え、インラインスタイル、1 パネル 16 行・1 行 16 トークンの上限（report_metadata 側の 16 とは別物）を、herdr 0.7.5 / protocol 17 の実機確認から記録する。spaces と agents の語彙は包含関係ではなく、どちらにもリポジトリ名のトークンは無い。"
resource: https://herdr.dev/docs/
tags: [herdr, ui, sidebar, reference, 417]
owner: tomoya-k31
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T15:40:00+09:00 }
stale_after: 2027-02-09
sources:
  - id: herdr-probe-2026-08-09
    resource: herdr 0.7.5 / protocol 17 の実機プローブ（`herdr api schema --json` と workspace 作成による実測）
    title: herdr 実機プローブ（2026-08-09）
---

# このドキュメントについて

[herdr Socket API](/references/herdr-socket-api.md) が totsuka の**プラグインが叩く API** を扱うのに対し、
本ドキュメントは **`~/.config/herdr/config.toml` の表示設定**を扱う。totsuka はこのファイルを
**一切書き換えない**（[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D6。herdr と運用者のものだから）
ので、サイドバーを整えるのは手作業になる。

実際に貼るスニペットは [herdr サイドバーに repo / タスクを出す](/operations/herdr-sidebar-setup.md) にある。

# 行の構成

`[ui.sidebar.spaces].rows` と `[ui.sidebar.agents].rows` に**トークン列の配列**を書く。
1 要素が 1 行、行の中の 1 要素が 1 トークン。

```toml
[ui.sidebar.spaces]
rows = [
  ["state_icon", "workspace"],
  ["branch", "git_status"],
]
```

| 制約 | 値 |
|---|---|
| 行数 | 1 パネルにつき 16 まで |
| 1 行のトークン数 | 16 まで |
| 反映 | `herdr server reload-config`（再起動は不要） |

（行数・トークン数の上限は #417 の実機プローブによる。`report_metadata` の「1 コール 16 トークンまで」とは
**別の 16** である — あちらは [Socket API](/references/herdr-socket-api.md) 側の制約で、こちらは表示側。）

# 組み込みトークン

**2 つのパネルの語彙は「片方がもう片方を含む」関係ではない。** herdr 同梱の設定テンプレート
（`strings herdr` で読める既定のコメント）が挙げているのは:

| パネル | トークン |
|---|---|
| spaces | `state_icon` `state_text` `workspace` `branch` `git_status` |
| agents | `state_icon` `state_text` `workspace` `tab` `pane` `agent` `terminal_title` `terminal_title_stripped` |

- **agents に `branch` / `git_status` は無い** — ブランチを agent 行に出すことはできない
- **spaces に `tab` / `pane` / `agent` / `terminal_title*` は無い**

**リポジトリ名のトークンはどちらにも無い。** これが [ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) の
出発点で、totsuka が `$repo` を metadata token として報告している理由である。pane のレコードは `cwd` を
持っているが、それを描画する手段が無い。

`terminal_title_stripped` は Claude Code が OSC で出す作業要約がそのまま入る
（実測値: `"Herdrのメニューでリポジトリとブランチ情報を表示"`）。**totsuka 側の実装ゼロで使える。**

# `$name` — メタデータトークンの参照

`$` を付けた名前は、報告されたメタデータトークンを指す。

**解決先はパネルによって違う**:

| パネル | `$name` が引くもの |
|---|---|
| spaces 行 | **workspace** のメタデータ |
| agents 行 | **pane** のメタデータ |

だから totsuka は `workspace.report_metadata` と `pane.report_metadata` の**両方**を呼ぶ
（[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D1）。片方だけでは片方のパネルしか埋まらない。

totsuka が報告するトークン（`[identity] enabled = true` のとき）:

| `$name` | 中身 |
|---|---|
| `$repo` | `[[repositories]].name` |
| `$task` | タスクのタイトル（整形済み） |
| `$mode` | `plan` / `implement` |
| `$totsuka_task` | 機械識別子。**表示しないこと**（不透明な ID がそのまま出る） |

**未報告のトークンは空になる。** オペレータが自分で開いた space、および**手で起動した agent** には
`$repo` も `$task` も載らない。したがって:

- **どの行も、常に非空のトークンを 1 つ以上含めること。** 報告されたトークンだけで組んだ行は、
  報告が無い相手では `state_icon` だけになる。spaces なら `workspace`、agents なら
  `terminal_title_stripped` あたりが確実
- **可変長のトークン（`workspace` / `terminal_title_stripped`）は行の最後に置く。** 先に置くと、
  幅の足りないサイドバーで後続のトークンが押し出される
- **agents 行の主語に `workspace` を使わない。** 1 つの space に別リポジトリの tab を足せるので、
  その space で動いている別リポジトリの agent が space 名で名乗ることになる

# `rows_by_agent` — エージェント種別ごとの差し替え

```toml
[ui.sidebar.agents.rows_by_agent]
claude = [ ... ]
```

**置き換えであって追加ではない。** `claude` に書いた行構成が、`[ui.sidebar.agents].rows` の代わりに
使われる。

# インラインスタイル

トークンは文字列でも、`{ token = "...", ... }` のテーブルでも書ける。

```toml
rows = [["state_icon", { token = "$repo", fg = "#89b4fa", bold = true }]]
```

# 関連

- [herdr Socket API](/references/herdr-socket-api.md) — `workspace.report_metadata` / `pane.report_metadata` の形と、値の 80 文字上限・`source` の 32 スロット制限
- [ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) — totsuka がどのトークンをなぜ報告するか
- [herdr サイドバーに repo / タスクを出す](/operations/herdr-sidebar-setup.md) — 貼って動くスニペット
