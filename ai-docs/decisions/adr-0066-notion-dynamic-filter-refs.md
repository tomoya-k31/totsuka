---
type: Decision
title: ADR-0066 trigger.filter の動的な値は @<name> の名前付き lookup で解決する
description: "「現在のスプリント」のように relation 越しの条件が Notion のクエリでは書けず、page id 直書きしか手が無かった問題への決定。[notion.dynamic.<name>] に生の Notion filter で lookup 規則を置き、trigger.filter 内の @<name> を poll ごとに 1 ページへ解決する。0 件と 2 件以上はどちらもエラーにして「条件なし」へ縮退させない。派生列での運用・型キーの追加・横断キャッシュ・予約語表は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/606
tags: [decision, notion, task-source, trigger, filter, adr]
generated: { by: claude-code/opus-5, at: 2026-09-02T10:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。#606 で実装した。

# Context

**Notion のクエリフィルタは、クエリ対象のデータベースのプロパティしか読めない。** relation の先のプロパティを条件にすることはできない。

実運用のスクラムボードで「**現在のスプリントの**タスクだけ拾う」を書こうとすると、これが直接刺さる:

- タスク DB のスプリント列は **relation**（→ 別のスプリント一覧 DB）
- 「現在かどうか」を持っているのは **スプリント側**の `status` プロパティ
- タスク DB 側にその値を引く rollup も formula も無い

したがって `trigger.filter` に書けるのは、**現在のスプリントの page id そのもの**だけになる:

```toml
trigger = { filter = { property = "スプリント", relation = { contains = "<page id>" } } }
```

**この id はスプリントが替わるたびに変わる**（実運用では 2 週間ごと）。書き換えを忘れると、取り込みが 0 件になる。**エラーも警告も出ない** —— フィルタは有効な id 形式のままで、Notion は素直に「該当なし」を返す。`config validate` も `doctor` も緑である。

つまり設定に**今期の答え**を書かせる形になっており、それが静かに腐る。

# Decision

**`[notion.dynamic.<name>]` に lookup の規則を宣言し、`trigger.filter` の中の `@<name>` を poll ごとに解決する。**

```toml
[notion.dynamic.current_sprint]
database_id = "<sprint-database-id>"
filter = { property = "スプリントステータス", status = { equals = "現在" } }

[[workflows]]
trigger = { status = "未着手", assignee = "@me", filter = { and = [
  { property = "タイプ",     multi_select = { contains = "AI" } },
  { property = "スプリント", relation = { contains = "@current_sprint" } },
] } }
```

設定は「**現在のスプリント**を指す」という規則を持ち、今期の id は持たない。

## 決めたこと 5 点

### 1. lookup の条件は生の Notion filter で書かせる

`property` / `equals` / `property_kind` のような分解した鍵を置かない。**プロパティの型（`status` / `select` / `date` …）を totsuka が知る必要が無くなる**ので、Notion の語彙に追従する義務が生まれない。`trigger.filter` 自体が既に生の passthrough なので、書き方も揃う。

### 2. 0 件と 2 件以上はどちらもエラーで、縮退しない

これが本 ADR の中心である。**「解決できないので条件を落とす」は絶対にやらない。** フィルタが消えれば**データベース全体が取り込まれる** —— 最も派手な事故が、成功の見た目で起きる。

`Err` は poll を失敗させ、運用者には毎周期エラーが出る。それが「評価できない条件を書いている」ことの正しい代価である。

2 件以上で**先頭を採らない**のも同じ理由。ページが本当に 2 つのスプリントを指しているとき、任意の一方を黙って選ぶのは誤りである。

判定は `page_size: 2` の 1 クエリで足りる。1 件だけ要求すると曖昧さが原理的に検出できない。

### 3. 名前は `[a-z0-9_]+` に限る

`@` で始まる**文字列リテラル**（`@example.com` / `@Channel`）を参照の名前空間から外すため。この制限があるおかげで、次項の「宣言の無い参照はエラー」を安全に強くできる。

### 4. 宣言の無い `@<name>` は `initialize` でエラー

放置するとタイポが**リテラルとして Notion へ送られ、何にも一致せず、取り込み 0 件**になる。#606 が直そうとしている壊れ方そのものを新しい形で作ることになるので、起動時に落とす。エラー文は宣言済みの名前を列挙する。

検査は **`trigger.filter` の部分木だけ**を歩く。トリガー表全体に広げると `assignee = "@me"` を未宣言の参照として読んでしまう（`@me` は #572 の assignee 語彙）。2 つの名前空間は**予約語表ではなくスコープで**分ける —— 予約語表にすると `plugin_sdk::AssigneeFilter` と歩調を合わせ続ける義務が生まれる。

### 5. poll を跨ぐキャッシュは持たない

解決は `fetch` 1 回の中でのみメモ化する。**lookup の答えが変わることが存在理由**なので、キャッシュには staleness ポリシーが要る。ところがスプリントが切り替わる時刻を config は知らないので、正しい TTL を選べる者がいない。

代価は「参照している名前 × workflow 数」だけクエリが毎 poll 増えること。既定 60 秒間隔・rps 上限 3 に対して十分小さい。

## 却下した案

| 案 | 破れ方 |
|---|---|
| **Notion 側に派生列を足す**（rollup / formula でスプリントのステータスを引き、それを filter で見る。コード変更ゼロ） | 実装としては一番安く、これが可能なら本 ADR は不要だった。**共有本番ボードのスキーマ変更が要る**のが問題で、他チームの合意と、以後その列を壊さない運用が前提になる。「Notion 側を触れない／触りたくない」状況が実在する。なお**この案は今も有効**で、派生列を足せる環境ではそちらのほうが安い（totsuka に何も足さずに済む）。両立するので禁じない |
| `property` / `equals` / `property_kind` に分解した設定 | プロパティ型の語彙を totsuka 側に複製することになり、Notion が型を増やすたびに追従が要る。`property_map.status_kind` に前例はあるが、あちらは**書き戻し**でボディ形状を作る必要があるため型を知らねばならない。lookup は読むだけなので知る必要が無い |
| 解決できないときは条件を落とす（縮退） | **データベース全体を取り込む。** 上記 2 のとおり |
| 2 件以上のとき先頭を採る | 任意の 1 件を黙って選ぶ。上記 2 のとおり |
| `@` で始まる文字列を無条件に参照とみなす | `@example.com` のようなリテラルを壊す。上記 3 のとおり |
| 未宣言の `@<name>` をリテラルとして通す | タイポが取り込み 0 件になり、#606 と同じ静かな壊れ方を再生産する。上記 4 のとおり |
| トリガー表全体で `@<name>` を探す | `assignee = "@me"` が未宣言の参照になる。上記 4 のとおり |
| poll を跨ぐキャッシュ（TTL 付き） | 正しい TTL を選べる者がいない。上記 5 のとおり |
| 予約語を固定する（`@current_sprint` を組み込みにする） | 「現在」の判定はスキーマ依存（プロパティ名も option 名もボードごと）なので、組み込みにできる普遍的な規則が無い |

# Consequences

- **`trigger.filter` に今期の答えを書かなくてよくなった。** スプリントが替わっても config は変えない。
- **代わりに poll あたりのクエリが増える。** 参照している名前 1 つにつき workflow あたり 1 クエリ。
- **lookup が解決できない間はそのソースの取り込みが止まり、毎 poll エラーが出る。** 意図した挙動である（縮退より安全）。スプリントの切り替わりの合間に「現在」のスプリントが 1 つも無い期間があるボードでは、その間エラーが出続ける。
- **`[notion.dynamic.*]` は `[notion]` の下なので全データベース共通**である。データベースごとに違う lookup が要る運用は書けない。要求が出たら決め直す。
- **`@<name>` が使えるのは `trigger.filter` の中だけ**である。`trigger.status` には書けない（status は core 所有のキーで、Orchestrator の閉路検査が字面を読む → [ADR-0062](/decisions/adr-0062-status-vocabulary.md)）。
- Notion の at-most-once（[ADR-0064](/decisions/adr-0064-notion-at-most-once.md)）はこの ADR では変わらない。1 ページ 1 実行の制約は残る。

# 関連

- [ADR-0064 notion のタスクは at-most-once のままにする](/decisions/adr-0064-notion-at-most-once.md)
- [ADR-0062 status は trigger と on_* で同じ綴りにする](/decisions/adr-0062-status-vocabulary.md) —— `status` を core 所有のキーにした決定
- [ADR-0063 取り込みの assignee ゲートは workflow ごとの trigger 条件にする](/decisions/adr-0063-trigger-assignee.md) —— `@me` / `@none` / `@any` の語彙
- [task-source-notion](/components/task-source-notion.md)
- [設定リファレンス](/development/config-reference.md)
