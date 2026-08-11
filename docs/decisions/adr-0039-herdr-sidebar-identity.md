---
type: Decision
title: ADR-0039 herdr サイドバーの identity は metadata token で運び、リポジトリ名はプロトコルの追加フィールドで渡す
description: "herdr の左サイドバーに「どのリポジトリの・どのタスクを・どのモードで」を出すため、identity を label ではなく workspace / pane の metadata token として報告し、リポジトリ名は TaskDispatchParams.repo_name（プロトコル 0.4.1、純追加）で渡す決定。worktree.open によるグルーピング・pane.rename・display_agent の各案を採らない理由と、サイドバー設定を totsuka が書き換えない理由。"
tags: [herdr, protocol, ui, identity, 417]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T16:10:00+09:00 }
verified:
  - { by: human:tomoya-k31, at: 2026-08-11T16:10:00+09:00 }
sources:
  - id: herdr-probe-2026-08-09
    resource: herdr 0.7.5 / protocol 17 の実機プローブ（`herdr api schema --json` と workspace 作成による実測）
    title: herdr 実機プローブ（2026-08-09）
---

# Status

Accepted — 2026-08-11（[#417](https://github.com/tomoya-k31/totsuka/issues/417)）。

実装は 3 本に分けて入った: **PR-1**（[#427](https://github.com/tomoya-k31/totsuka/pull/427)）= プロトコル 0.4.1 の `repo_name` ＋ core、**PR-2**（[#428](https://github.com/tomoya-k31/totsuka/pull/428)）= プラグインの identity 報告、**PR-3**（[#429](https://github.com/tomoya-k31/totsuka/pull/429)）= `workspace.rename` と docs 一式。

## 実機で確認した範囲（2026-08-11、herdr 0.7.5 / protocol 17）

`verified` はこの範囲に対して付けている:

- `workspace.report_metadata` / `pane.report_metadata` が受理される
- **`tokens` が `workspace.list` / `workspace.get` / `pane.list` の 3 つすべてから読み戻せる。**
  所有判定（D2）が読むのは **`workspace.list`** なので、そこに載ることが要点である
- **`workspace.rename` がトークンを保つ**（D4 が前提にしていた点）
- **`agent.start` を挟んでもトークンが保たれる**（D1 の「報告してから起動する」順序が依存している点）
- 日本語のタイトルがそのまま往復する
- 一連の操作の後も pane のレコードに `label` **キーが現れない**（#416 の前提の再確認）。
  herdr は `null` を送るのではなく**キーごと省く** — `agent` も伴走シェルでは同じく省かれる
- **サイドバーの描画。** 報告した pane の行に `$repo` / `$mode` が出ること、オペレータが自分で
  開いた space と手で起動した agent ではそれらが空になることを目視で確認した。**D6 の 3 制約は
  この目視で見つかった** — 最初のスニペットは 3 つとも破っており、手で起動した claude の行が
  `state_icon` だけになり、別リポジトリの tab で動く agent が space 名で名乗っていた

**確認できていないもの**（`verified` はこれらを含まない）:

- **プラグイン経由の dispatch 一周**。上は `herdr` CLI から同じ呼び出しを再現したもので、
  `agent.start` の readiness レース（#387 / #391）に往復 3 回の追加が影響しないかは測っていない
- **herdr 再起動をまたいで `tokens` が残るか**（下の Consequences 参照）
- **`[identity] enabled = false` の否定側**
- **エージェントが終了した後の pane レコードの形。** `agent` が残るなら、その pane が
  `list_sessions` の同点決着で勝ち続ける。挙動としては無害（1 workspace 1 セッションは保たれ、
  代表になるのは `doctor` が探している孤児そのもの）だが、測っていない

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
| `totsuka_task` | `task.id` を**そのまま** | 機械識別子（表示しない）。所有境界の新しい根拠 |
| `repo` | `repo_name`（D3） | 行の `$repo` |
| `task` | `task.title` を整形・切り詰め | 行の人間可読部 |
| `mode` | `plan` / `implement` | `$mode` |

- `source` は**定数 `"totsuka"`**。1 つの pane / workspace が受け付ける異なる `source` は**生涯 32 個まで**で、clear や expiry でスロットが戻らない。タスク毎の source はこれを食い潰す
- **`source` は名前空間ではない**（実機で確認）。トークン名はコンテナごとにグローバルで、別の
  `source` から同じ名前を書けば上書きでき、`--clear-token` は他人が入れた値ごと消す。定数にした
  理由はスロット制限のままだが、**同じ pane に 2 つの書き手が居てはいけない**という帰結が別につく —
  手で起動した agent に repo を出すシェルフックは、totsuka が dispatch した pane では
  走らせてはならない（[サイドバー設定手順](/operations/herdr-sidebar-setup.md)）
- `seq` / `ttl_ms` は付けない。identity は status ではなく、タスクより長生きすべきものである
- **失敗しても dispatch は落とさない**。`apply_layout` と同じく `tracing::warn!` のみ
- `start_agent` の**前**に置くのは、`start_agent` が最大 180 秒のリトライループだからである。後に回すと、オペレータが一番見ている時間帯だけ行が無名になる。ソケット 1 往復 ≒ 25ms（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）で `agent.start` に比べれば誤差

値は **80 文字**（バイトではない）で herdr 側が黙って切る。**表示用のトークン**（`task` / `repo`）は自前で 79 文字 ＋ `…` に整形する — 切れたことを見せるためで、`char_indices` で char 境界を切ること（`&s[..80]` は日本語タイトルで panic する）。

**`totsuka_task` だけは整形も切り詰めもしない。** これは表示ではなく**比較**に使う唯一のトークンで、空白の畳み込みや `…` の付与は「自分の pane が自分の同一性検査に落ちる」「`session/list` が合成した label を `doctor` が `source_task_id` と照合できない」を引き起こす。上限に収まらない id は**切らずに送らない** — herdr が黙って切るので、切れた機械識別子が残るくらいなら無いほうがよく、3 / 4 の label 経路が正しいフォールバックになる。

## D2 — 所有境界を workspace label から token へ（#416 の上に乗る）

#416 で `list_sessions` は `pane.list` ＋ `workspace.list` の結合になった。本 ADR はその判定条件に token 経路を足す。pane が自分のものである条件は**いずれか**:

1. `pane.tokens.totsuka_task` がある（新規 dispatch）
2. その workspace の `tokens.totsuka_task` がある
3. pane 自身の `label` が `"totsuka "` で始まる（将来 herdr が label を伝播したとき。**pane 自身の申告のほうが workspace より具体的**なので、label どうしではこちらが勝つ）
4. その workspace の `label` が `"totsuka "` で始まる（**#416 の経路**。報告が失敗した dispatch と、過去リリースが取りこぼした既存 pane の回収）

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
3. **2 が両方成功し、かつ `totsuka_task` が実際に載ったときだけ** `workspace.rename { label: "{repo}: {title}" }`（`repo_name` が無ければ rename しない）

**ゲートは「呼び出しが成功したか」ではなく「マーカーが読み戻せるか」。** この 2 つは離れる: D1 のとおり
上限を超える（あるいは空の）`task.id` は token を**載せずに**報告するので、`report_metadata` は成功する。
そこで rename すると、`totsuka ` label も token も無い container ができる — **本節が禁じている状態そのもの**で、
`list_sessions` はその pane を落とし（`doctor` から永久に消える）、`release` は拒否する（pane がリークする）。

herdr 側の一時障害で 2 が落ちても「機械 label のまま・サイドバーが綺麗にならないだけ」で、**label と token の両方から identity が消える瞬間が無い**。

token だけでは足りない理由: `rows` はグローバルで、オペレータが自分で開いた space にも同じ行構成が当たる。`$repo` / `$task` は人間の space では空なので、**spaces 行**から `workspace` トークンを外せない。つまり `workspace` が不透明なままだと spaces パネルが壊れたままになる（agents 側で `workspace` を主語にしない話は D6 の制約 2 で、こことは別のパネルの議論である）。

## D5 — `worktree.open` によるグルーピングは見送り

`workspace.create` を `worktree.open` に替えると、herdr が親リポジトリ配下に worktree をインデント表示し、`repo_name` も自前で解決してくれる。が、割に合わない:

- **`WorktreeOpenParams` に `env` が無い**（schema 確認済み: `{workspace_id?, cwd?, path?, branch?, label?, focus}`）。`TOTSUKA_HOOK_TOKEN` 等は「herdr が workspace env を root pane に適用し、agent が root pane で動く」（ADR-0032 D-4）から届いている。env を `pane.split --env` 側に移すと root pane と agent pane の役割が反転し、**#387 / #391 のシェル起動レースが住んでいる最も脆い箇所を作り直す**ことになる
- `already_open: true`（オペレータが既に開いている / リトライ）の所有判定が新たに必要。誤ると人の workspace を閉じる
- **token 報告は結局必要**。置き換わるのは `repo` token だけで、しかも herdr の repo 名は totsuka の `[[repositories]].name` と一致する保証が無く、**真実の源が 2 つになる**

**再検討条件**: herdr が `worktree.open` に `env` を追加したとき（または `agent.start` が `env` を取り戻したとき）。

## D6 — サイドバーの `rows` はオペレータの設定。totsuka は書き換えない

`~/.config/herdr/config.toml` は herdr とオペレータのものであり、totsuka が触ってよいファイルではない（[click-to-focus セットアップ](/operations/click-to-focus-setup.md) と同じ扱い）。推奨スニペットを docs に置き、手で入れてもらう。

**その推奨スニペットが満たすべき制約は 3 つある**（いずれも最初のスニペットが破っていて、実機で出た）:

1. **entry のどこかに、常に非空のトークンを 1 つ以上置く。** `rows` はグローバルなので、
   オペレータが自分で開いた space と**手で起動した agent** にも同じ行構成が当たる。報告された
   トークンだけで組むと、そこで entry 全体が `state_icon` だけになる。常に非空と言えるのは
   spaces の `workspace`、agents の `workspace` / `agent` だけである
2. **agents 行の主語に `workspace` を使わない。** 1 つの space に別リポジトリの tab を足せるので、
   space 名を主語にすると別リポジトリで動いている agent が space の名前で名乗る。1 と併せると、
   `workspace` は agents では 2 行目に置いて「常に非空」役を兼ねさせる形になる。
   **spaces 側では逆に `workspace` が正しい主語**（D4 の議論はそちらの話）
3. **長さに上限のないトークンは行の最後。** 幅の足りないサイドバーでは、先に置くと後続が
   押し出される。`$repo` / `$mode` のような短く上限のあるものは先でよい

`branch` / `git_status` は **space 単位**のトークンで agents には存在せず、また 1 space が複数
リポジトリの tab を持つと意味を成さないので、推奨スニペットからは外してある。

## D7 — 設定フラグ `[identity] enabled` は 1 つだけ

`plugins/herdr.toml`（`deny_unknown_fields` なので宣言が要る）に `[identity] enabled = true`（既定）を置く。`false` で D1 の報告も D4 の rename も止まり、現行挙動と完全に一致する。

**2 つに分けない。** 「label は人間可読だが token が無い」中間状態は、サイドバーが正しく見えているのに `doctor` の所有判定が最新の根拠を失っている状態で、**どちらの症状も単体では気づけない**。ロールバックの単位は「identity を報告するかどうか」1 つでよい。

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
- **rename 後は所有判定が token 単独に依存する**（label が `totsuka ` で始まらなくなるため）。**herdr の再起動やセッション復元をまたいで `tokens` が残るかは未実測**で、消えるならその pane は `session/list` からも `doctor` の孤児検出からも消える。実機検収の項目に入れてある（[サイドバー設定手順](/operations/herdr-sidebar-setup.md)）。消えると分かったら「rename しない」か「identity を再報告する経路を足す」かをここで決め直す
- **`workspace.rename` の label 長上限は未実測。** 実装は metadata token の 80 文字を安全側の代理として流用している

# 関連

- [#416](https://github.com/tomoya-k31/totsuka/issues/416) / [ADR-0013](/decisions/adr-0013-orphan-pane-detection.md) — 所有判定の土台
- [ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) — protocol 17 の実機作法（`env` が workspace 経由でしか届かないこと）
- [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) — 0.x のバージョン規約（minor を上げると何が起きるか）
- [herdr Socket API](/references/herdr-socket-api.md) — 実測した API 形
