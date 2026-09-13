> 🌐 [English](README.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

# Slack Event Gateway 適合テストスイート

このディレクトリは [ADR-0072](../../ai-docs/decisions/adr-0072-slack-event-gateway.md)
決定 7 が要求する**契約**である。

`event_source = "gateway"` のとき、Slack からのイベントは利用者の管理下で動く
イベントゲートウェイを経由して totsuka に届く。ゲートウェイはフォークして
自前実装されることも想定している（決定 9）ので、**totsuka は相手のコードを信用できず**、
両者が合意できるのはこのディレクトリの中身だけである。

## なぜ「見本 JSON」ではなくテストスイートなのか

決定 4 で保存対象を絞ったため、**ゲートウェイのフィルタが関門になった**。
通らなかったメッセージはレコードが存在せず、totsuka から永久に見えない。
そして誤りの重さは非対称である。

| 誤り | 結果 |
|---|---|
| 偽陽性（メンションでないのに通す） | 無駄な `fetch_message` が 1 回。`mention.rs` が落とす。実害なし |
| 偽陰性（メンションなのに通さない） | **メンションが黙って消える。唯一の致命傷** |

見本を 1 組置くだけでは**形**しか固定できず、**判定**はずれるに任せることになる。
「この本文からこのフラグが出るはず」は見本 1 組では表現できず、そして被害が出るのは
判定のほうである。だから各ケースは「生の配信 → 期待されるレコード」の**組**になっている。

## 置き場と、両側からの読み方

このディレクトリはリポジトリのルート直下にある。ゲートウェイ（`services/slack-event-gateway/`）は
`[workspace] exclude` で Cargo workspace の外に出る（決定 9）ため totsuka の型を共有できず、
共有できるのはこのファイル群だけである。

どちらの側も、`CARGO_MANIFEST_DIR` から**上へ**辿って
`contracts/slack-event-gateway/cases` を含むディレクトリを探す。`../` の段数を
ハードコードしないこと —— 片方をディレクトリごと動かしたとき、ハードコードしたパスは
失敗するのではなく**何も見つけず、ケース 0 件のスイートは緑になる**。
探索に失敗したら panic させること。

## ケースの形

`cases/*.json` が 1 ファイル 1 ケース。

```json
{
  "name": "message-mention-closed-tag",
  "why": "このケースが守っている性質を 1 文で",
  "delivery": {
    "endpoint": "events",
    "content_type": "application/json",
    "payload": { "…Slack が POST する JSON…" }
  },
  "expect": [
    {
      "topic": "events",
      "identity": "message:C0LOBBY:1757640000.000100",
      "record": { "…Pub/Sub トピックに載るレコード…" }
    }
  ]
}
```

| キー | 意味 |
|---|---|
| `delivery.endpoint` | `events` = Event Subscriptions の Request URL、`interactivity` = Interactivity & Shortcuts の Request URL。**この 2 つは Slack アプリ設定の別項目で、前者だけをゲートウェイに向けると承認ボタンが一切届かない** |
| `delivery.content_type` | `application/json` か `application/x-www-form-urlencoded` |
| `delivery.payload` | **デコード後**の JSON。`x-www-form-urlencoded` の場合、実際のリクエストボディは `payload=` にこのオブジェクトを `JSON.stringify` して percent-encode したものが続く |
| `expect` | publish されるレコード。**空配列はフィルタに落ちることを意味する** —— スイートの半分はこれである |
| `expect[].topic` | `events` か `block_actions`。後者が別トピックなのは、保持期間が `response_url` の寿命を超えている必要があるため（決定 5） |
| `expect[].identity` | 配送同一性の鍵。下記 |
| `expect[].record` | Pub/Sub メッセージの本体 |
| `expect_challenge` | `url_verification` のときだけ。エコーすべき文字列 |

### 固定した値

| 値 | 中身 |
|---|---|
| 登録利用者の Slack user id | `U_ME` |
| `received_at` | `2026-09-13T00:00:00Z` |

`received_at` は本来「受け取った時刻」なので実装が決める値である。適合ランナーは
**固定クロックを注入**すること —— ゲートウェイは時刻源を差し替え可能に作る。

totsuka の起票窓は Slack 自身の時計を優先する。レコードはすでにそれを持っていて、
メッセージなら `ts`、ボタン押下なら `action_ts` である。**リアクションだけが例外**で、
Slack は `reaction_added` に `event_ts` を打つが、この `kind` の追加フィールドは
`reaction` / `item_user` で閉じているため、**リアクションがいつ起きたか**を言える値は
`received_at` しかない。`ts` では代用できない —— それは「指しているメッセージ」の時刻で、
1 か月前のメモにリアクションしてタスクを起こすのは普通の使い方だからである。したがって
時計のずれたゲートウェイはこの窓 1 つだけを動かせる。

### 配送同一性

Slack の再送も Pub/Sub の配送も at-least-once なので、`kind` ごとに
「同じ出来事」を指す安定した鍵が要る（決定 7）。鍵が無いと、実装ごとに
重複処理か取りこぼしのどちらかに倒れる。

| `kind` | `identity` |
|---|---|
| `message` | `message:{channel}:{ts}` |
| `reaction` | `reaction:{channel}:{ts}:{user}:{reaction}` |
| `block_actions` | `block_actions:{container_channel}:{action_ts}:{action_id}` |

`message` の接頭辞を除いた部分は totsuka の `Mention::message_key()` と
**バイト単位で一致する**ので、既存の重複排除がゲートウェイ経由と Socket Mode 経由の
両方をそのまま覆う。テストがこれを固定している。

### 間違えやすい規則 2 つ

**`<!subteam^…>` は閉じているときだけ数える。** ID は `<!subteam^` と最初の `>` または `|` の
あいだにあるもので、英数字のみ・32 文字以内であり、かつ実際に `>` が続いていなければならない。
次の `>` までを丸ごと取ると、`<!subteam^S0ABC ここに本文>` がそのままトピックに乗る ——
本文を持たないことが前提のレコードに、自由文が入ることになる。検証を**ここで止めている**のは
意図的で、先頭 `S` や大文字を要求するのは Slack の ID 形式についての推測であり、
外すとメンションが失われる側に倒れるためである。

**`reaction_added` は `item.type` が `message` のときだけ数える。** totsuka は `file` /
`file_comment` を明示的に拒否するので、通すと消費側が必ず捨てるレコードを保存することになる ——
他人のリアクションを除外するのと同じ理屈を、もう 1 段下で適用している。

## このスイートが扱わない範囲

**HTTP レベルの拒否（署名不正・タイムスタンプ切れ・未知パス）はここに無い。**
射影の契約ではなく、ゲートウェイ単体の受け入れ条件だからである（ADR 決定 11）。
このスイートが通っても、署名検証が無い実装は適合していない。

同じ理由で、`x-www-form-urlencoded` のデコードそのものもゲートウェイ側のテストに属する。
ここが固定するのは**デコード後の payload から何が出るか**である。

---

この契約の背景にある設計判断は `ai-docs/decisions/adr-0072-slack-event-gateway.md` にある。
