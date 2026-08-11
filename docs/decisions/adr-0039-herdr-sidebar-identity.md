---
type: Decision
title: ADR-0039 herdr サイドバーの identity は metadata token で運び、リポジトリ名はプロトコルの追加フィールドで渡す
description: "herdr の左サイドバーに「どのリポジトリの・どのタスクを・どのモードで」を出すため、identity を label ではなく workspace / pane の metadata token として報告し、リポジトリ名は TaskDispatchParams.repo_name（プロトコル 0.4.1、純追加）で渡す決定。worktree.open によるグルーピング・pane.rename・display_agent の各案を採らない理由と、サイドバー設定を totsuka が書き換えない理由。"
tags: [herdr, protocol, ui, identity, 417]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T00:00:00Z }
sources:
  - id: herdr-probe-2026-08-09
    resource: herdr 0.7.5 / protocol 17 の実機プローブ（`herdr api schema --json` と workspace 作成による実測）
    title: herdr 実機プローブ（2026-08-09）
---

# Status

Accepted — 2026-08-11（[#417](https://github.com/tomoya-k31/totsuka/issues/417)）。

実装は 3 本に分けて入る: **PR-1** = プロトコル 0.4.1 の `repo_name` ＋ core（挙動変化なし。この ADR はここで入る）、**PR-2** = プラグインの identity 報告（見た目は変わらない）、**PR-3** = `workspace.rename` と docs 一式。**実機検収は PR-3 の後**で、それまで `verified` は付けない。

# Context

herdr の左サイドバー（spaces / agents）に出ている totsuka のエージェント行は、**`totsuka C0BNAU8KKG8:1754236800.123456` という不透明な文字列と、全行同じ `claude` だけ**である。worktree は detached HEAD（[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)。ブランチはエージェント自身が付ける）なので spaces の `branch` トークンも当初は空。**リポジトリ名に至っては表示する手段が現状ゼロ**。

複数タスクを並行させると、どの pane が何をしているのか行から読めない。

herdr 側に必要な機構は protocol 17 / 0.7.5 で既に揃っている（`workspace.report_metadata` / `pane.report_metadata` と `[ui.sidebar.*].rows` のトークン列）。**totsuka 側で足りないのは 1 つだけ — リポジトリ名を知る手段**である。

依存: [#416](https://github.com/tomoya-k31/totsuka/issues/416)（`session/list` が実機で機能していない件）。本 ADR の所有判定はその上に乗る。

# Decision

## D1 — identity は label ではなく metadata token で運ぶ

dispatch の `workspace.create` 直後（`start_agent` の**前**）に、**workspace と root pane の両方**へ同じトークンを報告する。`$name` の解決先が agents 行では pane メタデータ、spaces 行では workspace メタデータなので、両パネルに出すには両方に要る。

| token | 値 | 用途 |
|---|---|---|
| `totsuka_task` | `task.id` | 機械識別子（表示しない）。所有境界の新しい根拠 |
| `repo` | `repo_name`（D3） | 行の `$repo` |
| `task` | `task.title` を整形・切り詰め | 行の人間可読部 |
| `mode` | `plan` / `implement` | `$mode` |

- `source` は**定数 `"totsuka"`**。1 つの pane / workspace が受け付ける異なる `source` は**生涯 32 個まで**で、clear や expiry でスロットが戻らない。タスク毎の source はこれを食い潰す
- `seq` / `ttl_ms` は付けない。identity は status ではなく、タスクより長生きすべきものである
- **失敗しても dispatch は落とさない**。`apply_layout` と同じく `tracing::warn!` のみ
- `start_agent` の**前**に置くのは、`start_agent` が最大 180 秒のリトライループだからである。後に回すと、オペレータが一番見ている時間帯だけ行が無名になる。ソケット 1 往復 ≒ 25ms（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）で `agent.start` に比べれば誤差

値は **80 文字**（バイトではない）で herdr 側が黙って切る。自前で 79 文字 ＋ `…` に整形するのは、切れたことを見せるため。`char_indices` で char 境界を切ること — `&s[..80]` は日本語タイトルで panic する。

## D2 — 所有境界を workspace label から token へ（#416 の上に乗る）

#416 で `list_sessions` は `pane.list` ＋ `workspace.list` の結合になった。本 ADR はその判定条件に token 経路を足す。pane が自分のものである条件は**いずれか**:

1. `pane.tokens.totsuka_task` がある（新規 dispatch）
2. その workspace の `tokens.totsuka_task` がある
3. その workspace の `label` が `"totsuka "` で始まる（**#416 の経路**。報告が失敗した dispatch と、過去リリースが取りこぼした既存 pane の回収）
4. pane 自身の `label` が `"totsuka "` で始まる（将来 herdr が label を伝播しても正しい）

`SessionInfo.label` は token があれば `format!("totsuka {}", totsuka_task)` を**合成**して返す。これで `doctor_cmd.rs` の `strip_prefix` → `source_task_id` 照合は**無改修**で通り、[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md) の「label が source task id を運ぶ」意味論も保たれる（D4 で label を人間可読にしても壊れない）。

## D3 — リポジトリ名はプロトコルの追加フィールドで渡す（0.4.0 → 0.4.1）

`TaskDispatchParams.repo_name: Option<String>`（`skip_serializing_if`）を足す。値は totsuka の設定上の名前 `[[repositories]].name` ＝ **worktree のパス・ログ・`totsuka status` に出るのと同じ文字列**にして、サイドバーと `tt task show` の表示を一致させる（**ブランチ名ではない** — [ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) 以降、ブランチ名はエージェントがリポジトリの規約から選ぶ）。

**バージョンは 0.4.1（patch）。** 0.x では patch が後方互換な追加で、minor を上げると `<0.5` で束ねた manifest を**無用に全部弾く** — [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) が 0.4.0 でやったのはまさにそれで、あれは弾くことに意味があった。ここには無い。欠落時（旧 core）は `repo` トークンを省略し、label も現状形のまま。エラーにしない。

**不採用案**:

| 案 | 不採用の理由 |
|---|---|
| `worktree_path` からの推測 | `worktree_location` はリポジトリ毎に上書き可で、`{state_dir}/worktrees/{repo}/{name}` は**契約ではなく偶然**である |
| プラグインからの git shell-out | プロセスを一切起動しない IDE アダプタに git 知識を持ち込む。detached worktree では `--git-common-dir` ＋ basename が要り、得られるのは*ディレクトリ名*であって設定名ではない |
| `Task.repo_hint` | source 側の推測値でしかなく、リポジトリ選択で上書きされる。`None` もあり得る |
| herdr の `WorkspaceInfo.worktree.repo_name` | D5 参照 |

## D4 — workspace label を人間可読にする（create → report → rename の順）

1. `workspace.create { label: "totsuka {task.id}" }` — **現状とバイト同一**。所有マーカーが最初の瞬間から存在する
2. identity 報告（D1）
3. **2 が両方成功したときだけ** `workspace.rename { label: "{repo}: {title}" }`（`repo_name` が無ければ rename しない）

herdr 側の一時障害で 2 が落ちても「機械 label のまま・サイドバーが綺麗にならないだけ」で、**label と token の両方から identity が消える瞬間が無い**。

token だけでは足りない理由: `rows` はグローバルで、オペレータが自分で開いた space にも同じ行構成が当たる。`$repo` / `$task` は人間の space では空なので、1 行目から `workspace` トークンを外せない。つまり `workspace` が不透明なままだと**両パネルとも 1 行目が壊れたまま**になる。

## D5 — `worktree.open` によるグルーピングは見送り

`workspace.create` を `worktree.open` に替えると、herdr が親リポジトリ配下に worktree をインデント表示し、`repo_name` も自前で解決してくれる。が、割に合わない:

- **`WorktreeOpenParams` に `env` が無い**（schema 確認済み: `{workspace_id?, cwd?, path?, branch?, label?, focus}`）。`TOTSUKA_HOOK_TOKEN` 等は「herdr が workspace env を root pane に適用し、agent が root pane で動く」（ADR-0032 D-4）から届いている。env を `pane.split --env` 側に移すと root pane と agent pane の役割が反転し、**#387 / #391 のシェル起動レースが住んでいる最も脆い箇所を作り直す**ことになる
- `already_open: true`（オペレータが既に開いている / リトライ）の所有判定が新たに必要。誤ると人の workspace を閉じる
- **token 報告は結局必要**。置き換わるのは `repo` token だけで、しかも herdr の repo 名は totsuka の `[[repositories]].name` と一致する保証が無く、**真実の源が 2 つになる**

**再検討条件**: herdr が `worktree.open` に `env` を追加したとき（または `agent.start` が `env` を取り戻したとき）。

## D6 — サイドバーの `rows` はオペレータの設定。totsuka は書き換えない

`~/.config/herdr/config.toml` は herdr とオペレータのものであり、totsuka が触ってよいファイルではない（[click-to-focus セットアップ](/operations/click-to-focus-setup.md) と同じ扱い）。推奨スニペットを docs に置き、手で入れてもらう。

## 不採用: `pane.rename` / `display_agent` / `title`

- `pane.rename` — `show_agent_labels_on_pane_borders = true` の環境で**不透明な ID が pane 枠に出る**。D2 で代替できる
- `display_agent` — `claude` / `codex` / `opencode` の区別が消える診断上の後退。D4 で 1 行目に repo とタスクが出るので買うものが無い
- `title` — 将来の pane 単位ステータス行の置き場として、ここに記録するに留める

# Consequences

## 良くなること

- サイドバーの 1 行から「どのリポジトリの・どのタスクを・どのモードで」が読める
- 所有判定の根拠が label（表示のための文字列）から token（機械識別子）へ移り、**表示を変えても所有判定が壊れない**ようになる

## 引き受けたコスト

- **dispatch にソケット往復が 2〜3 回増える。** `agent.start` の readiness レース（#387 / #391）に影響しないことは実機で確かめる必要がある
- **プロトコルに表示専用のフィールドが 1 つ増えた。** `repo_name` は totsuka の内部語彙（`[[repositories]].name`）をプロトコルに露出させている。ここを将来変えるとサイドバーの表示が変わる
- **サイドバーの見た目はオペレータの設定次第。** D6 のとおり totsuka は `rows` を書かないので、スニペットを入れていない環境では `$repo` / `$mode` が単に空になる

# 関連

- [#416](https://github.com/tomoya-k31/totsuka/issues/416) / [ADR-0013](/decisions/adr-0013-orphan-pane-detection.md) — 所有判定の土台
- [ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) — protocol 17 の実機作法（`env` が workspace 経由でしか届かないこと）
- [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) — 0.x のバージョン規約（minor を上げると何が起きるか）
- [herdr Socket API](/references/herdr-socket-api.md) — 実測した API 形
