---
type: Decision
title: ADR-0081 グループメンションは trigger.to_group で別 workflow へ振り分ける
description: "所属ユーザーグループ宛のメンションを、宛先グループごとに別の workflow（profile / agent / repo が違う）へ振り分けられるようにした決定。一致の優先は「具体的な方が勝つ・順序非依存」で、同率のみ定義順で割る。to_group は所属グループの部分集合に限り、実体照合は live な事実なので initialize だけが行える（config validate はオフラインなので届かない）。タスク ID に workflow 名ではなくグループ ID を入れる理由、repo_pin と post_as の結合を切る理由、trigger.repo が LLM 分類と task/lookup を丸ごと飛ばす理由を記録する。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/reaction.rs
tags: [decision, adr, slack, mention, trigger, routing, usergroup]
generated: { by: claude-code/opus-5, at: 2026-09-19T18:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0080
    resource: /decisions/adr-0080-slack-mention-trigger-marker.md
    title: "メンショントリガを宣言にした決定 — ADR-0080"
  - id: adr-0068
    resource: /decisions/adr-0068-channel-watch-trigger.md
    title: "チャンネル監視トリガの決定 — ADR-0068"
  - id: adr-0025
    resource: /decisions/adr-0025-reaction-task-trigger.md
    title: "リアクションは本人が付けたときだけ — ADR-0025"
  - id: state-db-identity
    resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/state_db.rs
    title: "tasks の識別子が UNIQUE (source, source_task_id) である一次情報"
---

# Status

**採択（stable）。** [ADR-0080](/decisions/adr-0080-slack-mention-trigger-marker.md) の直接の続きで、そこで作った `trigger = { mention = true }` という宣言に宛先を足す。既存設定に破壊的変更はない —— `to_group` を書かなければ挙動は変わらない。

# Context

メンションの行き先は 1 つしか持てなかった。`mention = true` の workflow が 2 つあれば `CONFIG_INVALID` で、自分宛メンションもグループ宛メンションも同じ workflow に落ちていた。

宛先で分けたい理由は 3 つあり、どれも `[[workflows]]` の別のキーを変えたいという話である: グループ宛は `profile` を変えたい（チーム宛の依頼は即返信ではなく triage したい）、`repo` を固定したい（`@design-system` 宛は必ずそのリポジトリ）、`agent` を変えたい。

**一致しうる宛先が複数あることが、この機能の難しさのほぼ全部である。** 実際のメッセージは `@自分 @oncall 見てください` や `@oncall @design 両方に関係します` という形で来る。

# Decision

## 1. `trigger.to_group` に宛先グループを列挙する

```toml
[[workflows]]
name = "slack-oncall"
trigger = { mention = true, to_group = ["S0ONCALL"], repo = "web-app" }
profile = "triage"

[[workflows]]
name = "slack-mention"
trigger = { mention = true }      # 残り全部
profile = "answer"
```

`to_group` は**修飾キー**であって種別キーではない。ADR-0080 の `VALUED_TRIGGER_KIND_KEYS` には入れないので、`mention = true` の無い `to_group` 単独は「起動条件を 1 つも名指していない」として既に落ちる。

## 2. 一致は「具体的な方が勝つ」。順序非依存

`to_group` を持つ route が、素の `mention = true`（catch-all）より常に優先する。**`[[workflows]]` に書く順序は影響しない。**

定義順 first-match（このリポジトリの既定の規約 F-81）を採らなかった理由は、Slack のメンションが**片方は必ず catch-all** という非対称な構造だからである。順序に意味を持たせると「catch-all をうっかり上に書く」が一撃で下の全部を無効化する。ADR-0080 が潰したのは「書き手の意図と関係ないところで行き先が決まる」ことで、同じ判断を延長した。

## 3. 同率の tie-break だけが定義順

別々のグループを名指した 1 メッセージが 2 つの route に一致する（`@oncall @design`）のは**設定として正しい**ので起動時には落とせない。実行時に先に書いた方が勝つ。

メッセージ内の出現順は採らない —— 挙動が**メッセージを書いた他人のタイプ順**に依存し、設定を読んでも予測できなくなる。catch-all へ落とすのも採らない: 専門 route を 2 つ用意したのに、両方が呼ばれたときだけ汎用が動くのは意図から最も遠い。

決定 2 と 3 は**ルートの並び順そのもの**として実装されている。`ReactionTriggers::resolve` が「group route を定義順、catch-all を最後」に組み立てるので、それを前から舐めるだけで両方の規則になる —— 舐める側は規則を知らない。

## 4. `to_group` は所属グループの部分集合に限る

所属外のグループは `initialize` で拒否する。[ADR-0025](/decisions/adr-0025-reaction-task-trigger.md) が「リアクションは本人が付けたときだけ。緩和口は作らない」をハードな不変条件にしたのと同じ性質で、**自分が一切関与していない会話がこのマシンでエージェントを走らせる経路**を作らないためである。

## 5. タスク ID には workflow 名ではなくグループ ID を入れる

group route のタスク ID は `{profile_prefix}:{group_id}:{channel}:{ts}`（profile が prefix を持たなければ `{group_id}:{channel}:{ts}`）。catch-all は従来どおり `{channel}:{reply_ts}` で会話単位のまま。

**独立した ID 空間が要る理由**は、catch-all と同じ ID だと同じ会話の引き渡し（#565）になり、**実行中に届いたトリガーは見送られて Slack は再配送しない**ので黙って失われるからである。

**workflow 名を prefix にする案を検討し、退けた。** 調べたところ現状 workflow のリネームは何も壊さない: `tasks` の識別子は `UNIQUE (source, source_task_id)` で `workflow` は属性にすぎず（行の作成時に固定され `ON CONFLICT DO NOTHING` は更新しない）、リネーム後に同じスレッドへメンションが来れば同じ行が見つかり、#565 の引き渡しが worktree とセッションを保ったまま新しい名前へ移す。進行中タスクも `Queued` に留まり、名前を戻せば復帰する。workflow 名を ID に刻むとこの性質が**両方とも失われる**（引き渡しが起きず、古い行が残ったまま新しい行が並走する）。Slack のグループ ID はグループ名を変えても変わらないので、リネーム耐性を現状の水準に保てる。

`{ts}` だけでも一意性は足りる（決定 3 により 1 メッセージは 1 route にしか行かない）が、グループ ID を入れると**タスク ID を見れば何のメンションで起きたか分かる**。一致したグループの保持は決定 4 の実装で既に必要なので、追加コストはない。

**どのグループを使うかは route に聞く。** メッセージが名指した最初のグループではない。catch-all は空の `to_group` を持つので `None` を答え、会話 ID を保つ —— メッセージ側から採ると、たまたまグループを含んだスレッドで catch-all が勝手に分裂する。route 側から採れば複数グループを claim する route でも**自分の宣言順**で決まるので、タグを打った順序に依存しない。

## 6. `trigger.repo` は `task/lookup` も LLM 分類も飛ばす

既存の `repo_pin` 経路（#617）をそのまま使う。この分岐は `task/lookup` より**前**にあり、リポジトリ分類の `/chat/completions` も、in-thread picker も通らない。

会話が既に settle したリポジトリより pin を優先する。`repo` を書くのは「このグループ宛は必ずこのリポジトリ」という明示的な表明であり、会話の状態次第で効いたり効かなかったりすると設定を読んでも結果が決まらない。代償として、**スレッドの途中で `@oncall` を呼ぶとその会話とは別リポジトリのタスクが立ちうる**。

**`to_group` を伴わない `repo` も書ける。** catch-all もルートの 1 つなので、`trigger = { mention = true, repo = "web-app" }` は「どのメンションもこのリポジトリ」を意味し、分類 LLM を一切呼ばなくなる。候補リポジトリが 1 つしかない構成では素直に有用で、禁じる方が特例になる（watch の `repo` もグループを要求しない）。

## 7. `repo_pin` と「bot 名義で出す」の結合を切る

`post_as` は `repo_pin.is_some()` から導出されていた。根拠は「`repo_pin` を立てるのは channel watch だけで、watch の結果は bot の投稿」だが、group route がその前提を破る。

結合を残したまま `repo` を足すと、**グループ宛メンションへの返信だけが黙ってあなた名義から bot 名義に変わる**。本人名義の代理返信はこのプラグインの中心的な性質（[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md)）なので、設定に書いていない理由でそれが変わってはいけない。`Mention` に `post_as_bot` を持たせ、watch だけが `true` を立てる。

## 8. catch-all は「残り全部」

group route を 1 つ足しても、**名指しされていない所属グループ宛のメンションは catch-all に落ちる**。挙動は今までと同じ。

「catch-all は個人宛のみ」も検討したが、それは**挙動変更**であり、しかも「グループ宛 workflow を 1 つ足したら、無関係なグループのメンションが黙って来なくなる」という設定した覚えのない副作用になる。ノイズ削減は独立した決定として扱うべきで、この機能の副産物として忍び込ませない。

## 9. `to_group` があって `usergroups:read` が無ければハードエラー

スコープが無いと所属を解決できず、group route は**永久に一致しない**うえ決定 4 の検証もできない。設定は正しく見えるのに黙って死ぬので拒否する。`to_group` を書いていない設定は従来どおり警告のままで、既存への影響はない。

## 10. 所属の照合は `initialize` だけができる

`config/validate` は**意図的にオフライン**（ライブなトークン検証は `initialize` の TokenGuard の仕事）なので、`usergroups.list` を要する決定 4・9 はそこに置けない。**`totsuka config validate` は「あなたが抜けたグループ」を検出できない** —— 失効したトークンを検出できないのと同じ区分である。形（配列か・`S…` か・空でないか・`mention = true` があるか・同じグループを 2 つの workflow が claim していないか）は両方の経路で検査する。

## 11. `task_id_prefix` と `instructions_kind` は必ず一緒に動く

catch-all は prefix を持たない（タスクが会話そのものだから）ので、`profile` が何であれ**返信の指示**を取る —— ADR-0081 以前のメンション経路が両方を `None` に固定していたのと同じ挙動である。group route はメッセージ単位に key するので、`triage` / `implement` が想定する形になり、profile の指示を取る。

**割ると 1 つだけ不整合な状態が生まれる**: 会話に key されたタスクが implement の指示で走り、返信を期待していたスレッドに対してブランチと PR を開く。実装レビューで実際にこの穴が見つかった（`instructions_kind` だけを profile 由来にしていた）ので、対であることをコードの上でも 1 つの規則として書いてある。

# Consequences

- **既存設定は無変更で動く。** `to_group` を書かなければ route は catch-all 1 つで、ADR-0080 以前と同じ
- **`usergroups.list` の呼び出し位置が変わった。** `to_group` があるときだけ `initialize` で解決し、結果をパイプラインへ渡す（二重呼び出しをしない）。無ければ従来どおりパイプライン起動時に非致命的に解決する
- **`repo_pin` から `post_as` を導出していた潜在バグが 1 つ消えた。** 「`repo_pin` を立てるのは watch だけ」という暗黙の前提を守る仕組みはコード上どこにも無かった
- **mention workflow は watch リゾルバに渡さなくなった。** SDK は `channel` の無い `repo` を「壊れた watch」として拒否するので、渡したままだと group route の `repo` が弾かれる。mention は watch ではないので所有境界としても正しい
- **「操作者本人のタグを引用本文から外すか」も `post_as_bot` に移した。** #632 の処理は `repo_pin` から同じ推論をしていたので、pin を持つ group route では**本人名義で答えるのに本人のタグがエージェントに渡る**ところだった。スレッド文脈の行は `sanitize_reply` を通らないので、そちらには後段の網も無い
- **`channel_name` / `from` を mention workflow に書いたら拒否する。** mention workflow を watch リゾルバに渡さなくした副作用で、SDK の orphan 検査が効かなくなった 2 キーを自前で拾う
- **`config validate` と `initialize` の検査範囲が非対称になった。** 決定 10 のとおり避けられないが、ADR-0080 で「両者が食い違えないように」`resolve_trigger_shape` へ集約した直後に、意図的な非対称を 1 つ足したことになる。形は共有し、ライブな事実だけが `initialize` 側にある

# Alternatives

| 案 | 退けた理由 |
|---|---|
| 定義順 first-match で一致を決める | catch-all を上に書くと下の全部が永久に到達不能になる。Slack のメンションは片方が必ず catch-all という非対称な構造 |
| 同率をメッセージ内の出現順で割る | 挙動がメッセージを書いた他人のタイプ順に依存し、設定から予測できない |
| 同率で両方起動する | 1 メッセージ 1 タスクという既存の形を崩し、dedup キーと `UNIQUE(source, source_task_id)` の両方を作り替えることになる |
| 同率を catch-all に落とす | 専門 route を 2 つ用意したのに、両方が呼ばれたときだけ汎用が動く |
| 所属外のグループも書けるようにする | 自分が関与していない会話がこのマシンでエージェントを走らせる経路になる。ADR-0025 と同質の緩和で、やるなら独立した決定 |
| タスク ID の prefix に workflow 名を使う | 現状リネームは何も壊さないのに、新しい破壊を持ち込む（決定 5） |
| `task_id_prefix` を config に書かせる | profile から導出する [ADR-0033](/decisions/adr-0033-workflow-profile.md) に穴を開け、「書き忘れたら」という枝が増える |
| catch-all を「個人宛のみ」にする | 挙動変更。group route を足すと無関係なグループのメンションが黙って落ちる |
| `to_user` も対称に足す | 決定 4 で所属外を禁じた対称として自分以外は書けず、1 通りしか書けないキーになる。必要になってから足せる |
