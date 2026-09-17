---
type: Decision
title: ADR-0079 bot が投稿したメッセージへのリアクションでタスクを起こす
description: "Slack の全経路（Gateway の publish 判定・メンション判定表・チャンネル監視・リアクション）が bot 投稿を無条件に捨てているため、bot が流す PR 承認申請を起点にできなかった問題への決定。緩めるのは「リアクションを付けた人」ではなく「反応先の投稿者」だけであり、許可は workflow の trigger 単位の from_bot で宣言する。Gateway を変更せず schema も据え置ける理由、repo 解決を既存の LLM 分類のままにする理由、profile に implement を選ばざるを得ない理由（design は Bash(gh api *) を deny するため対象スキルが動かない）を記録する。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/reaction.rs
tags: [decision, adr, slack, reaction, trigger, bot, permissions]
generated: { by: claude-code/opus-5, at: 2026-09-18T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0025
    resource: /decisions/adr-0025-reaction-task-trigger.md
    title: "リアクションをタスクのトリガにした決定 — ADR-0025"
  - id: adr-0068
    resource: /decisions/adr-0068-channel-watch-trigger.md
    title: "チャンネル監視トリガの決定 — ADR-0068"
  - id: bot-post-observation
    resource: "実機観測。承認申請 bot が投稿する Slack チャンネルの直近 2 日ぶんの投稿（2026-09-16〜09-17）"
    title: "起点になる bot 投稿の実物"
---

# Status

**採択（stable）。** 決定 1〜5 は実装済みで、決定 6・7 は設定と運用の取り決めである。

[ADR-0025](/decisions/adr-0025-reaction-task-trigger.md) 決定 1（「本人が付けたときだけ受理する。緩和口は作らない」）は**変えない**。本 ADR が緩めるのは別の軸 —— **反応先メッセージの投稿者**である。

同 ADR 決定 4 は「リアクション経路は**反応先の投稿者を見ない**」と書いているが、実装（`reaction.rs` の `to_mention`）は `subtype` / `bot_id` を見て捨てている。本 ADR はその齟齬を、**見る対象を限定したうえで明文化する**方向で解消する。

# Context

## やりたいこと

承認フロー bot が Slack へ流す「PR のリリース承認申請」を起点に、対象 PR のリポジトリを特定し、その repo で Claude を起動してレビュー用のスキルを実行したい。

起点になる投稿は完全に定型である。

```text
<!subteam^S0EXAMPLE> :soon:リリース承認申請が届きました
 タイトル: feat: [ABC-1234] …
 URL: <https://github.com/example-org/service-api/pull/1234>
repo: <https://github.com/example-org/service-api|example-org/service-api>
```

## なぜ現状は 1 件も拾えないのか

bot 投稿の除外は**5 箇所で重複して掛かっており、設定で緩められる箇所が 1 つも無い**。

| 層 | 位置 | 何をするか |
|---|---|---|
| Gateway の publish 判定 | `services/slack-event-gateway/src/project.rs:210` | `subtype` / `bot_id` があれば publish しない |
| 契約モジュール | `plugins/task-source-slack/src/gateway_contract.rs:587` | 同上 |
| メンション判定表 1 行目 | `plugins/task-source-slack/src/mention.rs:11` | 同上 |
| チャンネル監視 | `plugins/task-source-slack/src/watch.rs:154` | 同上 |
| リアクション | `plugins/task-source-slack/src/reaction.rs:291` | 再取得したメッセージが bot 投稿なら捨てる |

`mention.rs` には、まさにこの用途を「タスクにならないこと」として固定した回帰テストがある。

```rust
let mut bot = said("<!subteam^S0MINE> deploy finished");
bot["bot_id"] = json!("B0DEPLOY");
assert!(filter_in_group().assess(&bot).is_none());
```

## 実機で確かめた 3 つの事実

1. **投稿者は `bot_id` として届き、`user` を持たない。** 人間の投稿が `U…` を返すのに対し bot は `B…` で、`user` フィールドが無い古典的な bot 投稿の形である
2. **repo は本文に literal で入っている。** `repo: <owner>/<name>` の行が毎回付く
3. **流量は 1 日 5〜10 件**で、`pull` ではなく `issues` を指す申請も同じ形式で混ざる。すでに人手の承認リアクション運用が乗っている

# Decision

## 1. 緩めるのは「反応先の投稿者」だけ

起動のジェスチャは**操作者本人のリアクションのまま**にする。ADR-0025 決定 1 が守っている性質 —— 「同僚が絵文字を 1 つ付けるだけで他人のマシンで実行が始まる」ことが無い —— は 1 ミリも動かない。

動かすのは「反応先が人間の投稿であること」という暗黙の前提だけである。**bot 投稿は自動では何も起こさない。** 操作者が 1 件ずつ見て絵文字を付けたものだけがタスクになる。1 日 5〜10 件という流量に対して、取捨選択を人間が握り続けるという意味でもある。

## 2. 許可は workflow の trigger 単位で宣言する

```toml
[[workflows]]
name = "pr-approval-review"
projects = ["slack"]
agent = "herdr"
profile = "implement"
trigger = { reaction = "mushimegane", from_bot = ["B0EXAMPLE"] }
```

`[slack]` のグローバル設定にしない。グローバルだと**すべての絵文字トリガが一斉にその bot へ開く**ので、`yaruzo` / `todo` / `eyes` が意図せず bot 投稿にも効くようになる。trigger 単位なら「この絵文字だけ bot 投稿にも効く」という宣言になり、既存の 4 本は人間投稿のみのまま保たれる。

**`channel` を併記して「このチャンネルの bot だけ」と縛る書き方は採れない。** `channel` はチャンネル監視トリガの語彙で、`reaction` との併記は `WatchTrigger::parse` が明示的にエラーにする（[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)）。絞り込みの軸は bot id のみとする。

## 3. Gateway は変更しない。wire schema も据え置く

リアクション経路の Pub/Sub レコードは**リアクションを付けた人（= 操作者）と座標だけ**を運び、本文はプラグインが `fetch_message` で引き直す。`project_reaction` は bot 判定をしておらず、`item_user` も `Option` である。

したがって `SCHEMA_VERSION` のバンプも、Gateway の再デプロイも要らない。**メッセージ経路を緩める案（不採用、下記）ならこれらが全部必要になる**ので、この差は小さくない。

## 4. `Mention.user` は `bot_id` で埋める

`SlackMessage` は既に `user: Option<String>` と `bot_id: Option<String>` を持っている。`Mention.user` は表示名解決（`pipeline.rs:1151`）と `sender_id` の記録にしか使われず、解決に失敗したら id をそのまま出すフォールバックが既にある。

よって `user` が無ければ `bot_id` を入れる。pane に出る送信者名が bot id のままになるのは許容し、`bots.info` による表示名解決は**本 ADR のスコープ外**とする。

## 5. リポジトリ解決は既存のまま

リアクション由来のタスクは repo が固定されないので、既存の 3 段（`[[channel_groups]]` の prefix → プラグイン内 LLM 分類 → スレッド内ピッカー）にそのまま乗る。本文に repo 名が literal で入っているので分類はまず外さない。

**URL から決定的に repo を引く段を挟む案は採らない。** 効果は「LLM 呼び出し 1 回の節約」と「外したときの誤爆の回避」だが、外れた場合の受け皿（ピッカー）が既にあり、解決段を 1 つ増やすほどの差にならない。必要になったら別 ADR にする。

前提として、申請が流れてくる repo は `[[repositories]]` に登録されている必要がある。

## 6. profile は `implement`、`output` は `none`

成果物は pane で読むだけとし、Slack のスレッドへは返さない。

**`design` を選べない。** 読み取り専用 profile として筋は良いが、対象スキルが動かない。

| 衝突 | 場所 |
|---|---|
| `gh api` を 5 箇所で使う（うち `repos/…/deployments` は `gh` にサブコマンドが存在せず代替不能） | `design` は `Bash(gh api *)` を deny |
| worktree 後片付けの `git branch -D` | `Bash(git branch *)` を deny |
| レポート出力 | `Edit` / `Write` を deny |

`deny_rules` は Rust 固定で、設定文字列から到達させない（[ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) と同じ理由）。そして `Bash(gh api *)` を `design` から単に外すと、`gh api -X DELETE repos/{owner}/{repo}` や `gh api -X POST repos/{owner}/{repo}/pulls` が通るので `DENY_PR` / `DENY_REPO_ADMIN` が実効を失う —— 「実際より強く読めるルール列」は短いルール列より悪い、というのが `permissions.rs` がそこに書いている判断である。

`deny_rules` が `None` を返す唯一の profile が `implement` なので、**まず動かす**ためにこれを採る。レビューに不要な push / PR 作成の権限まで開くことは自覚したうえでのトレードオフであり、`design` の deny 見直し（`permissions.rs` 自身が「`gh` サブコマンドの無い API が要ると分かったらルールを意図的に見直す合図だ」と予告している）は**別 ADR に残す**。

## 7. PR ブランチの取得はスキルに任せる

totsuka が用意する worktree は `origin/{default}` の detached HEAD である。対象スキルは PR URL だけで動き、必要なとき（Java の呼び出し階層を見るときだけ）に自分で `git worktree add ./.worktree/pr-N` を作る。

したがって totsuka 側の worktree は「どの repo のディレクトリで Claude を起動するか」を決める役割に徹し、PR ブランチの取得を `initial_prompt` で指示しない。ネストした worktree が残ると親の削除が失敗しうるので、スキル側の後片付けに依存する点は既知のリスクとして受け入れる。

# 検討した選択肢

## bot 投稿を自動でタスク化する（メンション経路を緩める）

**不採用。** 取りこぼしが無いのは利点だが、代償が 3 つある。

1. 1 日 5〜10 件の pane が無人で立ち上がる
2. PR の本文とタイトル（**第三者が書く**）が、そのままエージェントの起動入力になる
3. Gateway の publish 判定と `user` 必須を両方緩める必要があり、`SCHEMA_VERSION` のバンプと Gateway の再デプロイを伴う

## チャンネル監視トリガを bot へ開く

**不採用。** 監視トリガは `repo` を config で固定する設計で、それが設計の核である（ADR-0068）。対象チャンネルには 10 以上の repo の申請が流れるので、固定すると決定 5 のリポジトリ解決が成立しない。`repo` を任意化するのは監視トリガの契約を崩す変更で、本件の代償としては重すぎる。

## `design` の deny セットを見直す

**本 ADR では不採用、別 ADR へ送る。** 筋は通っている（決定 6 参照）が、「読み取り API は要るが書き込みは禁じる」を deny パターンで表すのは `&&`・パイプ・heredoc で抜けられるという既知の失敗があり、[ADR-0036](/decisions/adr-0036-read-only-violation-fails-the-task.md) の「読み取り違反はタスクを失敗させる」層とどう分担するかまで含めて設計が要る。本件を止めてまで先に解く問題ではない。

# Consequences

## 実装範囲

| 対象 | 変更 |
|---|---|
| `plugins/task-source-slack/src/reaction.rs` | `WorkflowTrigger` / `TriggerEmoji` に `from_bot` を持たせ、`to_mention` の bot フィルタを「この workflow が許可した `bot_id` なら通す」に変更。`user` が無ければ `bot_id` で埋める |
| 同 `config.rs` | trigger の有効キーに `from_bot` を追加（未知キーは `initialize` の硬い失敗になるため必須） |
| `services/slack-event-gateway/` | **変更なし** |
| `crates/orchestrator-core/` | **変更なし** |
| `config.toml` | 対象 repo の `[[repositories]]` 登録と `[[slack.channel_groups]].repos` への追記、workflow 1 本の追加 |
| docs | 本 ADR・[task-source-slack](/components/task-source-slack.md)・[設定リファレンス](/development/config-reference.md)・log 断片 |

## 残るリスク

- **対象スキルが Notion MCP を前提条件チェックで要求し、無ければレビューせず中断する。** 無人 pane での故障モードが「何も出力せずに終わる」になるため、実機検証が要る
- **bot 投稿の本文は第三者が書いた PR タイトルを含む。** 起動は操作者のリアクションなので暴走はしないが、起動後にエージェントが読む入力として扱う（指示として実行させない）前提は変わらない
- **回帰テストを 1 本書き換える。** `the_earlier_filter_rows_still_outrank_a_group_mention` が固定している「bot 投稿はタスクにならない」はメンション経路の話なので残すが、リアクション経路側には「許可した bot だけ通り、許可していない bot は落ちる」を両方向で固定するテストを新設する

# 実装

`trigger.from_bot` として実装した。Gateway・orchestrator-core・wire schema はいずれも無変更である。

| 対象 | 何をしたか |
|---|---|
| `reaction.rs` | `WorkflowTrigger` / `TriggerEmoji` / `ReactionTarget` に `from_bot` を通し、`to_mention` の bot フィルタを許可リスト参照に変更。`user` が無ければ `bot_id` を入れる |
| `server.rs` | `TRIGGER_KEYS` に `from_bot` を追加。`parse_from_bot` が値の形を検証し、`reaction` 無し・`channel` 併記の 2 つの誤用を `initialize` で弾く |
| `approval.rs` | 返信の先頭に付く `<@sender_id>` を `asker_prefix` に切り出し、**人間の id（`U…` / `W…`）のときだけ**付けるようにした |
| `templates/config.toml` | トリガ形状の例に 1 行追加 |
| `scripts/config-template-lint.sh` | `OPAQUE_ALLOWED` に登録（プラグインが解釈する無解釈キーなので config struct には現れない） |

`approval.rs` の変更は決定 4 の副作用を塞ぐものである。返信は「訊いた人」への `<@…>` メンションで始まるが、bot 由来のタスクではその id が `B…` になる。**Slack の id は接頭辞で型が決まり、`U…` / `W…` だけが人間を指す**ので、`<@B0123ABC>` は黙って落ちるのではなく**その文字列のまま**返信の先頭に描画される。しかも通知すべき相手が居ないので、そもそもこの接頭辞が存在する理由が無い。`output = "source"` と `from_bot` を併用した構成でのみ踏むが、踏むと投稿済みのメッセージに残る。

回帰ガードは 8 本。うち 2 本が不変条件そのものを固定している —— **空の `from_bot` はどの bot も通さない**（このキーができる前の挙動）と、**許可した bot の編集は依然として落ちる**（広がるのは投稿者であってイベント種別ではない）。

**まだ実機で確かめていない。** 対象スキルが前提条件チェックで Notion MCP を要求し、無ければレビューせず中断するため、無人 pane での故障モードは「何も出力せず終了」になる。`verified` はそれを確認してから書く。
