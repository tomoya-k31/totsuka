---
type: Decision
title: ADR-0068 チャンネル監視トリガと「操作者本人のみ」不変条件の明示的緩和
description: "チャンネルへの投稿そのものをトリガにする trigger = { channel = … } を導入する決定。リアクショントリガが ADR を要求していた「操作者本人のみ」不変条件は維持しつつ、from allowlist を唯一の明示的な緩和口として認める。id + channel_name 併記・trigger.repo 固定・1 投稿 = 1 タスク（message_key なし）・カーソルなし起動時 backfill（N 件 + 年齢上限）もここで決定。判定順はメンション優先。"
resource: https://github.com/tomoya-k31/totsuka/issues/615
tags: [decision, trigger, channel-watch, security, plugin-sdk, slack, discord, adr]
generated: { by: claude-code/opus-5, at: 2026-09-06T06:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: slack-socket-mode
    resource: https://docs.slack.dev/apis/events-api/using-socket-mode
    title: Slack — Using Socket Mode（切断中のイベント損失の明言）
  - id: discord-gateway
    resource: https://docs.discord.com/developers/events/gateway
    title: Discord — Gateway（RESUME 窓外は再生不可）
---

# Status

stable。#616（plugin-sdk）で語彙を実装し、#617（slack）と #618（discord）で消費側を実装した。**実機検証は未了**。

# Context

`plugins/task-source-slack/src/reaction.rs` は「**操作者本人のリアクションしか受け付けない**」を不変条件として明文化し、「緩めるには設定キーではなく ADR が要る」と書いている。理由: 他人がジェスチャ 1 つで操作者のマシン上の実行を起こせるのは、名前が違うだけのリモート実行だから。

チャンネル監視トリガ（#615、`clip` ユースケース）は定義上この境界に触る。「チャンネルに**投稿すること**」が起動ジェスチャの全部であり、チャンネルに投稿できるのは操作者だけではない。しかも clip フローは他人が貼った任意 URL の内容をエージェントに処理させ、リポジトリへ書き込むため、攻撃面はリアクションより広い。

# Decision

## 1. 不変条件は維持し、`from` を唯一の緩和口にする

既定では**操作者本人の投稿だけ**がトリガになる（リアクショントリガと同じ境界）。緩和は
`trigger = { …, from = ["<user id>", …] }` という**明示的な allowlist だけ**で行い、
「チャンネル参加者全員」を意味する設定は存在させない。チャンネルの招待権限をそのまま
実行権限に変換しない — 実行を許す相手は config に名前で書かれる。

- 操作者は `from` に関係なく**常に**許可される。`from` は拡張であって置換ではない（操作者を締め出せる allowlist に用途が無い）
- `from = []` は「誰も追加しない」なので拒否する（書いた人は何かを意図している）
- id は**完全一致**で比較する。プラットフォーム発行の id であり、人間が打つ login ではないので、大文字小文字の同一視は一致範囲を広げるだけで正当な入力を救わない

## 2. チャンネルは id が正、`channel_name` 併記を必須にする

名前は両プラットフォームとも改名自由で、名前指定は「黙って別チャンネルにマッチ」の再発形になる（スコープ欠落が無症状・ボードの番号だけ同一視、と同型）。逆に id だけでは設定が読めない。よって **id を正**とし、**名前の併記を必須**にして、起動時に実名と照合して不一致を警告する。冗長だから要るのであって、冗長なのに要るのではない。

## 3. 1 投稿 = 1 タスク、`message_key` なし

`id = "{channel}:{ts|message_id}"`、`message_key` は付けない。2 回目の配送は台帳（`IngestOutcome::Duplicate`）で止まり、恒久 at-most-once になる。会話継続（1 スレッド = 1 会話 = 1 タスク）の**対象外**。スレッド返信は監視しない — 生成ドキュメントへの「ありがとう」で 2 本目が走る事故を構造的に消す。

## 4. backfill はカーソルなし、「N 件 + 年齢上限」

両プラットフォームとも切断中のイベントは失われ（Slack は公式に "you may lose events"[^slack-socket-mode]、Discord は RESUME 窓外で再生不可[^discord-gateway]）、回復手段は REST の履歴取得だけ。3 の決定により**重複送信は台帳が無害化する**ので、起動時に「直近 N 件（既定 100）かつ年齢上限（既定 24h）以内」を毎回無条件に送る。永続カーソルは持たない — 取りすぎは無害・取り足りないと投稿が黙って消える非対称では、過剰側に倒して状態ファイルを 1 つも増やさないのが正しい。年齢上限は、履歴のある既存チャンネルを初回指定したときの洪水を有界化する（上限なしだと直近 N 件の過去投稿が全部タスク化される）。

## 5. 判定順はメンション優先

監視チャンネル内の投稿がメンションも含む場合（`from` で他人に開いたときだけ起きる）、**メンショントリガが勝つ**。明示的な名指しは監視より強い、という利用者の決定。実装は既存のメンション判定が `None` に落とした message を watch 分岐が拾う順序で、1 メッセージ = 最大 1 タスク（F-81）は保たれる。

## 6. `repo` はトリガに固定で書く

`trigger.repo` を必須にし、`initialize` の `repositories` に無い名前は起動時に拒否する。監視チャンネルは「ここに貼ったものは あのリポジトリ」という固定対応が自然で、LLM 分類に毎回同じ答えを出させる意味が無い。

## 7. Discord 側は「監視 + 結果投稿」だけの薄いソースにする

Slack の重心（本人名義の返信・下書き・承認ゲート）は **Discord では成立しない** —— self-bot 禁止によりアプリは bot 名義でしか投稿できない。bot の声のまわりに承認フローを組み直すと機構だけ残って理由が消えるので、[task-source-discord](/components/task-source-discord.md) はチャンネル監視の取り込みと結果投稿だけを行う。

実装で確定した Discord 固有の事実（#618）:

- **close code 4014（privileged intent 未許可）は恒久エラー**。Discord はハンドシェイクを失敗させず**ソケットを閉じる**ので、再接続し続けるとログ上は回線不調と見分けがつかない。止めて、Developer Portal のトグルまで案内する
- **intent が off でも Gateway は繋がる**。その場合 `content` が空文字で配送され、**エラーは何も出ない** —— 「タスクは起票されるが本文が空」がその症状
- **スレッドはチャンネル**なので、返信はスレッド自身の `channel_id` を持つ。Slack で明示的な判定行が要る「スレッド返信を除く」は、Discord では構造的に成立する
- **webhook 投稿の author には `bot` フラグが無い**。bot フラグだけを見ると通り抜けるので、`webhook_id` の有無も併せて見る
- 認証スキームは **`Bearer` ではなく `Bot`**。間違えると 401 になるだけで、語が原因だとは分からない
- バックフィルの下限は **合成 snowflake**（snowflake が生成時刻を内包するので、余分な往復なしに「now − max_age」を表せる）

# Consequences

- 実行権限の境界が config の `from` 1 箇所に集まり、レビューで読める
- `from` に人を足した瞬間、その人の投稿（と、その中の任意 URL の内容）が操作者のマシンで処理される。これは本 ADR が明示的に許した唯一の経路であり、既定構成では発生しない
- 監視ワークフローの成果物投稿は bot 名義（Slack も `xoxb-`、Discord は bot しかない）。本人名義の自動投稿を clip に許すと、承認ゲートが防いでいた形を別口で再導入することになる（実装の詳細は #617 / #618）
- 語彙とゲートは plugin-sdk（`watch.rs`）にあり、slack / discord は同じ検証文言・同じ境界を共有する

# 不採用案

- **「チャンネル参加者全員を許可」設定** — チャンネル管理者の招待操作がそのまま実行権限になる。境界が totsuka の外に出る
- **名前だけ / id だけのチャンネル指定** — 前者は改名で黙って壊れ、後者は設定が読めない
- **永続カーソルでの厳密 backfill** — 状態ファイルと壊れ方のパターンが増える。冪等な台帳がある以上、厳密さが買うものが無い
- **`message_key` 付与（スレッドでの追加指示）** — backfill が再起動のたびに会話を再開しうる形になり、4 の「無条件送信が無害」が崩れる

# 関連

- [ADR-0025 リアクショントリガ](/decisions/adr-0025-reaction-task-trigger.md) — 維持される不変条件の出所
- [会話継続](/glossary/conversation-continuity.md) — 本トリガが対象外とする同一性モデル
- [plugin-sdk](/components/plugin-sdk.md) — 実装の置き場所

[^slack-socket-mode]: Slack — Using Socket Mode（切断中のイベント損失の明言）
[^discord-gateway]: Discord — Gateway（RESUME 窓外は再生不可）
