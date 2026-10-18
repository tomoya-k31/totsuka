---
type: Decision
title: ADR-0120 Slack のメンションは返信要否を LLM で判定し、不要ならタスクにしない
description: task-source-slack が、メンション（追いメンション・グループ宛を含む）を workflow に割り当てた直後、repo 解決と task/submit より前に、本文・送信者種別・スレッド文脈から返信が要るかを [llm]（chat / decisions）で判定する決定。返信不要（専用閾値 0.7 以上）ならアプリ DM に理由と抜粋を送って終わり、判定に失敗したら中断して DM で知らせる。どちらも重複防止キーを戻し、本人のリアクションで必ずタスク化できる。設定なしで常に有効で、メンションに答える workflow があれば [llm] が必須になる。
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/reply_check.rs
tags: [decision, slack, llm, mention, noise, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T21:18:02+09:00 }
status: stable
owner: tomoya-k31
---

# Status

**採択（stable）。** #927 の stacked PR 3/3 で実装した。[ADR-0081](/decisions/adr-0081-slack-group-mention-routing.md) が「ノイズ削減は独立した決定として扱うべき」と書いて残した課題への答え。[ADR-0118](/decisions/adr-0118-llm-choice-crate.md)（判定の土台）と [ADR-0119](/decisions/adr-0119-plugin-degraded-notification.md)（失敗の可視化）に依存する。

# Context

自分宛のメンションはすべてタスクになり、エージェントが返信案を作る。実際には返信の要らないメンションが多い。

- 共有・FYI・ご参考・Cc・お知らせ・会議の案内・お礼だけ・「見てもらえれば」程度の軽い依頼
- 完了したら所定の絵文字リアクションを付けるよう指定された作業依頼（返信ではなくリアクションで答える）
- bot からの依頼・お願い・確認依頼（[ADR-0109](/decisions/adr-0109-slack-bot-mentions.md) で bot のメンションも受けるようになった）

これらに worktree とエージェントのセッションを割き、毎回返信案を却下している。

# Decision

## 1. 判定の位置と対象

`handle_mention` の先頭、repo の pin・`task/lookup`・repo 解決より前に置く。対象はメンション経路（catch-all と `to_group`）のすべてで、既知スレッドへの追いメンションも含む。

- **対象外**: 本人のリアクション起動（本人が明示した依頼）、チャンネル監視
- Slack プラグインの中だけの処理で、他の task_source と core のタスク処理には入らない
- 返信不要なら repo 分類は呼ばない

## 2. 判定の材料と問い

[ADR-0118](/decisions/adr-0118-llm-choice-crate.md) の `Question { id: "reply", options: [reply, no_reply], none: None }` を [llm] の backend に投げる。subject は本文・`Sender: bot | human`・スレッド文脈（`thread_context_limit` 件）。enrich 済みなので Slack API の追加呼び出しは無い。

bot かどうかは `SlackMessage.bot_id` を `Mention` に運んで判定する（`user` の頭文字では、`bot_id` と `U…` の両方を持つアプリの bot を取りこぼす）。bot の依頼を機械的に落とさず LLM に渡すのは、bot が人間の依頼を代理投稿する境界例を文面で判断させるため。

## 3. 基準はプロンプトに書き、上書きできる

`[slack.prompts]` に 4 キーを足す（組み込みは英語、[ADR-0054](/decisions/adr-0054-prompt-language-policy.md)）。

| キー | 使う backend |
|---|---|
| `reply_check_criteria` | 両方（chat は system に、decisions は instructions に入る） |
| `reply_check_system` / `reply_check_user` / `reply_check_correction` | chat |

組織固有の絵文字名は組み込みに書かない。「完了報告のリアクションを指定した依頼」と一般化し、名前は運用者が上書きで足す。

## 4. 閾値は専用

`[slack] reply_check_threshold`（既定 0.7）以上の確信度で `no_reply` のときだけ落とす。迷ったら拾う側に倒す。repo 分類の `confidence_threshold` とは問いが違うので分ける。

## 5. 結果ごとの扱い

| 結果 | 動作 |
|---|---|
| 返信必要 | 従来どおり repo 解決 → `task/submit` |
| 返信不要 | アプリ DM を送って終了 |
| 判定失敗（API エラー・401/403・読めない応答） | 中断してアプリ DM で知らせる。401/403 と到達不能（transport・timeout・5xx）なら `plugin/degraded` key `llm` を active にする（読めない応答はゲートウェイが答えた証拠なので解除側。→ 決定 7） |

返信不要・失敗のどちらでも、**重複防止キーを戻す**。リアクション起動は判定を通らないので、本人がリアクションを付ければ必ずタスクになる。これが誤判定と障害の両方の回復手段である。アプリ DM は ADR-0021 のとおり通知専用で、ボタンは置かない。Gateway のレコードは従来どおり ack する（回復はリアクションで行う）。

DM には送信者・理由・本文の抜粋（先頭数行の引用）・「:絵文字: を付けるとタスクになります」・スレッドリンクを載せる。理由は chat ならスレッドと同じ言語の 1 文、decisions なら「返信不要 84%」の確率表示。案内はリアクション workflow がある構成のときだけ出す。`bot_token` が無ければ DM は出ずログだけになる。

## 6. 設定なしで常に有効。メンションに答えるなら [llm] は必須

メンションに答える workflow（`mention = true` のルート）があれば、Slack プラグインは `[slack.llm]` か core から供給された `[llm]` を必須にし、どちらも無ければ `initialize` を `CONFIG_INVALID` にする。全メンションが黙って中断される状態では起動させない。リアクションとチャンネル監視だけの構成は判定しないので、従来どおり候補が 2 件以上のときだけ要る。

## 7. 仕組み

- **対象の印**: `Mention.reply_check` はメンション判定表を通ったときだけ立つ（リアクションとチャンネル監視が作る `Mention` は偽）。`Mention.from_bot` は `bot_id` の有無
- **キーを戻す**: 重複防止の集合（`MentionFilter`）はイベントループが持っている。判定は spawn した `handle_mention` の中で走るので、そこからチャネルでキーをループへ送り、ループが `readmit_for_reaction` する。**キーは処理済みのまま残し、リアクションにだけ 1 回の再取得を許す** —— 集合から消すと、チャンネル監視が履歴を読み直したとき（起動時バックフィル・Gateway のポーリング）に、落としたメンションが bot 名義のタスクとして戻ってくる。メンションの再配送も重複のまま
- **bot の投稿の救い方**: リアクション起動は `from_bot` がその bot を名指す trigger でしか bot の投稿を受けない（ADR-0079）。DM はその bot を受ける絵文字だけを案内し（`ReactionTriggers::emoji_for`、`Mention.bot_id` で判定）、無ければ `from_bot` に足すよう案内する。**bot の投稿は、その設定が無い限りリアクションでは救えない** —— 決定 5 の「必ずタスク化できる」の例外
- **健全性**: 返信要否判定とリポジトリ分類は 1 本の LLM トランスポートを共有している。それを `llm::Reported` で包み、401/403 と到達不能を `plugin/degraded` key `llm` で報告し、それ以外の答えで消す。呼び出し側が報告を忘れる余地を作らない
- **判定中のリアクション**: 判定の LLM 呼び出し中（数秒）に本人がリアクションを付けると、その時点ではキーが使用中なので無視される。その後に返信不要で落ちた場合は付け直しが要る。窓は判定の往復ぶんで、防ぐには「判定中のメッセージ」を別に追う状態が要るため、入れていない
- **DM の本文**: 抜粋は最大 3 行・200 文字で、`<` `>` `&` をエスケープする。`<@U…>` が DM の中で生きたメンションにならないため。**見出しの送信者名・理由・エラー文も同じくエスケープし、200 文字で切る** —— 表示名は相手が決められ、chat の理由は読んだメンションをなぞりうるので、細工したメンションがアプリ自身の DM に `<https://…|ラベル>` のリンクを置けてしまうため

# Consequences

- **破壊的変更**: リポジトリ候補が 1 件で `[llm]` を書いていなかった構成は起動しなくなる。既存の利用者も、更新した時点で返信不要のメンションがタスクにならなくなる
- メンション 1 件ごとに LLM 呼び出しが 1 回増える（新規会話で候補が複数なら 2 回）
- 返信不要で落としたメッセージは orchestrator に何も残らない。同じスレッドの後続メンションは新しい会話として扱われ、スレッド文脈には落としたメッセージも含まれる

# Alternatives considered

| 案 | 不採用の理由 |
|---|---|
| workflow ごとの opt-in（`options.reply_check`） | 運用者は全メンションに効かせたい。opt-in にすると設定を忘れた構成で効かない |
| 判定失敗なら返信必要として続行 | 運用者の判断で、障害中に返信案を作り続けるより止めて知らせる方を選んだ |
| bot 投稿は LLM を通さず一律に返信不要 | 人間の依頼を bot が代理投稿する境界例を落とし、ADR-0109 で bot のメンションを受けるようにした意味がなくなる |
| repo 分類と 1 回の呼び出しにまとめる | 返信不要のときも repo 分類分の入力トークンを払い、クレートに複数問の API が要る |
