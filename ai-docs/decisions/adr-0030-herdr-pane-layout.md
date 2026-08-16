---
type: Decision
title: ADR-0030 dispatch の pane レイアウトは herdr.toml の [layout] 3 ノブで決める
description: "dispatch が pane 配置を一切指定せず herdr の既定（右分割 0.5）が漏れていた問題に対し、plugins/herdr.toml へ [layout]（shell / direction / ratio）を追加し、agent.start の後に初期シェル pane を close してから agent pane を split する決定。既定を down / 0.8 へ変え、失敗は警告して続行する。プリセット名・workflow 別スコープ・ratio の範囲検査は不採用。副次効果として人間が叩くシェルから TOTSUKA_HOOK_TOKEN が消える。"
resource: https://github.com/tomoya-k31/totsuka/issues/356
tags: [decision, agent-ide, herdr, layout, pane, config, security, adr]
generated: { by: claude-code/opus-5, at: 2026-08-01T20:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: herdr-schema
    resource: "herdr 0.7.4 (protocol 16) の `herdr api schema --json`"
    title: "herdr Socket API スキーマ（SplitDirection / PaneSplitParams / AgentStartParams / workspace_created）"
  - id: issue-356
    resource: https://github.com/tomoya-k31/totsuka/issues/356
    title: "#356 pane レイアウト（分割方向・比率・シェル有無）を設定可能にする — 実機プローブ記録つき"
---

# Status

stable。[#356](https://github.com/tomoya-k31/totsuka/issues/356) の実装とともに確定した。

[ADR-0005](/decisions/adr-0005-click-to-focus.md) / [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md) が確立した「pane を触る機能は既存の `pane_control` ケイパビリティに相乗りする」パターンを踏襲する。**実機検収は未了**（この ADR に `verified` は付けていない）。

# Context

[agent-ide-herdr](/components/agent-ide-herdr.md) の `dispatch` は `workspace.create` → `agent.start` を呼ぶだけで、**pane の配置を一度も指定していなかった**。結果として herdr 側の既定がそのまま漏れる:

| 実測 | 値 |
|---|---|
| 分割方向 / 比率 | `right` / 0.5（`agent.start` は `split` 未指定でも分割する） |
| エージェント pane の幅 | **123 桁**（同じ端末で上下分割なら 247 桁） |
| 残り半分 | 誰も使っていない、workspace の初期シェル pane |
| 稼働中の実タスク workspace | `pane_count = 2` で一致 |

これは**誰も選んでいない値**で、設計として決めた記録も無い。TUI エージェントにとって桁数は作業面積そのもので、123 桁は Claude Code が自分のクロームを折り返し始める領域に入る。

あわせて、**その初期シェル pane には `TOTSUKA_HOOK_TOKEN` が載っていた**（実測）。`dispatch` は `workspace.create` に `env` を渡しており（[#131](https://github.com/tomoya-k31/totsuka/issues/131) のフック起動）、それが root pane に効くため。[hook-security](/security/hook-security.md) が「トークンはプロセス env に閉じる」と書いている一方で、**人間が直接叩くシェルにベアラトークンが常駐している**状態だった。

## herdr 側の制約（0.7.4 / protocol 16、schema + 実機で確定）

設計の自由度はここで決まる。[herdr Socket API](/references/herdr-socket-api.md) には**いずれも未記載**だったので、本 ADR とともにミラーへ追記した。

| 事実 | 効き方 |
|---|---|
| `SplitDirection` は **`right` / `down` の 2 値のみ**（`up` / `left` は無い）[^herdr-schema] | 設定語彙もこの 2 値に固定できる |
| `AgentStartParams.split` は方向のみで **`ratio` を取らない**[^herdr-schema] | **起動時に比率は指定できない** |
| `agent.start` は `split` 未指定でも分割する（既定 `right` / 0.5） | 「分割しない」という選択肢が起動時に無い |
| `pane.split {direction(必須), ratio?, target_pane_id?, cwd?, env?, focus?}`[^herdr-schema] | **比率を指定できる唯一の経路** |
| `ratio` は**分割元（上 / 左）の取り分**（実機: area 67 行に `down` / 0.8 → 上 54 行 + 下 13 行） | 分割元＝エージェント pane にすれば、設定値がそのままエージェントの取り分になる |
| `pane.split` はフォーカスを**分割元に残す** | 追加の `pane.focus` が要らない |
| `workspace.create` の `env` は **root pane にしか効かない**。`pane.split` の pane は継承しない（実機: root で `MARK<SENTINEL>` / split pane で `MARK<>`） | `env` を渡さないだけでトークンが消える |
| `workspace.create` の応答は `root_pane`（`PaneInfo`）を**必須フィールドとして返す**[^herdr-schema] | 初期シェル pane を掴む唯一の手段。`pane.list` では判別できない（split pane の `label` は agent 側も shell 側も `null`） |

OS ウィンドウの座標・サイズ・ディスプレイ指定は**存在しない**。制御できるのは herdr の階層（workspace → tab → pane）内のタイル位置だけ。

## レイテンシ

生ソケット・1 リクエスト 1 接続で 2 回計測[^issue-356]:

| 呼び出し | 実測 |
|---|---|
| `ping`（接続込みのベースライン） | 0.04–0.13 ms |
| `workspace.create` | 4.3–5.2 ms |
| `agent.start` | 5.8–6.5 ms |
| **`pane.close`（追加分）** | **23.0–25.3 ms** |
| **`pane.split`（追加分）** | **6.4–6.7 ms** |

追加は約 **30 ms**。`dispatch` は `submit_prompt` のリトライ待ち（`enter_settle` 1200 ms 締切、`send_render_timeout` 3 s、CLI が `❯` を描くまで約 1200 ms）で元々**秒オーダー**なので、比率で 1–2% 未満。体感には出ない。

# Decision

## 設定 — `plugins/herdr.toml` の `[layout]`

```toml
[layout]
shell     = true      # 併設シェル pane を出すか
direction = "down"    # "down" = 上下 / "right" = 左右
ratio     = 0.8       # エージェント側の取り分
```

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `shell` | bool | `true` | 併設シェル pane を出すか。`false` ならエージェント全画面 |
| `direction` | `"down"` \| `"right"` | `"down"` | 分割方向。herdr の `SplitDirection` そのまま |
| `ratio` | float | `0.8` | **エージェント側**の取り分 |

- 置き場所は `plugins/herdr.toml` のみ（グローバル 1 箱）
- `shell = false` のとき `direction` / `ratio` は無視される
- **`ratio` の範囲検査はしない**。float としてパースできればそのまま herdr へ送る
- **`direction` は型で検査する**（下記「2 つの検証方針が非対称な理由」）
- ケイパビリティは新設せず、既存 `pane_control` に相乗りする

## dispatch の手順

```text
1. workspace.create {cwd, label, env}                      （現状のまま）
2. agent.start      {workspace_id, focus: false, ...}      （現状のまま。split は渡さない）
3. pane.close       <root_pane.pane_id>                     ← 新規
4. pane.split       {target_pane_id: <agent pane>,          ← 新規
                     direction, ratio,
                     cwd: <worktree>, focus: false}
                     ※ env は渡さない
```

`shell = false` のときは 4 を省略する（3 は実行し、エージェント全画面になる）。

**3〜4 は `submit_prompt` の前**に置く。split はエージェント pane をリサイズし、`submit_prompt` はその pane の画面を読んでプロンプトの着弾を確認する（`prompt_marker`）。投入の途中で折り返しが変わると、起動レースを守っているまさにその照合が無効化される。

**順序が close → split である理由**: split が失敗した中間状態が「エージェント全画面」＝ `shell = false` と同じ**正当なレイアウト**に落ちる。逆順にすると、エージェント・新シェル・閉じ損ねた初期シェルの 3 枚が残りうる。

**フォーカス**: `pane.close` で残った唯一の pane（エージェント）にフォーカスが移り、`pane.split` は分割元にフォーカスを残すため、エージェントがフォーカスされたままになる。追加の `pane.focus` は呼ばない。

**失敗時は `tracing::warn!` を出して続行し、dispatch は成功させる**。レイアウトは装飾であり、その失敗でタスクを落とさない。ただし **`pane.close` が失敗した場合は split も省略する** — 初期シェルが残っている上にもう 1 枚足すと 3 枚になるので、herdr の既定配置（＝ #356 以前の全タスクの姿）のまま放置する方が筋が通る。`root_pane` が応答に無い場合（古い/将来の herdr）も同じ扱い。

## セキュリティ上の副次効果

`pane.split` に `env` を渡さず、env を持つ初期シェル pane を close するため、**人間が直接叩くシェルから `TOTSUKA_HOOK_TOKEN` が消える**。[hook-security](/security/hook-security.md) の意図に現状より近づく。エージェント pane には従来どおり載る（完了検知の幹線なので当然）。

## 既定値を変える

`shell = true` / `direction = "down"` / `ratio = 0.8` を新しい既定にする。**既存ユーザの画面は変わる**（視覚のみ。データ・フロー・完了検知に影響なし）。リリースノートに明記する。

# Consequences

## 得るもの

- エージェントの作業面積が 123 桁 → 端末幅いっぱい（実測 247 桁）になる
- 人間が叩くシェルからベアラトークンが消える
- 「誰も選んでいない herdr の既定の漏れ」が、選んだ値になる
- レイアウトの好みが設定で表現でき、`shell = false` で全画面も選べる

## 代償・制約

1. **下 20% は実測 13 行**。シェルプロンプトが 2 行を使うので実働 10 行前後。ターミナルを小さくすると実用に足りなくなる。
2. **`ratio` 不正値の herdr 側挙動は未検証**。範囲検査をしない以上、herdr がエラーを返せば「警告して続行」に落ちてシェルなしになる。herdr が黙って受理して 0 行 pane を作る場合の挙動は確認できていない。
3. **`session/list` の label フィルタはエージェント pane しか拾わない**。`totsuka ` 前置の label は `agent.start` の `name` 由来で、split で作るシェル pane は `label = null`。`workspace.close` で巻き添えに閉じるので実害はなく、むしろ孤児検出（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）の対象が 1 タスク 1 件に保たれる。
4. **既存の結合テストが誤検知しうる**。`dispatch` が `pane.close` を呼ぶようになったため、cancel / release 系の `sent("pane.close")` は dispatch 由来の close でも通ってしまう。**pane_id で絞る**アサーションへ直した。
5. **`workspace.create` の `env` が実質デッドになる**。root pane にしか効かず、その pane を close するため。削除できる可能性が高いが、完了検知の幹線に触れるので**別 issue** とする。
6. dispatch が 1 タスクあたり herdr 呼び出しを 2 本増やす（約 30 ms）。上記のとおり比率としては無視できる。

## 2 つの検証方針が非対称な理由

`ratio` は検査せず、`direction` は型で検査する。矛盾ではなく、**loud に落とせるかどうか**の違い:

- `direction` は閉じた 2 値。typo は `initialize` の時点で `unknown variant 'up'` として落とせる。設定を読み込んだその場で人間に届く。
- `ratio` は連続値で、「妥当」の意味は herdr が持っている。ここで clamp すると**オペレータが書いていない配置を黙って描く**ことになり、拒否すると herdr が受け入れる値まで巻き添えにする。

`direction` を文字列のまま通すと、失敗は dispatch 時の warning になる。それは**誰も見ていない場所で、シェル pane が黙って消えるだけ**の失敗になる。ケイパビリティを宣言しただけでは縮退しない（[ADR-0014](/decisions/adr-0014-tool-abstraction.md) の `prompt_verification`）のと同じで、「約束より検査が弱い」形を作らないための選択。

# Alternatives considered

| 論点 | 決定 | 採らなかった案と理由 |
|---|---|---|
| 設定の形 | `[layout]` に 3 ノブを直接露出 | **プリセット名 1 本**（`layout = "agent_over_shell"`）→ 比率を変えたいときに必ず行き詰まる。**プリセット + 比率上書き** → `agent_only` に `ratio` を書いたときの整合を実装で捌く必要がある。**herdr の `LayoutNode` 直書き** → herdr のスキーマがそのまま totsuka の設定 API になり、herdr 側の変更に直接引きずられる |
| 既定値 | `shell = true` / `direction = "down"` / `ratio = 0.8` | **現状維持（`right` / 0.5）** → 誰も選んでいない「herdr の既定の漏れ」を仕様として固定することになる。壊れるのは見た目だけで、データもフローも影響しない。pre-1.0（0.2.x）の今が直し時 |
| スコープ | `plugins/herdr.toml` グローバル 1 箱 | **workflow 別** → `TaskDispatchParams` へのフィールド追加＝プロトコル 0.3 の breaking bump が必要で、「描画の好み」をプロトコルに持ち込むことになる。**mode 別**（`[layout.plan]`）→ plan と implement で画面を変えたい理由が現時点で無い。後方互換で後から足せるので、狭く始める |
| `ratio` の範囲検査 | しない | **initialize で拒否** / **clamp して警告** → herdr がレイアウトの意味論を持つ層なので、検証も herdr に委ねる。プラグインを薄く保つ |
| 失敗時 | 警告して続行 | **dispatch を失敗させる** → herdr の一時的な blip でタスクが落ちる。**初回だけ失敗** → 振る舞いが dispatch 回数に依存し説明できない |
| シェル pane の env | 渡さない | **エージェントと同じ env を渡す** → シェルから手動でフックを叩けてデバッグには便利だが、人間が叩くシェルに認証トークンを常駐させることになる |
| 初期シェル pane の掴み方 | `workspace.create` 応答の `root_pane` | **`pane.list` から絞る** → split pane の `label` はエージェント側も シェル側も `null` で、workspace 内の 2 枚を区別できない。`root_pane` は schema 上 required |
| `design_preview` | deprecated の印を付ける（削除は 0.3） | **触らない** → `side_pane` が 2 つの意味で存在する期間が伸びる。**`[layout]` に統合して実装** → 「設計プレビューとは何をどこに出すことか」の仕様策定が要り、今回の範囲を大きく超える |

## `design_preview` の deprecated 化について

`design_preview = "side_pane"` は設定でき、ケイパビリティも `true` で宣言しているが、**core もプラグインも一切読んでいない**（参照はマニフェスト宣言・デフォルト値・テストのみ）。[ADR-0014](/decisions/adr-0014-tool-abstraction.md) が `ToolCapabilities.prompt_verification` について「ケイパビリティは宣言しただけでは縮退しない」と実害の前例を明記しているのと同じ形。

`[layout]` が入ると `side_pane` という語が二重化し、「`design_preview = "side_pane"` にすれば横に出る」と誤読されるため、設定キー・ドキュメントに deprecated と明記する。削除は `agent_command` / `plan_args` と同じく次の breaking bump（0.3）。ケイパビリティ宣言自体は 0.3 まで残す（プロトコルの `Capabilities` は additive に扱うため、宣言を今落とすと旧 Orchestrator との組合せで意味が変わりうる）。

> **後日談（#411）**: ここで「削除は 0.3」と書いたが、0.3.0 の破壊的バンプは `Task.thread_key` しか落とさず、`design_preview` は設定キー・ケイパビリティ宣言ともに 0.3 系を丸ごと生き延びた。実際に消えたのは **プロトコル 0.4.0**（[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。「次の breaking bump で消す」という書き方が、その bump が来たときに誰も参照しないので実行されない、という教訓つき。

# 検証

- `cargo test -p agent-ide-herdr` — 既定 / `shell = false` / `direction = "right"` / split 失敗時の続行 / close 失敗時の split 省略 / `root_pane` 無し / **`env` が split に渡らないこと**を固定した
- **未了（実機検収）**: 実 herdr で 3 パターンを dispatch し `pane.layout` の `rect` が設定どおりの比率になること、シェル pane で `echo "MARK<${TOTSUKA_HOOK_TOKEN}>"` が `MARK<>` を返すこと、既定レイアウトで実 Claude Code の TUI が 80% 側で破綻しないこと

[^herdr-schema]: herdr 0.7.4 の `herdr api schema --json`
[^issue-356]: #356 の実機プローブ記録
