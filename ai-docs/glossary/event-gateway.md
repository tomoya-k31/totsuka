---
type: Term
title: Event Gateway（イベントゲートウェイ）
description: "Slack の配信を HTTPS で受け、本文を保存せずに座標へ射影して Pub/Sub へ流す、totsuka の外で動く常駐しないサービス。event_source = \"gateway\" のときだけ経路に入る。totsuka が止まっている間もイベントが失われず、Slack が購読を自動で無効化することも起きない。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/slack-event-gateway
tags: [glossary, slack, gateway, pubsub, cloud-run, event-source]
generated: { by: claude-code/opus-5, at: 2026-09-14T01:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 定義

Slack の配信を **HTTPS の Request URL で受け**、署名を検証し、**本文を保存せずに座標へ射影して**
Pub/Sub へ publish するサービス。totsuka は起動しているあいだにそのトピックを引く
（[ADR-0072](/decisions/adr-0072-slack-event-gateway.md)）。

`[slack] event_source = "gateway"` を選んだときだけ経路に入る。既定の `"socket"` では存在しないのと同じである。

# なぜあるのか

Socket Mode では **`totsuka run` のプロセス自身が WebSocket を握る**。したがって totsuka が
止まっている間、Slack は配信できず、そのメンションは**取り戻す手段なく失われる**。さらに
配信試行の失敗が続くと、**Slack はそのアプリのイベント購読を自動で無効化する**（60 分の配信試行の
95% 超が失敗したアプリ）。復旧は Slack の設定画面での手作業で、**無効化されたことを totsuka 側から
知る方法は API に無い**。

ゲートウェイは「常時稼働するプロセス」を足して解決するのではなく、**Slack の配信先そのものを
HTTP に変える**。Slack から見た配信は常に成功し、totsuka は好きなときに起動すればよくなる。

# 性質

| | |
|---|---|
| 常駐しない | Cloud Run の scale-to-zero。リクエストが無い間は動いていない |
| 本文を持たない | 本文に対してやってよいのは定数文字列との一致判定だけ。保存も転送もログ出力もしない。**レコードのスキーマに本文フィールドが無いことは保証ではなく**、テストで守る振る舞いである |
| totsuka の管理外 | 利用者のクラウドで動き、フォークして自前実装してよい。合意は `contracts/slack-event-gateway/` の**適合テストスイート**だけである |
| 関門はコンテナの中にしかない | Slack は IAM プリンシパルになれず、安定した送信元 IP の一覧も公開していない。推測不能なパス・署名・5 分のタイムスタンプ窓の 3 つが全部である |

# 混同しやすいもの

- **Socket Mode のリレーではない。** 中継プロセスを置く案は採らず、配信経路そのものを替えた。
  Socket Mode と Request URL は**Slack アプリ単位で排他**なので、両立しない
- **フォールバックは無い。** 上の排他性ゆえに「ゲートウェイが落ちたら Socket Mode に戻る」は
  実現しようがない（切り替えは Slack アプリの manifest 変更を伴う）
- **チャンネル監視はここを通らない。** publish 対象は「自分に関係しうるもの」に絞ってあるので、
  監視チャンネルへの（メンションを含まない）投稿は流れてこない。Gateway 方式では
  `conversations.history` の定期ポーリングで拾う（[チャンネル監視トリガ](/glossary/channel-watch.md)）

# 関連

- [Event Gateway 構築手順](/operations/event-gateway-setup.md)
- [slack-event-gateway](/components/slack-event-gateway.md)
- [ADR-0072](/decisions/adr-0072-slack-event-gateway.md)
