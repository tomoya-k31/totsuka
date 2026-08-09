---
type: Decision
title: ADR-0034 期限切れの非推奨 3 面をプロトコル 0.4.0 で削除する
description: "0.3.0 で削除すると宣言しながら残っていた TaskDispatchParams.hook / HookLaunchSpec、herdr の agent_command・plan_args・launch_command、design_preview（設定キーとケイパビリティ宣言）を、プロトコル 0.4.0 として実際に削除する決定。agent_ide の manifest 下限を >=0.2.3 へ上げてフォールバックを到達不能化してから消す。宣言だけ直す案・アプリ 0.3 とまとめる案は採らない。"
resource: https://github.com/tomoya-k31/totsuka/issues/411
tags: [decision, protocol, deprecation, breaking-change, herdr, capability, adr]
generated: { by: claude-code/opus-5, at: 2026-08-09T19:40:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-411
    resource: https://github.com/tomoya-k31/totsuka/issues/411
    title: "chore(protocol): protocol 0.3.0 で削除予定だった非推奨 3 面が残っている"
  - id: version-history
    resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/version.rs
    title: PROTOCOL_VERSION の履歴コメント（一次情報）
---

# Status

stable（[#411](https://github.com/tomoya-k31/totsuka/issues/411)）。

# Context

## 宣言と実態が 1 世代ずれていた

`PROTOCOL_VERSION` は既に **0.3.0** で、その破壊的バンプで `Task.thread_key` は実際に削除された。ところが**同じバンプで消すと宣言していた 3 面が取り残されていた**。

| 面 | 宣言 | 実態 |
|---|---|---|
| `TaskDispatchParams.hook` / `HookLaunchSpec` | 「0.2.3 で deprecated、次の breaking（0.3）で削除」 | 型も残り、core は 0.3 系の間ずっと送り続けていた |
| herdr の `agent_command` / `plan_args` / `launch_command()` | 「次の breaking protocol バンプで削除」 | 設定キーもフォールバック経路も残存 |
| herdr の `design_preview`（設定キー）＋ `Capabilities.design_preview`（宣言） | [ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)「削除は 0.3」 | 設定できるが誰も読まない状態が継続 |

**問題は残っていること自体より、宣言が嘘になっていたこと。** 次に読む人が「まだ消せない事情があるのか / 単に落とし忘れたのか」を判断できない。

なぜ落ちなかったかもはっきりしている。「**次の** breaking bump で削除」という書き方は、その bump が来たときに誰も参照しない。0.3.0 を切った作業は `Task.thread_key` の削除であって、「期限切れの非推奨を棚卸しする」作業ではなかった。

## バージョン軸は 2 本ある

**プロトコルは 0.3.0**（`crates/plugin-protocol/src/version.rs`）、**アプリは 0.2.0**（`workspace.package.version`）。ここで扱うのはプロトコル側だけである。アプリ 0.3 で落とす予定の `result/publish` / `trigger_reactions` は本 ADR のスコープ外。

# Decision

プロトコル 0.4.0 として、**3 面を実際に削除する**。

## D1. `TaskDispatchParams.hook` / `HookLaunchSpec` を削除する

`ToolLaunchSpec`（0.2.3, #196）が `--settings` も `--resume` も焼き込んだ argv と env を運ぶので、`hook` は同じ情報を「プラグインが解釈しなければならない形」で重複させていただけだった。

**core 側に穴は開かない。** `hook_spec` はコア内部でも使われていたが（`tool_launch` 組み立ての `settings_path`/`env`、および `extra_context` の経路判定）、必要なのは `(settings_path, env)` の 2 値だけなので、ワイヤ型を消して**内部のタプル**に落とした。core 専用の構造体を新設する必要すらなかった。

## D2. herdr の argv 自前組み立てを削除する

`agent_command` / `plan_args` / `launch_command()`、およびそれを呼ぶ `resolve_launch` のフォールバック分岐を削除する。

**削除の前に、到達不能にする。** herdr の manifest 下限を `>=0.1.0` → **`>=0.2.3`** へ上げる。0.2.3 は `tool_launch` が入ったバージョンなので、それを送らない Orchestrator は F-54 により**起動時点で拒否される**。フォールバックが存在意義を失ってから消すので、これは挙動変更ではない。この不変条件はコメントではなく `version.rs` のテストで固定した（`the_agent_ide_lower_bound_is_what_makes_the_fallback_unreachable`）。

**`tool_launch` 不在は `INVALID_PARAMS` で失敗させる。** 黙って `claude` を素で起動する誘惑があるが、それは `--settings` 無しの起動＝**フックが載らないペイン**を意味する。走るが完了を報告しないので、タイムアウトでエスカレーションするまで誰も気づかない。「起動できたように見えて完了しない」より「dispatch が落ちる」ほうが良い。

エラーは `HerdrError::InvalidResponse` ではなく専用の `MissingToolLaunch` にした。前者だと「herdr returned an unexpected response」と表示されるが、herdr は一切関与していない。

## D3. `design_preview` を設定キーとケイパビリティ宣言の両方から削除する

[#302](https://github.com/tomoya-k31/totsuka/issues/302) が扱った「宣言だけで誰も読まないケイパビリティ」と同じ形。core もプラグインも読んでいないので、宣言は**存在しない機能の約束**だった。

## D4. 束ねているプラグインの範囲

| プラグイン | 変更後 | 下限を上げたか |
|---|---|---|
| herdr（agent_ide） | `>=0.2.3, <0.5` | **上げた**（D2 の前提） |
| orca（agent_ide） | `>=0.1.0, <0.5` | 上げない |
| slack / github / notion（task_source） | `>=0.1.6, <0.5` | 上げない |
| notifier-macos | `>=0.1.0, <0.5` | 上げない |

**orca の下限を上げないのは意図的。** orca は `orca` CLI 自体を駆動しており、`tool_launch` を一度も読まない。D2 の理屈は「フォールバックを消したから」であって「0.4.0 だから」ではないので、それを orca に適用すると**問題なく動く Orchestrator を弾く**ことになる。同じ kind でも下限は同じにならない。

## D5. 削除したキーは serde の "unknown field" で終わらせない

`HerdrConfig` は `deny_unknown_fields` なので、フィールドを消すだけで既存の `herdr.toml` は落ちる。それ自体は正しい（黙って無視されるより良い）が、`unknown field 'agent_command', expected one of ...` は**いつ・なぜ消えたのか・何に置き換わったのか**を何も言わない。

削除したキーの墓標 `REMOVED_KEYS` を持ち、`initialize` と `config/validate` の両方で名指しして報告する:

```text
`agent_command` was removed in protocol 0.4.0 (#411): the Orchestrator resolves
the full argv itself since protocol 0.2.3 (#196); set `[tools]`/`default_tool`
in the orchestrator config instead. Delete the key.
```

3 つ設定していれば 3 つまとめて返す（1 往復で全部消せる）。

# 不採用案

## B. 宣言を現実に合わせるだけ（「次の breaking」→「0.4」と書き換える）

コード量は減らないが宣言と実態の食い違いは消える、という案。**採らない。** 0.3 を逃した原因がまさに「将来のバンプに預ける」書き方だったので、同じ書き方でもう一世代預けることになる。差分は数十行で済むが、買えるものが「次も同じ理由で落ちるかもしれない約束」しかない。

## C. アプリ 0.3 の非推奨群（`result/publish` / `trigger_reactions`）とまとめる

破壊的変更を 1 回にまとめれば移行も 1 回で済む、という案。理屈は正しいが、**まとめる相手が存在しない**: 確認時点でマイルストーンは未設定、`result/publish` 削除の issue も未起票だった。決定済みの掃除を、日程の決まっていないリリースに預ける形になる。

加えて C は「アプリ 0.3.0 = プロトコル 0.4.0」と決め打つことを含意するが、`version.rs` 冒頭が「プロトコルはアプリと**独立した** SemVer を持つ」と明記している。2 軸を独立に保つと決めておきながら、1 回目の判断で束ねるのは筋が悪い。

**ただし C の懸念自体は実在する**: `result/publish` を消すときプロトコルは 0.5.0 になり、利用者は 2 回連続で manifest 更新を強いられる。それを承知で受け入れる — 「近いうちに来るかもしれない 2 回目」のために、今できる掃除を止めない。

# Consequences

## 良くなること

- 宣言と実態が一致する。`version.rs` の履歴が「0.4.0 で何を消したか」を一次情報として持つ
- herdr プラグインから CLI フラグの知識が完全に消えた（`--settings` / `--resume` / `--permission-mode` の文字列がプラグイン側に無い）。[ADR-0014](/decisions/adr-0014-tool-abstraction.md) が目指した境界が、フォールバック込みで達成された
- 起動できないより悪い「起動できたが完了しない」状態を、dispatch 時の明示的な失敗に置き換えた

## 引き受けたコスト

- **既存の `plugins/herdr.toml` が壊れる。** 3 キーのいずれかを書いていると `initialize` が `CONFIG_INVALID` で落ちる。D5 のメッセージが緩和策で、消すこと自体は避けられない
- **古い manifest のプラグインは起動できない**（F-54 の設計どおり）。同梱プラグインは同時に更新されるが、別途インストールしたプラグインバイナリが残っていると拒否される
- **プロトコルの破壊的バンプがもう一度来る可能性が高い**（不採用案 C）

## 分かったこと

「次の breaking bump で削除」は**期限ではない**。日付でも、バージョン番号でもなく、「誰かが思い出したら」と書いているのと同じである。少なくとも具体的なバージョン番号（`0.4.0 で削除`）を書き、可能なら削除する issue を先に起票しておくべきだった。

# 検証

- `cargo test --workspace --all-features` — `version.rs` の境界テスト（`<0.4` 系 manifest が拒否される / agent_ide 下限が pre-0.2.3 を排除する）、`removed_keys_in` の墓標メッセージ、`tool_launch` 不在で dispatch が失敗すること、mock plugin が `tool_launch.env` からフック env を読むこと
- `cargo doc --workspace --no-deps` — 削除した型への intra-doc link が残っていないこと
