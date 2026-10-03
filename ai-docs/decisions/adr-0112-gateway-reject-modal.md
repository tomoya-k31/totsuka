---
type: Decision
title: ADR-0112 Event Gateway が却下モーダルを開き、その送信を中継する
description: Event Gateway 構成でも却下モーダル（ADR-0111）を使えるよう、ゲートウェイに任意のボットトークンを持たせて却下の押下で views.open を呼び、開けたら押下は publish せず、view_submission を新しいレコード種別として押下用トピックへ中継する決定。ADR-0072 の「クラウドに置く資格情報は signing secret だけ」「レコードに本文を載せない」を、この 1 経路に限って改める。開けなければ従来どおり押下を publish し、その場で却下する。
resource: https://github.com/tomoya-k31/totsuka/blob/main/services/slack-event-gateway/src/modal.rs
tags: [decision, slack, gateway, modal, approval, security, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-03T21:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[ADR-0111](/decisions/adr-0111-reject-modal-alt-reply.md) の却下モーダルを Event Gateway 構成に広げる。[ADR-0072](/decisions/adr-0072-slack-event-gateway.md) の決定 1（クラウドの資格情報）と決定 4（本文を保存しない）を、下記の範囲に限って改める。

# Context

モーダルは押下の `trigger_id` でしか開けず、`trigger_id` は押下から **3 秒**で失効する。Gateway 構成では押下が Slack → Cloud Run → Pub/Sub → totsuka と渡り、totsuka の pull は空振りのたびに最大 20 秒まで間隔を広げる（`gateway.rs` の `backoff_max`）。**totsuka がモーダルを開くことは原理的に間に合わない。** 3 秒以内に押下を見ているのはゲートウェイだけである。

加えて、モーダルの送信（`view_submission`）はゲートウェイの射影対象に無く、totsuka に届く経路が無かった。

# Decision

## 1. ゲートウェイが任意のボットトークンで `views.open` を呼ぶ

登録表の行に `bot_token`（任意）を足す。ある行で `reject_reply` の押下が届き、`trigger_id` とボタン値（`{"d","c","ts"}`）が揃っていれば、ゲートウェイがそのトークンで却下モーダルを開く。**開けたら押下は publish しない。** publish すると totsuka がその場で却下してしまい、入力を待てないからである。モーダルの `private_metadata` はボタン値に押下の `response_url` を `r` として足したもので、プラグインが自分で開くモーダル（ADR-0111）と同じ形にしてある。

`bot_token` が無い行、`views.open` の失敗、1 秒（`MODAL_BUDGET`）の超過では、従来どおり押下を publish し、totsuka がその場で却下する。**失うのは代わりの返信だけで、却下は失わない**（ADR-0111 決定 2 と同じ）。1 秒にしたのは、退避の publish を Slack の約 3 秒の ack 窓に収めるためである。

ゲートウェイのモーダルには却下する返信案の引用を出せない。エフェメラル上の押下には元メッセージの写しが含まれないからである。

## 2. `view_submission` を新しいレコード種別として中継する

`callback_id = reject_reply_modal` の送信だけを `kind = "view_submission"` として**押下用トピック**へ publish する。ほかのモーダルの送信は捨てる。

| 項目 | 中身 |
|---|---|
| `channel` / `ts` | `private_metadata` の `c` / `ts`（下書きのスレッド。送信自体は会話の座標を持たない） |
| `value` | `private_metadata` そのまま |
| `response_url` | `private_metadata` の `r` |
| `view_id` | Slack の `view.id`。配送同一性 `view_submission:{view_id}` |
| `alt_text` | 運用者が書いた代わりの返信 |
| `send_alt` | 「スレッドにも送信する」にチェックがあったか |

`v` は 1 のまま。古い totsuka は未知の `kind` を不正なレコードとして捨てるので、その間は「モーダルを送っても何も起きない」状態になる（ゲートウェイと totsuka は同じリリースで更新する）。押下用トピックに載せるのは、`response_url` の寿命（約 30 分）に合わせた保持期間（ADR-0072 決定 5）がそのまま当てはまるからである。

totsuka 側は、受けたレコードを Socket Mode と同じ `view_submission` ペイロードに組み直し、ADR-0111 の `handle_view_submission` に渡す。処理は 1 本で、経路による分岐は無い。

## 3. ADR-0072 から改める点

| ADR-0072 | 改めた後 |
|---|---|
| 決定 1: クラウドに置く資格情報は signing secret だけ | **任意で**ボットトークン（`xoxb-`）も置く。ユーザートークン（`xoxp-`、本人名義で投稿できる）は引き続き置かない。ボットトークンでできるのはボット名義の操作だけで、ゲートウェイが使うのは `views.open` のみ |
| 決定 4: レコードに本文を載せない | `view_submission` の `alt_text` だけは自由記述を載せる。**他人の発言ではなく、運用者本人がこの経路に渡すつもりで書いた文面**であり、7 日ではなく押下用トピックの短い保持期間に置かれる |

`bot_token` を書かなければ、ゲートウェイの振る舞いも資格情報も ADR-0072 のままである。

# Consequences

- ボットトークンが登録表（Secret Manager）と OpenTofu の state に入る。漏れた場合の影響はボット名義の範囲（ナッジ DM と同じ権限）に留まる
- ゲートウェイの外向き通信が Pub/Sub だけでなくなる。`views.open` に渡すのは押下自身の座標から作ったモーダルだけで、メッセージ本文は渡さない
- モーダルの `callback_id` / `block_id` / `action_id` が、ゲートウェイとプラグインの間の契約になる。適合スイートの `view-submission-*` が固定する
- Cloud Run のコールドスタートが長引くと 3 秒に間に合わず、その押下は従来どおりその場で却下になる
- **既知の競合**: `views.open` が 1 秒の打ち切り後に Slack 側では成功していた場合、モーダルは開くが押下も publish され、totsuka がその場で却下する。後から届いた送信は「決定済み」の経路に入り、代わりの返信は記録されない（送信チェックがあっても投稿されない）。打ち切りを延ばすと退避の publish が ack 窓に収まらなくなるので、1 秒のまま受け入れる
