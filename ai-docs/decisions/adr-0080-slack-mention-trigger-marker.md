---
type: Decision
title: ADR-0080 Slack のメンショントリガは trigger = { mention = true } で宣言する
description: "メンションの行き先を「trigger に reaction が無い workflow」という不在で決めていたのをやめ、trigger = { mention = true } の宣言で決める破壊的変更の記録。宛先 ID を trigger に列挙する案（to_user / to_group）を、挙動同一が前提なら二重管理とドリフトしか増やさないとして退けた経緯。空 trigger を移行期間なしで CONFIG_INVALID にした理由、mention を bool として素直に読む（false は真の表明として受理する）決定、その帰結として検査基準が「テーブルが空か」ではなく「種別キーを 1 つでも名指すか」になること、VALUED_TRIGGER_KIND_KEYS に mention を入れない理由を記録する。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/server.rs
tags: [decision, adr, slack, trigger, mention, config, breaking-change]
generated: { by: claude-code/opus-5, at: 2026-09-19T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0025
    resource: /decisions/adr-0025-reaction-task-trigger.md
    title: "リアクションをタスクのトリガにした決定 — ADR-0025"
  - id: adr-0068
    resource: /decisions/adr-0068-channel-watch-trigger.md
    title: "チャンネル監視トリガの決定 — ADR-0068"
  - id: adr-0034
    resource: /decisions/adr-0034-protocol-0-4-0-removals.md
    title: "期限付き非推奨が誰にも参照されずに残った記録 — ADR-0034"
  - id: schema-trigger-default
    resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/config/schema.rs
    title: "WorkflowConfig::trigger が #[serde(default)] である一次情報"
---

# Status

**採択（stable）。** 破壊的変更であり、移行期間は置かない。既存の `trigger = {}` な Slack ワークフローは `initialize` で `CONFIG_INVALID` になる。

# Context

## メンションの行き先は「不在」で決まっていた

Slack ソースには 3 つのトリガ種別がある —— リアクション（[ADR-0025](/decisions/adr-0025-reaction-task-trigger.md)）、チャンネル監視（[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)）、そしてメンション。前の 2 つは `trigger` にキーを書いて宣言するが、**メンションだけは「`reaction` キーが無いこと」で選ばれていた**（`reaction.rs` の `mention_candidates`）。設定上は `trigger = {}` と書く。

この形には、読みにくさとは別に**機械的な壊れ方**がある。

| 書き手の意図 | 実際の `trigger` | 起きること |
|---|---|---|
| メンションに答えたい | `{}` | 意図どおり |
| `trigger` を書き忘れた | `{}`（`WorkflowConfig::trigger` は `#[serde(default)]`） | **黙ってメンションの行き先になる** |
| `reaction` を書いたつもりが綴りを間違えた | `{ reation = "eyes" }` | 未知キー検査（#574）が拾う |

3 行目を #574 が塞いだのは、塞がなければ**キーが 1 つ消えて catch-all に化ける**からだった。つまり「不在で決まる」という設計のせいで、**無関係な検査がその設計を支える**必要が生じていた。2 行目は今も無言のままである。

## `to_user` / `to_group` を trigger に列挙する案を検討した

最初の案は、宛先を trigger に書き出すものだった:

```toml
trigger = { to_user = ["U…"], to_group = ["S…", "S…"] }
```

これは**採らない**。目的が可読性（挙動は現状と同一）である以上、これらの ID は情報を 1 ビットも増やさないからである。宛先は既に 2 箇所で決まっている —— `[slack] target_user_id` と、起動時に `usergroups.list` が解決する操作者の所属ユーザーグループ。trigger に書き写せば:

- **二重管理になる。** 同じ事実が 2 箇所にあり、食い違ったときにどちらが真かを新たに決めねばならない
- **ドリフトする。** ユーザーグループの所属は Slack 側で変わる。新しいグループに入った瞬間、設定は黙って古くなる
- 一致を起動時に照合して守ることはできるが、その場合の代償は「グループに入るたびに totsuka が起動しなくなる」である

可読性のために、存在しなかった故障モードを買うことになる。

# Decision

## 1. メンションは `trigger = { mention = true }` で宣言する

宛先 ID は持たせない。誰宛が「自分宛」かは従来どおり `[slack] target_user_id` と `usergroups.list` が決める。**キーは種別を名乗るだけで、値を運ばない。**

`reaction` / `channel` が「一致させる値」を運ぶのに対し `mention` が bare `true` になるのは、この非対称のためである。

## 2. 起動条件を 1 つも名指さない trigger は `CONFIG_INVALID`

検査の基準は「テーブルが空か」ではなく **「種別キー（`mention = true` / `reaction` / `channel`）を 1 つでも名指すか」**。これにより次がすべて同じ 1 つのエラーになる:

- `trigger = {}`
- `trigger` の書き忘れ（`#[serde(default)]` により `{}` と区別できない）
- `trigger = { mention = false }` だけ

**移行期間は置かない。** [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) が記録したとおり、「次の破壊的変更で消す」という期限は、その破壊的変更が来たときに誰も参照しない。利用者が実質的に単独である現状で、移行期間が守る相手はいない。エラー文が直し方を名指す（`REMOVED_KEYS` と同じ作法）。

## 3. `mention` は bool として素直に読む

`mention = false` は「この workflow はメンションに答えない」という**真の表明**であり、`reaction` の横に書いてよい。拒むと、選んだ「bool として読む」規則そのものが崩れる。

非 bool（`mention = "me"` 等）は hard error。`false` として読めば「メンションに答える」と書いた設定が 1 つもメンションに答えない状態になり、しかも二次被害として「種別キーが無い」という**書き手が書いたキーについての別のエラー**が出る。非文字列 `reaction` を hard error にしているのと同じ判断である。

## 4. 検査は Slack プラグインに置く（SDK へ引き上げない）

`plugin_sdk::unknown_trigger_keys` は 4 ソースが同じ失敗を持っていたから SDK にある（#574）。今回の「種別 / 修飾」の区別を必要とするのは Slack だけで、github の `label` や notion の `filter` に種別概念が要るかは未定である。**2 つ目のソースが要求したときに引き上げる。**

`workflow_infos`（`crates/orchestrator-core/src/plugins/spec.rs`）は解決済みソースで workflow を絞るので、この拒否が github / notion の workflow に届くことは構造的に無い。**`trigger = {}` が catch-all であることは SDK の契約として残り、Slack だけがその上に自分の条件を足す。**

## 5. `VALUED_TRIGGER_KIND_KEYS` に `mention` を入れない

種別キーの定数は `["reaction", "channel"]` であり、`mention` は入らない。**`mention` の種別性は存在ではなく値だから**である。`trigger.get("mention").is_some()` で種別を数えると `mention = false` が種別を名乗ることになり、決定 2 が捕まえるべき形（メンションワークフローを止めようとして書かれる、まさにその形）を素通りさせる。

実装中にこの取り違えを一度踏み、`mention_false_alone_names_no_kind` が捕まえた。定数から `mention` を外したのは、同じ取り違えが `is_some()` を書いた瞬間に再発しないようにするためである。

## 6. `initialize` と `config/validate` は同じ検査を通る

新しい検査を `initialize` にだけ入れるのは**この決定の中で最も高くつく間違い**だった（レビューで指摘され、マージ前に直した）。理由は移行期間を置かないこと（決定 2）と直接噛み合う: 破壊的変更を受け取った操作者が最初に叩くのは `totsuka config validate` であり、そこが「問題なし」と答えてから `totsuka run` が落ちるなら、エラー文をどれだけ丁寧に書いても届くのが遅すぎる。

`ConfigValidateParams.workflows` は「`initialize` が受け取るのと同じリスト」と protocol が明記しており、トリガ検査はそのリストの純粋関数なので、`config/validate` のオフライン性は損なわれない。両者は `resolve_trigger_shape` を共有し、**片方にだけ検査を足すことが構造的にできない**ようにした。

# Consequences

- **既存の設定は 1 行直す必要がある。** `trigger = {}` → `trigger = { mention = true }`。リポジトリ内のテンプレート・E2E アセット・ドキュメントは同じ PR で更新済み
- **`trigger` の書き忘れが無言でなくなった。** 決定 2 の副次的効果で、これは当初の動機に無かった
- **#574 の未知キー検査が背負っていた重荷が減る。** 綴り間違いはもう catch-all を作らない。検査自体は残る（条件が消えることに変わりはない）が、その失敗はもう「別のワークフローに化ける」ではない
- **メンションワークフローが 2 つあるときのエラー文が具体的になった。** 「プレーンメンションで起動する workflow が複数ある」ではなく「どちらも `mention = true` と書いている」と言える
- **`config/validate` が workflow のトリガを見るようになった。** 従来は `[slack]` テーブルと gateway の状態しか見ておらず、`initialize` だけが持っていた検査がある状態だった。本 PR の破壊的変更はその非対称を許容できないので解消した（決定 6）。なお**チャンネル監視トリガの検証は依然 `initialize` だけ**にある（`resolve_watch_triggers` はリポジトリ一覧を要る）—— 本 ADR の範囲外だが、同じ形の穴として残っている
- **`trigger = {}` の意味がソースによって割れた。** github / notion では catch-all、slack ではエラー。SDK と設定リファレンスはこの分岐を明記する必要がある —— 一枚岩の規則を 1 つ失ったことは、この決定の実コストである

# Alternatives

| 案 | 退けた理由 |
|---|---|
| `to_user` / `to_group` に宛先 ID を列挙する | Context のとおり。挙動同一が前提なら情報を増やさず、二重管理とドリフトだけを増やす |
| 同じ形で書くが起動時に実体と照合する | 嘘は防げるが、グループ加入のたびに起動不能になる。「TOML に ID が見える」ために払う運用コストとして釣り合わない |
| 同じ形で書くが照合しない | 実質コメント。死んだグループ ID や他人の ID を書いても誰も気付かない |
| 書いた ID を真として `usergroups.list` を廃止する | これは可読性ではなく**挙動変更**（宛先の絞り込み）。書き落としたグループ宛メンションが黙って落ちる |
| 空 trigger を警告付きで当面受理する | [ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md) が罰したパターン。期限を誰も参照しない |
| 空 trigger を「どの種別でもない」として無視する | 起動は通るのに機能が黙って死ぬ。実行時 warn は設定を直す人の目に入らない |
| `mention = false` も含めて `mention` を常にエラーにする | 決定 3 のとおり、bool として読む規則と両立しない |
