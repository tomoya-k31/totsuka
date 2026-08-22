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

`80 chars / 240 bytes` なら 0.7.5 と同じ。**片付けを忘れないこと** — 1 つの pane が
受け付ける異なる `source` は**生涯 32 個まで**で、clear しても枠は戻らない。

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

## C-4 / C-5 env の伝播

**この 2 つは 1 回の dispatch で同時に確かめられる。** どちらもフック環境が
どの pane に載るかの話なので、両方の pane を見る。

```bash
# dispatch 後、タスクの workspace の 2 つの pane それぞれで実行する
herdr pane send-keys <agent-pane> 'echo "AGENT: ${TOTSUKA_HOOK_TOKEN:+set}${TOTSUKA_HOOK_TOKEN:-unset}"'
herdr pane send-keys <shell-pane> 'echo "SHELL: ${TOTSUKA_HOOK_TOKEN:+set}${TOTSUKA_HOOK_TOKEN:-unset}"'
```

期待は **`AGENT: set` / `SHELL: unset`** の 2 行。

- `AGENT: unset` → **C-4 が破れている。** 完了検知が来なくなるので、
  そのまま実機検証を続けても全タスクがタイムアウトする
- `SHELL: set` → **C-5 が破れている。セキュリティ問題として扱う。**
  人間が叩くシェルにフックトークンが載っている状態で、タスクは正常に完了する

**値そのものを画面に出さないこと。** 上のコマンドが `set` / `unset` しか
出さないのはそのためで、`echo "$TOTSUKA_HOOK_TOKEN"` と書くと pane の履歴と
`pane.read` の両方にトークンが残る。

# いつ測り直すか

**herdr の版を上げるたび。** 上げた直後の実機検証（[live-e2e スキル](https://github.com/tomoya-k31/totsuka/tree/main/.claude/skills/live-e2e)）に
C-4 / C-5 を必須項目として入れてある。C-1 / C-2 は 1 コマンドで済むので同時に回す。
C-3 は独立に測れないため、dispatch のログから読む。

**下限版（0.7.5）で測り直す必要は無い。** それは現に本番で動いている版そのもので、
ここの記述はすべてその版の実測である。知りたいのは常に「新しい版で変わったか」だけ。
