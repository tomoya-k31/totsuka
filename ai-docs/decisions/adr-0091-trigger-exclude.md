---
type: Decision
title: ADR-0091 trigger の否定は exclude テーブル 1 つで書き、中は OR にする
description: "「ラベル waiting が付いていない」のような否定条件が github の trigger で書けなかった問題への決定。trigger と同じ語彙を持つ exclude テーブルを足し、中のどれか 1 つに一致したら取り込まない（NOT (a OR b)）。キー同士は AND・配列は OR という規則を github / notion 共通の語彙として明文化し、label を配列と大小無視の照合に広げる。値の記号（!waiting）・否定キーの増設（exclude_label）・キーごとのテーブル値・in_progress_statuses の統合は不採用。"
tags: [decision, config, workflow, trigger, exclude, label, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。実装済み。実機検収はまだ。

[ADR-0062](/decisions/adr-0062-status-vocabulary.md)（`status` の語彙）、[ADR-0063](/decisions/adr-0063-trigger-assignee.md)（`assignee` の語彙）に続いて、同じ `trigger` テーブルにキーを 1 つ足す決定である。

# Context

github のボードで次の条件を書きたかった:

- `status = "🤖 Spec"` かつ
- assignee が空 かつ
- ラベルに `waiting` が**付いていない**

前の 2 つは書ける（`status = "🤖 Spec"`, `assignee = "@none"`）。3 つ目が書けなかった。github の trigger が読むキーは `assignee` / `label` / `status` の 3 つだけで、`label` は「その 1 つを含む」しか表せない。否定はどのキーにも無かった。

notion は raw `filter` を Notion API へそのまま渡すので、`does_not_contain` 等で否定を**書けた**。書けないのは github だけで、しかも github の ProjectsV2 にはサーバー側の絞り込みが無い（全件を取得してクライアント側で照合する）ので、notion と同じ「生のクエリを渡す」道も無い。

# Decision

## 1. `exclude` テーブルを足す。中は OR

```toml
trigger = { status = "🤖 Spec", assignee = "@none", exclude = { label = "waiting" } }
```

`exclude` の中身は trigger と**同じキー・同じ値の語彙**で、**どれか 1 つに一致したらそのタスクを取り込まない**。

語彙全体の規則はこうなる:

| 場所 | 結合 |
|---|---|
| キー同士 | AND |
| 1 つのキーの配列 | OR |
| `exclude` の中のキー同士 | OR（= `NOT (a OR b)` = `NOT a AND NOT b`） |

`exclude` の中を AND にしなかったのは、ド・モルガンで取り込み側と対称になるからである。取り込み側は「全部満たす」、除外側は「どれも満たさない」。`exclude = { label = "waiting", assignee = "bot" }` を「`waiting` が付いていて**かつ** `bot` がアサインされているものだけ除外」と読む人はまずいない。

## 2. 共通の語彙として github / notion の両方に入れる

解釈の枠（`exclude` の中のキー検査、文字列/配列の読み取り、`exclude.assignee` の解析と評価可能性の検査）は `plugin-sdk` に置き、github と notion の両方につないだ。ADR-0063 で `assignee` の語彙を SDK に置いたのと同じ置き方である。

| | `exclude` の中で書けるキー |
|---|---|
| github | `status` / `label` / `assignee` |
| notion | `status` / `assignee` |

**notion の `exclude` に `filter` は書けない。** `filter` はサーバーへそのまま渡す生のクエリで、Notion API には汎用の NOT が無い。包む方法が無い以上、書けてしまうと「否定したつもりで何もしていない」になる。否定したいなら `filter` の中の演算子で書く。

slack / discord は起動の種別（メンション・リアクション・チャンネル監視）そのものが別物なので対象外。

## 3. 値の形

- `exclude.status` は**配列を許す**。取り込み側の `status` が文字列 1 つなのは core の閉路検査（ADR-0062）の都合で、core は `exclude` を読まない。「配列は OR」という共通規則を優先した
- `label` は取り込み側でも**配列（OR）を許す**ようにした。キー名は単数形のまま —— `assignee` が単数形で配列を受けているのに揃えた
- `exclude.assignee` は `assignee` と同じ語彙だが、**省略時の既定を持たない**（書かなければ誰も除外しない）

## 4. `label` の照合は大文字小文字を区別しない（破壊的変更）

GitHub はラベル名を大小無視で一意にしているので、`label = "Waiting"` と書いて `waiting` に一致しないのは設定の書き手にとって驚きでしかない。一致する範囲が広がる方向の変更で、ADR-0063 で notion の assignee 照合を大小無視にしたのと同じ種類である。

## 5. 検査するのはキーの綴りだけ

- **キーのタイポは `initialize` の硬い失敗にする**（#574 と同じ）。`exclude = { lable = "waiting" }` を黙って捨てると、除外したかったタスクに発火する。入れ子の `exclude` も同じ検査で落ちる（`exclude` の中の有効キーに `exclude` は無い）
- **意味の検査はしない。** `exclude = {}`（何も除外しない）・`exclude = { assignee = "@any" }`（全部除外する）・`exclude` だけの trigger は、書いた側の責任として書いたとおりに動かす
- ただし **評価できない条件は落とす**。notion で `exclude.assignee` を書いたのに `property_map.assignee` が無い、`@me` を書いたのに `notion_user_id` が無い、は ADR-0063 §4 と同じ扱いにした。これは「意味が変」ではなく「黙って何も除外しない」なので、タイポと同じ側に入る

## 6. `in_progress_statuses`（F-08）は統合しない

`in_progress_statuses` も trigger の外で取り込みを弾くゲートで、ADR-0063 が `assignable_to_me` を消したときと同じ二重ゲートの形をしている。それでも残す:

- あれはボードの事実（「この列は実行中」）で、workflow が書くことではない
- status トリガーは文字列 1 つなので、そもそも実行中の列とは一致しない。効くのは status を書かない workflow だけ
- `exclude` の既定にすると、`exclude` を書いた瞬間に実行中ガードが外れる。一番見つけにくい壊れ方である

# Consequences

- github で否定条件が書けるようになった。notion でも `status` / `assignee` の否定が `filter` を使わずに書ける
- `label` が配列と大小無視の照合になった。`label = "Bug"` が `bug` ラベルに一致するようになる
- 取り込み後に除外条件を満たしても、実行中のタスクは止まらない。`exclude` は取り込みの条件である
- `exclude` は lane identity（`message_key`）に関与しない。除外されている間は取り込まれないだけなので、除外が外れた最初の poll が初回の配送になる
- trigger の共通規則を 1 か所に書いた（[設定リファレンス](/development/config-reference.md) の「`trigger` の語彙」節）。それまでは `[[workflows]]` 表の 1 セルに全ソースの語彙が詰め込まれていた

# Alternatives considered

- **値に記号を付ける（`label = "!waiting"`）**: 最短だが、GitHub のラベル名は `!` を含められる。`assignee` の `@` はログイン名に使えない文字なので曖昧さが無かったが、ラベルにはそういう文字が無い
- **否定キーを増やす（`exclude_label` / `not_assignee` …）**: 最初の 1 個は最小差分だが、否定したいキーが増えるたびに `TRIGGER_KEYS` が倍になる。`exclude` なら 1 キーで全キーの否定が書ける
- **キーごとの値をテーブルにする（`label = { any = [...], none = [...] }`）**: 表現力は最大だが、文字列・配列・テーブルの 3 形が混在し、`assignee` の語彙と揃わない
- **式言語（`status:spec && !label:waiting`）**: ADR-0063 と同じ理由で不採用。パーサ・検証・エラーメッセージの維持コストに見合わない
- **取り込み側の `status` も配列にする**: 自然な拡張だが、core の閉路検査と github の `message_key`（`status:{name}@{updatedAt}`）が文字列 1 つを前提にしている。core を巻き込む変更なので切り離した

# 関連

- [ADR-0062 status の語彙統一](/decisions/adr-0062-status-vocabulary.md)
- [ADR-0063 trigger.assignee](/decisions/adr-0063-trigger-assignee.md) —— `exclude.assignee` はこの語彙をそのまま使う
- [ADR-0066 notion の動的な filter 参照](/decisions/adr-0066-notion-dynamic-filter-refs.md)
- #574 —— `trigger` の未知キー検査。`exclude` の中にも同じ検査をかける
- [Trigger（用語）](/glossary/trigger.md)
