---
type: Reference
title: herdr の暗黙契約（schema に載らない依存）
description: "totsuka が herdr の Socket API schema に載っていない振る舞いへ依存している箇所の一覧と、その確かめ方。metadata token 値の 80 文字上限（超過は黙って切られる）、pane id の w1:p1 形式、herdr 内部の 5 秒下限、workspace.create の env が root pane に適用されること、pane.split が env を継承しないこと（セキュリティ前提）を扱う。型化も CI の schema 差分もこの層を一切カバーしない。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/agent-ide-herdr
tags: [herdr, socket-api, implicit-contract, security, live-e2e, external]
generated: { by: claude-code/opus-5, at: 2026-08-23T00:00:00Z }
verified:
  - { by: claude-code/opus-5, at: 2026-08-23T00:00:00Z }
status: stable
stale_after: 2027-02-01
owner: tomoya-k31
sources:
  - id: adr-0055
    resource: /decisions/adr-0055-herdr-schema-typed-wire.md
    title: "ADR-0055 herdr Socket API を下限版の schema から生成した型で受け、互換を CI で機械検査する"
  - id: herdr-socket-api
    resource: /references/herdr-socket-api.md
    title: "herdr Socket API（外部一次情報ミラー）"
  - id: hook-security
    resource: /security/hook-security.md
    title: "フック認証のセキュリティ"
  - id: agent-ide-herdr
    resource: /components/agent-ide-herdr.md
    title: "agent-ide-herdr プラグイン"
---

# なぜこの文書があるのか

[ADR-0055](/decisions/adr-0055-herdr-schema-typed-wire.md) で、herdr のレスポンスは
生成した型で受け、互換は CI の schema 差分が機械検査するようになった。
**その仕組みはここに挙げる層を 1 つもカバーしない。**

理由は 3 つある。

| なぜ捕まらないか | 例 |
|---|---|
| **schema に無い** ものは差分に出ない | 80 文字上限、5 秒下限、env の伝播規則 |
| **schema にあっても意味が現れない** | pane id は `string`。`:` 区切りという構造は herdr のドキュメントと実測にしかない |
| **黙って切り詰める**挙動は、変わっても観測できない | `…` が付く位置がズレるだけ |

つまりこの層が壊れたときに教えてくれるものは、**実機で動かすこと以外に無い**。
だから一覧と「確かめ方」をここに置く。

# 一覧

| # | 暗黙契約 | 依存箇所 | 壊れたときの現れ方 |
|---|---|---|---|
| C-1 | metadata token 値は **80 文字**。herdr は超過を**拒否せず黙って切る** | `agent.rs` `TOKEN_VALUE_CHARS` | 表示が短くなるだけ。**識別には影響しない**（下記） |
| C-2 | pane id は `w1:p1` 形式で、`:` の前が workspace id | `agent.rs` `workspace_of` | cancel / release が workspace を閉じ**られなくなる**（空の workspace が残る） |
| C-3 | herdr 内部の **5 秒下限**（設定不能）を前提にした stall 回復 | `agent.rs` の `agent_prompt_stalled` 処理 | 成功した投入が失敗として返り、Enter が余計に押される |
| C-4 | `workspace.create` の `env` が **root pane に適用される** | `agent.rs` `dispatch` | フック環境が届かず、**完了検知が来ない**（タスクがタイムアウトするまで気づかない） |
| C-5 | **`pane.split` で作る shell pane は env を継承しない** | `agent.rs` `apply_layout` | **壊れても動き続けたまま秘密が漏れる**（下記） |
| C-6 | `agent.start` の `name` は `[a-z][a-z0-9_-]{0,31}` | `agent.rs` `agent_name` / `NAME_PREFIX_CHARS` | `invalid_agent_name` で dispatch が落ちる（大声で壊れる） |

**C-6 は schema 上は素の `string`** で、実際の制約は herdr のドキュメントと
`invalid_agent_name` エラーにしかない。C-1 と同型（schema にあっても意味が
現れない）だが、**破れたら大声で壊れる**ので確かめ方は要らない — dispatch が
そのエラーで落ちる。ここに載せてあるのは、`agent_name` の生成規則を触るときに
「なぜ `t-` を前置しているのか」を辿れるようにするためである。

**C-5 だけ性質が違う。** 他は「壊れたら動かなくなる」ので、遅かれ早かれ気づく。
C-5 は壊れても**何も起きない** — `TOTSUKA_HOOK_TOKEN` を持ったシェルが人間の隣に
常駐するだけで、タスクは今までどおり完了する。だから**必須の検収項目**にしている
（[hook-security](/security/hook-security.md)）。

# 確かめ方

## C-1 metadata token の 80 文字上限

`"あ"×100` を送って返りを数える。**バイトではなく文字**であることまで見る。

```bash
sock=~/.config/herdr/herdr.sock
pane=$(printf '{"id":"1","method":"pane.list","params":{}}\n' | nc -U "$sock" \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["panes"][0]["pane_id"])')
long=$(python3 -c 'print("あ"*100)')
printf '{"id":"2","method":"pane.report_metadata","params":{"pane_id":"%s","source":"probe","tokens":{"probe":"%s"}}}\n' \
  "$pane" "$long" | nc -U "$sock" >/dev/null
printf '{"id":"3","method":"pane.get","params":{"pane_id":"%s"}}\n' "$pane" | nc -U "$sock" \
  | python3 -c 'import sys,json;v=json.load(sys.stdin)["result"]["pane"]["tokens"]["probe"];print(len(v),"chars /",len(v.encode()),"bytes")'
```

`80 chars / 240 bytes` なら 0.7.5 と同じ。片付けは:

```bash
herdr pane report-metadata <pane-id> --source probe --clear-token probe
```

**枠は戻らない。** 1 つの pane が受け付ける異なる `source` は**生涯 32 個まで**で、
clear しても expiry でも戻らない。上の例が `pane.list` の先頭を掴むのは楽だが、
それは**本番の pane かもしれない** — 掴んだ pane の 32 枠を 1 つ恒久的に使う。
気にするなら使い捨ての workspace を 1 つ作ってそこで測る。

**totsuka はこの上限に依存していない。** 識別に使う `totsuka_task` は、上限を超える
なら**切らずに省く**（切れた機械識別子は無い識別子より悪く、ラベル経路が正しい
フォールバックになる）。したがって上限が変わっても壊れない — **測るのは、上限が
「拒否」に変わっていないかを見るため**である。拒否に変われば `report_metadata` が
エラーを返し、identity 報告が丸ごと落ちる。

## C-2 pane id の `w1:p1` 形式

`pane.list` の `pane_id` と `workspace_id` を突き合わせるだけでよい。

```bash
printf '{"id":"1","method":"pane.list","params":{}}\n' | nc -U ~/.config/herdr/herdr.sock \
  | python3 -c 'import sys,json
for p in json.load(sys.stdin)["result"]["panes"]:
    ok = p["pane_id"].split(":")[0] == p["workspace_id"]
    print(("OK " if ok else "NG "), p["pane_id"], p["workspace_id"])'
```

1 行でも `NG` が出たら C-2 は破れている。

## C-3 herdr の 5 秒下限

**直接は測れない**（設定として露出していない）。観測できるのは
`agent.prompt` が `agent_prompt_stalled` を返すまでの時間で、実機 dispatch の
ログから読む。**5 秒より短い値で返り始めたら**、totsuka の stall 回復が
「まだ反応していないだけ」を「止まった」と誤認するようになる。

## C-5 `pane.split` の shell pane が env を継承しないこと

**シェル pane はシェルなので、そこでコマンドを 1 本走らせれば見られる。**
dispatch 後、タスクの workspace の**シェル側**（`w…:p2`）で:

```bash
herdr pane run <shell-pane> 'printenv TOTSUKA_HOOK_TOKEN >/dev/null 2>&1 && echo TOKEN=set || echo TOKEN=unset'
```

期待は **`TOKEN=unset`**。`TOKEN=set` なら **C-5 が破れている。セキュリティ問題
として扱い、そこで以降を止める** — 人間が叩くシェルにフックトークンが載った
状態で、タスクは正常に完了してしまう。

**値そのものを絶対に画面へ出さないこと。** 上の形が安全なのは `printenv` の
出力を捨てて終了コードだけを見ているからで、`echo "$TOTSUKA_HOOK_TOKEN"` は
もちろん、**`echo "${TOTSUKA_HOOK_TOKEN:+set}${TOTSUKA_HOOK_TOKEN:-unset}"` も
危険である**（`:-` は変数が設定されているとき**値そのもの**へ展開するので、
`setSECRET…` と表示される）。実測で両方の分岐が値を出さないことを確認済み。

一度出してしまうと、pane の履歴と `pane.read` の両方に残る — S0 をエージェントに
回させている場合は、そのエージェントの transcript にも載る。

**引数は 1 つにまとめる。** `herdr pane run <PANE_ID> <COMMAND>...` は argv を
空白で繋いで pane のシェルへ**打ち込む**ので、複数語に分けるとこちらのシェルが
先にクォートを剥がして壊れる（実測）。

## C-4 `workspace.create` の env が root pane に適用されること

**シェルで確かめる方法は無い。** root pane はエージェントの pane で、そこで
動いているのは Claude Code の TUI であってシェルではない（totsuka 自身も
`agent.prompt` 以外では触らない）。`herdr pane run` を打てば、コマンド文字列が
そのままプロンプトとしてエージェントに渡るだけである。

**観測点は完了検知そのもの。** env が root pane に届かなければ Stop フックが
Orchestrator の UDS を叩けず、**タスクは完了報告を出さない**。つまり
**フック完了する dispatch が 1 本通れば C-4 は成立している** — S1 が毎回それを
確かめているので、C-4 に独立した手順は要らない。

逆に、S1 の dispatch が「エージェントは答え終わっているのにタスクが
`running` のままタイムアウトする」形で落ちたときは、**まず C-4 を疑う**。

# いつ測り直すか

**herdr の版を上げるたび。** 上げた直後の実機検証（[live-e2e スキル](https://github.com/tomoya-k31/totsuka/tree/main/.claude/skills/live-e2e)）に
C-4 / C-5 を必須項目として入れてある。C-1 / C-2 は 1 コマンドで済むので同時に回す。
C-3 は独立に測れないため、dispatch のログから読む。

**下限版（0.7.5）で測り直す必要は無い。** それは現に本番で動いている版そのもの
だからで、知りたいのは常に「新しい版で変わったか」だけである。

## この文書の `verified` が掛かる範囲

| 契約 | 0.7.5 での裏取り |
|---|---|
| C-1 / C-2 | **このページのコマンドをそのまま実機で実行して確認**（`80 chars / 240 bytes`、`pane_id` の接頭辞と `workspace_id` の一致） |
| C-4 / C-5 | 実機の env 挙動は [herdr Socket API](/references/herdr-socket-api.md) の sentinel 実測が根拠。**このページの手順そのものは S0 で初めて通る** |
| C-3 | **測っていない。** 独立に観測する手段が無いので、dispatch のログから読むしかない |
| C-6 | エラーコード（`invalid_agent_name`）で観測済み。専用の手順は無い |

`verified` は**この表の範囲**に掛かる。文書全体が実機で通ったという意味ではない。
