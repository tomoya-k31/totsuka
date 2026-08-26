---
type: Decision
title: ADR-0063 取り込みの assignee ゲートは workflow ごとの trigger 条件にする
description: "プラグイン全体でハードコードされていた F-08 の取り込みゲート（未アサイン または 自分）を、[[workflows]].trigger.assignee へ移す決定。@me / @none / @any / login / 配列の語彙を持ち、省略時の既定が旧ゲートと同一なので二重ゲートにならない。未アサインを人間の取り分として残す運用が初めて書けるようになる。式言語・二重ゲートの維持・bot アカウントの分離は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/572
tags: [decision, config, workflow, trigger, assignee, ingest, adr]
generated: { by: claude-code/opus-5, at: 2026-08-27T05:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。実装・**実機検収（2026-08-27）**まで完了。

検収は**同じ issue で assignee だけを変える A/B** で取った。片側だけだと「fetch が空振りしただけ」と区別できないためで、単体テストで対にしてある形をそのまま実機へ持ち込んでいる。`trigger = { status = "Todo", assignee = "@me" }` の下で、`Todo` 列にある**未アサインの** issue は 2 ポーリングを超えて（4 回観測）取り込まれず、同じ issue に `tomoya-k31` を付けた 73 秒後に `dispatched` になった。列も本文も変えていない。

あわせて claim の縮退（§Consequences）も確認できた: 取り込み後も assignee は `tomoya-k31` 1 人のままで、self-assign の書き込みは発生していない。

[ADR-0062](/decisions/adr-0062-status-vocabulary.md) が揃えた `trigger` の語彙に乗る決定であり、同じテーブルにキーを 1 つ足す。

# Context

既存のボード運用に totsuka を載せるとき、**「着手可能」を表す列が分かれていない**環境がある。ステータスもラベルも存在して動いてはいるが、「設計レビュー済みで実装に入ってよい」といった区別が列として切られておらず、列を増やすこともできない。

そこで assignee を「AI に渡す合図」に使いたい:

- `status = Todo` かつ**自分にアサインされている** → 着手する
- `status = Todo` かつ**未アサイン** → 人間の取り分として残す

**後者が書けなかった。** 取り込みゲートは workflow ごとではなくプラグイン全体でハードコードされていた:

```rust
// 旧 GithubConfig::assignable_to_me
assignees.is_empty() || assignees.iter().any(|l| l.eq_ignore_ascii_case(&self.github_login))
```

未アサインは**常に**取り込まれるので、「通常は人間がタスクを取る」という制御が成立しない。前者（自己アサインの検知 = F-08）は既定でそうなっているため、足りなかったのは「未アサインを除外できること」と「その条件を workflow ごとに書けること」の 2 つである。

notion は raw `filter` の passthrough があるので**条件自体は書けた**が、`assignable_to_me` は同じくハードコードで効いていたため、raw filter では「未アサインを除外」も「他人のタスクを取り込む」も実現できなかった。

# Decision

## 1. `trigger.assignee` を足す

```toml
[[workflows]]
name       = "github-implement"
source     = "github"
trigger    = { status = "🤖 実装・受入検証", assignee = "@me" }
profile    = "implement"
on_start   = { status = "🚧 実装中" }
on_success = { status = "🚧 最終レビュー" }
```

| 値 | 意味 |
|---|---|
| 省略 | `["@me", "@none"]` —— 旧ゲートと**同一** |
| `"@me"` | 運用者本人が assignee に含まれる |
| `"@none"` | 未アサイン |
| `"@any"` | 条件なし。**他人のタスクも取り込む**（新しい能力） |
| `"<login>"` | その login（notion は user id）が含まれる |
| 配列 | いずれか（OR） |

**特殊語に `@` を付けるのは衝突回避である。** `me` / `none` / `any` は GitHub のログイン名として実在しうるので、素の文字列だと「`any` というユーザーのタスク」が書けなくなる。`@` はログイン名に使えない文字なので曖昧さがゼロになり、ついでに GitHub 検索構文（`assignee:@me`）の見た目に寄る。

## 2. 旧ゲートは「置き換える」。前に残さない

`assignable_to_me` は github / notion 両方から**削除**した。フィルタが唯一の経路で、省略時の既定がその旧ゲートそのものである。

「書かれていたらバイパスする」形にしなかったのは、**二重ゲートが「設定に書いた条件が黙って何も起こさない」を作る**からである。`assignee = "teammate"` は旧ゲートに必ず弾かれるので、書けるのに効かない設定になる。経路を 1 本にすれば、書かれた条件を書かれていない条件が上書きすることは構造的に起きない。

## 3. 語彙と照合は SDK に置き、参照先はプラグインが持つ

共有するのは**キー名 `assignee` と値の語彙・照合ロジック**（`plugin_sdk::AssigneeFilter`）だけ。何と突き合わせるかは各プラグインのままである:

| | github | notion |
|---|---|---|
| assignee の取得元 | Issue 組み込みの `assignees`（設定キー無し） | `[notion].property_map.assignee` が名指す people プロパティ |
| assignee の値 | ログイン名 | Notion の user id |
| 「自分」 | `[github].github_login`（必須） | `[notion].notion_user_id`（任意） |

`status` と違い `assignee` は **core 予約キーではない** —— core はこのキーを読まない。両ソースで同じ綴りにしているのは、ソースを乗り換えても workflow の書き方が変わらないようにするためで、規約であって強制ではない。

## 4. 評価できない条件は `initialize` の硬い失敗にする

notion には**黙って効かなくなる前提が 2 つ**ある。people プロパティが未マップだと assignee が常に空リストとして読まれ、`notion_user_id` が未設定だと `@me` が誰にも一致しない。どちらも旧ゲートの下では実害が薄かったが、**条件を明示したのに評価不能**なら、そのワークフローは永久に起動しないまま無言になる。

したがって `trigger.assignee` を書いたら `property_map.assignee` は必須、値に `@me` を含むなら `notion_user_id` も必須とし、`initialize` で落とす。github は `github_login` が必須なので追加の前提は無い。

**`@me` 以外のログイン名は `notion_user_id` を要求しない。** 名前を assignee 一覧と突き合わせる作業は誰が実行しても同じで、「自分が誰か」を知る必要がないためである（起票時の #572 本文はここを「ログイン名でも必須」と書いていたが、実装では不要と判断した）。

## 5. `assignee` 単独トリガーは at-most-once（github でのみ警告 1 行）

github の `message_key` は status トリガーのときだけ `status:{name}@{updatedAt}` になる。`assignee` 単独だと `None` にフォールバックし、`UNIQUE(task_id, message_key)` で 1 タスク 1 回になる。label 単独トリガーが今日そうなっているのと同じ性質なので、**エラーではなく `initialize` の警告 1 行**にする（エラーにすると label 単独の既存挙動と不揃いになる）。

**警告を出すのは lane identity を刻むソースだけである。** notion は `message_key` を**どのトリガーでも** `None` にしている（#573）ので、そこで「`status` を足せば再実行できる」と案内しても直らない。効かない対処を教えるのは黙っているより悪いので、`check` は `status_mints_lane_identity` を受け取り、github だけがこの警告を出す。notion の at-most-once はトリガーの種類によらないソース全体の性質で、#573 の担当である。

# Consequences

- **未アサインを人間の取り分として残せるようになった。** どの workflow にも一致しないカードは取り込まれない。
- **`@any` は新しい能力である。** 他人のタスクまで取り込めるようになったので、書くときは意図的であること。
- **first-match の順序に落とし穴がある。** 既定が許容的（`["@me","@none"]`）なので、`assignee` を省略した catch-all を上に置くと未アサインごと飲み込む。設定例集に順序の注意を書いた。
- **claim（#556）は同一ログイン運用で書き込みなしの `Won` に縮退する。** `assignee = "@me"` で dispatch されるタスクは既に自分がアサインされており、`claim` の pre-read が `holds(me)` で即 `Won` を返す。壊れではなく、**排他の担い手が claim から「人間のアサインそのもの」へ移る**ということである。ただし同一 `github_login` で 2 台の totsuka を動かすと原理的に裁定不能という制約は今より重くなる。
- **notion の assignee 照合が case-insensitive になった。** 旧 `assignable_to_me` は user id を `==` で比べていた。UUID の表記ゆれで一致するようになるだけで実害は想定していないが、挙動の拡大ではある。
- **`@me` を「AI に渡す合図」に使う運用では、「人間が自分で作業中」をボード上で表現できない**（アサインした瞬間に走る）。bot アカウントを分ければ表現できるが、下記の理由で今回は採らない。

# Alternatives considered

- **式言語**（`status:todo && assignees in (tomoya-k31)`）: パーサ・検証・エラーメッセージの維持コストに見合わない。キーの AND と配列の OR で `in (...)` と `is empty` は表現できる。将来もっと複雑な条件が要るなら、notion の raw `filter` と同型のエスケープハッチを足すほうが筋がよい。
- **旧ゲートを前に残し、`assignee` があればバイパスする**: 差分は小さいが、`assignee = "teammate"` のような「書けるのに効かない」設定を許す。二重ゲートは一番デバッグしづらい壊れ方を作る。
- **bot アカウントを `github_login` にする**: 「人間が自分で作業中」「AI に渡す」「AI が claim した印」の 3 つを分離できるが、bot 用トークンと権限（user 所有ボードは scope 方式のトークンが必須）が要る。今回の運用は「自分にアサイン = AI に渡す」で意図が一致するので同一ログインのままとする。
- **`AssignedEvent` を fetch に足して `assignee:{login}@{createdAt}` を lane-entry identity にする**: 「アサインを外して付け直す = 再実行」を作る案。列が存在しない環境を想定していたが、その前提が誤りだった（列は在って動く。分かれていないだけ）。**github なら** status を併記すれば既存の `message_key` で再実行が成立するので不要。notion には元から lane identity が無く（#573）、この案でも解決しないので、そちらは #573 の側で決める。
- **プラグインごとに別の綴りにする**（notion は `person` 等）: ソース固有の語を使えるが、乗り換え時に workflow を書き直すことになる。`assignee` は両ソースで意味が同じなので揃えた。

# 関連

- [ADR-0062 status の語彙統一](/decisions/adr-0062-status-vocabulary.md) —— 同じ `trigger` テーブルの語彙
- [ADR-0059 claim による二重着手防止](/decisions/adr-0059-task-claim-exclusion.md)
- #574 —— `trigger` / `on_*` の未知キー検査。`assignee` のタイポもここで落ちる
