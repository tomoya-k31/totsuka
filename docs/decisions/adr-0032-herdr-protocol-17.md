---
type: Decision
title: ADR-0032 herdr protocol 17 への追随（agent.start の manifest 駆動化と agent.prompt への移行）
description: herdr 0.7.5 (protocol 17) で agent.start が manifest 駆動（kind + 呼び出し側が用意した既存 pane）へ、プロンプト投入が agent.prompt へ破壊的に変わったことへの追随方針。program→kind 写像、agent name の生成規則と agent_name_taken の扱い、pane 確保順序の反転、submit_prompt と RetryPolicy の廃止、protocol 16 以下を切る判断を定める。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [adr, herdr, agent-ide, protocol, breaking-change]
generated: { by: claude-code/opus-5, at: 2026-08-03T13:30:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: probe-17
    resource: "herdr 0.7.5 (protocol 17) 実機プローブ記録（`herdr api schema --json` + CLI 経由の workspace.create → pane.split → agent.start → agent.prompt、および重複名・起動タイムアウトの再現、2026-08-03）"
    title: "protocol 17 プローブ（本 ADR の全事実の出典）"
  - id: mirror
    resource: /references/herdr-socket-api.md
    title: "herdr Socket API ミラー（2026-08-03 改訂で protocol 17 を反映済み）"
---

# Status

**採択（stable）。** [agent-ide-herdr](/components/agent-ide-herdr.md) に実装済み。

# Context

## 何が起きたか

実機動作テスト環境を組んで `totsuka run --watch` を回したところ、GitHub の Issue から作られたタスクの
`task/dispatch` が次で失敗した。[^probe-17]

```text
herdr error (invalid_request): invalid request: missing field `kind` at line 1 column 2113
```

手元の herdr は `brew install herdr` で入る最新の **0.7.5 (protocol 17)**。
[agent-ide-herdr](/components/agent-ide-herdr.md) が前提にしているのは 0.7.4 (protocol 16) までで、
**protocol 17 でエージェント起動とプロンプト投入の 2 系統が破壊的に変わっていた**。

差分の一次情報は [herdr Socket API ミラー](/references/herdr-socket-api.md) の 2026-08-03 改訂節にある。
本 ADR が依拠する事実だけを再掲する:

| # | protocol 16 まで | protocol 17 |
|---|---|---|
| 1 | `agent.start {name, argv, cwd?, workspace_id?, tab_id?, split?, env?, focus?}` | `agent.start {name, kind, pane_id, args?, timeout_ms?}`。**それ以外は受け付けない** |
| 2 | `agent.start` が pane を**作って返す**（`split` 未指定でも既定 `right` / 0.5 で分割） | **既存 pane に起動する**。pane の用意は呼び出し側 |
| 3 | 実行ファイルは `argv[0]` で指定 | **`kind`（21 値の enum）が実行ファイルを決める**。`args` はその後ろに付く |
| 4 | `name` は表示ラベル（pane label になる） | **識別子**。`[a-z][a-z0-9_-]{0,31}`、かつ**生存中のエージェント間で一意** |
| 5 | `agent.send` + `pane.send_keys ["enter"]` | **`agent.send` は廃止**。`agent.prompt {target, text, wait?}` |
| 6 | 複数行プロンプトは自前の自己修正手順が必須 | **`agent.prompt` がそのまま投入・送信する**（実機で 3 行プロンプトを確認） |

## なぜこれが設計判断になるのか

単なる API 追随なら実装だけで済む。判断が要るのは、**#3 と #4 が totsuka 側の既存の決定と衝突する**からである。

- **#3 は [ADR-0014](/decisions/adr-0014-tool-abstraction.md) の前提を削る。** ADR-0014（#196）は
  「CLI フラグの知識は Orchestrator の `[tools]` レジストリ側に置き、agent_ide プラグインは
  `ToolLaunchSpec` の `program` / `args` / `env` を**そのまま**起動する」と決めた。
  protocol 17 では実行ファイルを herdr が `kind` から決めるため、**プラグインは `program` をそのまま使えない**。
- **#4 は totsuka の task_id と両立しない。** 現行は `format!("totsuka {}", task.id)`。
  Slack の task_id は `{channel}:{ts}`（例 `C0BNAU8KKG8:1754...`）で、**空白・`:`・大文字の 3 つすべてに違反する**。
  実機でも `"totsuka probe"` は `invalid_agent_name` で拒否された。

## 実機で確認した追加の挙動

**同名の生存エージェントがあると `agent_name_taken` で拒否される。**[^probe-17]

```text
{"code": "agent_name_taken",
 "message": "agent name dup-name is already used; candidates: ... pane_id=w4:p2 ... status=Idle"}
```

決定論的な名前を採る以上、この応答は**必ず起きうる**ので、扱いを決めておく必要がある。

**`agent.start` は pane が対話シェルプロンプトに達する前に呼ぶと `timeout` する。**
`workspace.create` の直後（約 1 秒後）に `agent.start` した試行が
`{"code":"timeout","message":"timed out waiting for agent startup"}` で失敗した一方、
`workspace.create` から数秒おいた試行は成功した。**1 事例ずつの観測**なので断定はしないが、
実装は「起動直後の pane はまだ使えないことがある」前提で書く必要がある。

# Decision

## D-1: `ToolLaunchSpec.program` は basename で `kind` へ写像し、判定は herdr に委ねる

`program` の**ファイル名**をそのまま `kind` として送る。`args` はそのまま `agent.start` の
`args` に渡す。

```text
/Users/x/.local/bin/claude   → kind = "claude",     args = ToolLaunchSpec.args
codex                        → kind = "codex",      args = …
/opt/wrappers/my-claude      → kind = "my-claude"  → herdr が拒否（下の逃げ道を使う）
```

**プラグイン側で enum と照合はしない。** 21 値の enum を複製すると、上流が値を増やしたときに
黙って食い違う——**この ADR 自体が、herdr の形を写した記述が古びて起きた問題である**。
未知の `kind` は `agent.start` が herdr 自身の言葉で拒否し、dispatch はそこで失敗する。
「黙って既定の `claude` に落とす」ことをしない、という要件は満たされる。

**逃げ道は `plugins/herdr.toml` に置く。** ラッパースクリプト等のために
`[kind_map]` を設け、basename からの明示写像を許す:

```toml
[kind_map]
my-claude = "claude"
```

**`[tools]` レジストリ側には置かない。** `[tools]` は agent_ide 非依存の共有レジストリであり、
そこに herdr 固有の語彙を持ち込むと、orca しか使わない利用者の設定にも herdr の都合が漏れる。
写像は herdr の protocol の詳細なので、herdr プラグインの設定に閉じるのが正しい層である。

**これは ADR-0014 の破棄ではない。** ADR-0014 の核は「**CLI フラグの知識**を core に集める」ことで、
`args` が不透明なまま渡る点は変わらない。プラグインが新たに得るのは
「program の**同一性**を herdr の語彙へ翻訳する」責務だけで、これは herdr プロトコルの詳細そのものである。
ADR-0014 の「そのまま起動する」という記述だけが狭まる。

## D-2: `name` は `t-<可読プレフィクス>-<task_id の 8 桁ハッシュ>` にする

書式制約（`[a-z][a-z0-9_-]{0,31}`）と、**衝突しないこと**と、**再計算できること**を同時に満たす必要がある。

```text
name = "t-" + sanitize(task.id)[..N] + "-" + hex(sha256(task.id))[..8]
```

- `sanitize` = 小文字化 → `[a-z0-9]` 以外を `-` に → 連続する `-` を畳む → 前後の `-` を落とす
- 全体が 32 文字に収まるよう `N` を決める（`t-` 2 + `-` 1 + ハッシュ 8 = 11 なので `N = 21`）
- 例: `C0BNAU8KKG8:1754...` → `t-c0bnau8kkg8-1754-3f2a9c11`

**ハッシュを付ける理由**は、切り詰めが**別タスクとの取り違え**になるからである。`name` が
表示ラベルだった 16 までは重複しても表示が紛らわしいだけだったが、17 では識別子なので、
衝突したタスクは同じエージェントを指すことになる。
**可読プレフィクスを残す理由**は、`herdr agent list` を人間が読んだときに
どのタスクか当たりが付かないと、実機の切り分けができないからである。

ハッシュは `sha2`（既にワークスペース依存にあり、`plugin install` の SHA-256 検証で使っている）を使う。
プラグイン内に独自ハッシュを書き起こすより、既にある依存を使うほうが読み手の負担が小さい。

## D-3: `agent_name_taken` は「同一タスクの再 dispatch」とみなして失敗させる

`name` が決定論的である以上、この応答は**同じ task_id のエージェントがまだ生きている**ことを意味する。
起こりうるのは孤児 pane（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）が残っている場合か、
`session/release`（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）が失敗した場合で、
**どちらも「掃除されていない」という異常**である。

したがって **自動で別名にリネームして起動しない**。エラーメッセージが返す `pane_id` を含めて
`totsuka doctor` の孤児 pane 検出と `herdr pane` での解消を案内し、dispatch を失敗させる。
別名で起動すると、孤児が積み上がったまま成功し続け、**気づく機会が永久に来ない**。

## D-4: pane の確保順序を反転し、`pane.close` を廃止する

```text
16 まで: workspace.create → agent.start（新 pane を作る）→ pane.close(root_pane) → pane.split(agent pane)
17:      workspace.create → pane.split(root_pane)［layout.shell = true のときだけ］→ agent.start(root_pane)
```

- **フック環境変数の注入先は `workspace.create` の `env`** に移す。`agent.start` は `env` を取らなくなったが、
  エージェントは root pane のシェルから起動されるので、
  「`workspace.create` の `env` は root pane にしか効かない」（#356 で実測、0.7.5 でも再現）が
  ちょうど噛み合う。`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` は従来どおり届く
- **`pane.close` は不要になる。** 16 までは `agent.start` が新しい pane を作るせいで初期シェル pane が
  余っていたが、17 では初期 pane がそのままエージェント pane になる。実測で `pane.close` は
  23–25 ms と本 API 群で最も遅く、1 dispatch ぶんそれが消える
- **`[layout].ratio` の意味は変わらない。** `ratio` は分割元の取り分で、分割元は 16 でも 17 でも
  エージェント pane である。**既存の `plugins/herdr.toml` を書き換える必要はない**
- **`agent.start` は `agent_pane_busy` の間リトライする**（下記 D-7）

## D-5: `submit_prompt` の自己修正手順と `RetryPolicy` を廃止する

`agent.prompt {target, text, wait: {until: ["working", "blocked", "done"], timeout_ms}}` 1 回に置き換える。

`until` が 3 値なのは、`working` だけだと**極端に短いターンが settle してから herdr が状態を採り、
成功した投入をタイムアウトとして失敗させる**からである。`blocked` / `done` はどちらも
「プロンプトを読んだ」ことを意味するので、含めても偽陽性にならない。

`agent.prompt` は「非 working 状態からの投入では、まず 5000ms 以内の状態変化を要求し、
無ければ `agent_prompt_stalled` を返す」と CLI ヘルプに明記されており、
**16 まで自前で作っていた「着弾したか」「送信されたか」の判定を herdr 側が持っている**。

これにより次が消える:

- `agent.send` → 画面末尾の空白除去マッチ → 未着弾なら再送
- `agent_status` が動くまでの `enter` 再押下ループと `ENTER_SETTLE` のポーリング（#281）
- `RetryPolicy` の `send_attempts` / `enter_attempts`、および
  「`Default` が実機検証値と一致すること」を固定していた unit test（[ADR-0018](/decisions/adr-0018-ci-test-time.md)）

**契約は変えない。** 16 までも「どちらも確定できなければエラーで dispatch を失敗させる
（無言で永久ハングするセッションを作らない）」だったので、`agent_prompt_stalled` を
dispatch 失敗に写像すれば同じ意味になる。

## D-6: protocol 16 以下は切り、`initialize` で明示的に拒否する

`ping` が返す `protocol` を `initialize` で検査し、**17 未満なら `CONFIG_INVALID` 相当で初期化を拒否**する
（「herdr 0.7.5 以降が必要。`herdr update` を実行せよ」というガイダンス付き）。

**二重実装しない理由**は 3 つある。

1. **CI で検証できない。** herdr は CI に入っていない（§9・[テスト戦略](/quality/test-strategy.md)）ので、
   2 経路のうち片方は**誰も走らせないコード**になる。今回の非互換自体、
   実機で走らせるまで 1 度も検出されなかった
2. **dispatch は 2 経路の差が最も大きい場所である。** pane の所有者・env の注入先・
   プロンプト投入手段の 3 つが同時に違うので、共通化の余地がほとんどない
3. **上流が自動更新を前提にしている。** `herdr update` があり、`brew install herdr` は 0.7.5 を入れる。
   16 に留まる利用者を支える期間は短い

**これは利用者にとっては破壊的変更**なので、リリースノートに移行手順（`herdr update`）を書く。

## D-7: herdr の起動過渡状態は 2 段とも `agent.start` / `agent.prompt` のリトライで待つ

**`workspace.create` が返した root pane は、すぐには使えない。** シェルがまだ起動中で、
herdr は `agent_pane_busy: agent target pane w5:p1 is not an available shell` を返す。
実機の初回検証はこれで dispatch が落ちた（`workspace.create` の約 1 秒後に `agent.start`）。

対処は**リトライで待つ**ことにした。予測もポーリングも成立しないためである。

- **予測できない**: シェルがプロンプトへ達するまでの時間は運用者の rc ファイル次第で、
  バージョンマネージャや補完の読み込みが入れば延びる。`timeout_ms` を伸ばしても解決しない
  （`agent_pane_busy` は待たずに即時返るため）
- **ポーリングする先が無い**: `pane.process_info` は `shell_pid` を返しうる形をしているが、
  実測では 10 秒間ずっと `null` のままだった。`foreground_processes` も空で、
  「プロンプトに達したか」を読み取れる場所が無い
- **`pane.wait_for_output` は使えない**: 待つべき文字列はプロンプトのカスタマイズ次第で
  何にでもなる（`❯` を決め打ちすると別のプロンプトで必ず外れる）

`agent.start` **そのものが herdr の readiness 検査**なので、その判断を再実装せず
同じ問いを繰り返す。予算 60 秒・間隔 500ms。

### 過渡状態は 2 段ある

**`agent.start` の成功は「プロンプトを受けられる」を意味しない。** 成功応答が
`launch_pending: true` / `agent_status: unknown` を返すことがあり、その間 `agent.prompt` は
`agent_not_ready: agent … is not an active named agent` で拒否する。実測では
pane 待ちを終えた `agent.start` が t=1.0s で成功し、`agent.prompt` が通ったのは t=5.0s だった。

しかも **herdr の応答は非決定的**で、同じ状況が `agent_pane_busy`（起動前）にも
`launch_pending: true`（起動受理済み・未完了）にもなる。したがって片方だけ待っても足りず、
**`agent.start` の `agent_pane_busy` と `agent.prompt` の `agent_not_ready` の両方**を
同じ予算で待つ。

**リトライするのはこの 2 コードだけ。** 未知の `kind`・`agent_name_taken`・
CLI が現れない `timeout` は、いずれも放っておいても直らない。リトライは報告を遅らせるだけになる。
**`agent_not_found` も含めない** — これは pane が死んだ形で、resume 付き dispatch では
`SESSION_UNRESUMABLE` として上げる必要がある（#261）。リトライすると、その報告を
タイムアウトまで遅らせたうえで別のエラーに化けさせてしまう。

# Consequences

## 良くなること

- **実装が小さくなる。** D-4 で `pane.close` が 1 手減り、D-5 で `submit_prompt` の自己修正・
  `RetryPolicy`・その unit test が丸ごと消える。#124 と #281 で積み上げた回避策が、
  上流の API 追加によって不要になった
- **dispatch のレイテンシが下がる。** `pane.close`（23–25 ms）の削減に加え、
  #281 で「成功する dispatch が毎回 1.2 秒払っていた」`ENTER_SETTLE` が無くなる

## 悪くなること・注意点

- **herdr 0.7.4 以前が使えなくなる**（D-6）
- **任意パスの実行ファイルが `[kind_map]` 無しでは使えなくなる**（D-1）。
  `[tools].command` に絶対パスを書いている構成は影響を受ける
- **`agent_session`（`--resume` の識別子）は `herdr integration install claude` が前提**である。
  これは 17 の変更ではなく従来どおりだが、実機検証環境で統合が未インストールだったため
  `pane.get` に出なかった。**Slack の会話継続（`--resume`）を検証するには統合の導入が要る**

# 不採用案

| 案 | 不採用の理由 |
|---|---|
| `agent.start` を使わず、pane に起動コマンドをキー入力する | herdr の検出・準備完了保証（`interactive_ready`）と `agent.prompt` の target 解決を失う。`agent_status` も付かず、状態ストリームのデッドマンが機能しなくなる |
| `kind` を `plugins/herdr.toml` の固定値にする | 1 プラグインインスタンスで複数ツール（claude / codex）を使い分ける構成（`[[workflows]].tool`）が表現できなくなる。#196 で入れた解決順（workflow > repo > `default_tool`）が死ぬ |
| `name` を task_id の切り詰めだけで作る | 32 文字制限に対し GitHub の task_id は 24 文字、Slack は可変長で、切り詰めの衝突が**別タスクとの取り違え**になる（D-2） |
| `agent_name_taken` を別名で自動回避する | 孤児 pane が積み上がったまま成功し続け、異常に気づく機会が失われる（D-3） |
| protocol 16 / 17 の二重実装 | CI で検証できない経路が生まれる（D-6） |

# 未解決 / 実機で確認すること

1. **`session/attach`（回復経路）が 17 で従来どおり動くか。** `pane.get` による pane 生存確認は
   変わっていないはずだが、実機で未確認
2. ~~`herdr integration install claude` を導入したときの `agent_session` の出方~~ →
   **解決した。** 統合を明示インストールしなくても `agent_session` は報告される
   （実機の dispatch が `session_id = "wS:p1|aa37194a-…"` を返した）。
   最初に出なかったのは、`agent.start` 直後で報告が届く前に読んだためである

# 関連

- [herdr Socket API（一次情報ミラー）](/references/herdr-socket-api.md) — 本 ADR の全事実の出典
- [agent-ide-herdr](/components/agent-ide-herdr.md) — 実装。現在は 0.7.4 前提で、非互換の注記付き
- [ADR-0014 AI ツール抽象](/decisions/adr-0014-tool-abstraction.md) — D-1 が狭める前提
- [ADR-0030 herdr pane レイアウト](/decisions/adr-0030-herdr-pane-layout.md) — D-4 が変える手順
- [ADR-0018 CI テスト時間](/decisions/adr-0018-ci-test-time.md) — D-5 が消す `RetryPolicy`
- [ADR-0010 worktree 掃除と pane 解放](/decisions/adr-0010-worktree-cleanup-pane-release.md) / [ADR-0013 孤児 pane 検出](/decisions/adr-0013-orphan-pane-detection.md) — D-3 の受け皿

[^probe-17]: protocol 17 プローブ（本 ADR の全事実の出典）
