---
type: Service
title: slack-event-gateway
description: Slack の配信を HTTPS で受け、署名を検証し、本文を保存せずに座標へ射影して Pub/Sub へ publish する常駐しないサービス。event_source = "gateway" のときだけ経路に入る。同一リポジトリの workspace 外に置き、適合テストスイートだけを totsuka と共有する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/slack-event-gateway
tags: [rust, service, slack, gateway, cloud-run, pubsub, hmac, security]
generated: { by: claude-code/opus-5, at: 2026-09-13T19:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 何のためにあるか

Socket Mode では `totsuka run` のプロセス自身が WebSocket を握る。したがって
**totsuka が止まっている間のメンションは失われ**、配信失敗が続けば Slack が
イベント購読そのものを無効化する（復旧は Slack の設定画面での手作業）。
これが #652 の Problem であり、[ADR-0072](/decisions/adr-0072-slack-event-gateway.md)
はこれを「中継プロセスを足す」のではなく **Slack の配信経路を HTTP Request URL に
切り替える**ことで解いた。その HTTP を受けるのがこのサービスである。

`event_source = "socket"` の利用者にとっては存在しないのと同じで、経路に入らない。

# 絶対に守る制約

**本文を保存しない・外部送信しない・ログに出さない。** 本文に対してやってよいのは
定数文字列との一致判定だけで、メッセージを解釈するための外部 API 呼び出し（LLM を
含む）を行わない。外向きの通信は Pub/Sub への publish 1 本だけで、それがこのプロセスの
目的である。

**レコードのスキーマに本文フィールドが無いことは、この振る舞いを保証しない** ——
プロセスは文字列をメモリに保持することも、出力することも、別の宛先へ送ることもできる。
保証はコードと `tests/http.rs` が固定する振る舞いのほうである（応答が本文を反映しないこと、
publish されたレコードが本文を含まないこと）。

# 関門は 3 つで、すべてコンテナの中にある

Slack は IAM プリンシパルになれず、許可リストに使える**安定した送信元 IP の一覧を公開して
いない** —— 推測で組めば配信を落とし始め、それはこの設計が直そうとしている失敗そのものに
なる。したがってコンテナの手前に層は無い（ADR-0072 決定 11）。

| 関門 | 実装 |
|---|---|
| 推測不能なパス | `/slack/e/<opaque-token>` を利用者ごとに 1 本。`registry::Registry::lookup` が**定数時間比較で全行を走査**する（応答時間から当たった接頭辞を絞られないため） |
| 署名 | 生のボディに対する HMAC-SHA256。**定数時間比較**（`subtle::ConstantTimeEq`）。ボディはパースする前に検証する —— 再シリアライズしたものを検証すると、署名されていない入力で検査が通る |
| タイムスタンプの窓 | ローカル時刻から前後 5 分。署名は「Slack から来たこと」しか証明せず、キャプチャされたリクエストが永久に使えるのを止めるのは窓だけである |

**未登録のパスと署名不正には同じ 404 / 401 の区別しかなく、どちらが原因かは応答から
分からない。** 「そのトークンは存在するが署名が違う」と教えると、パスが総当たりする価値の
あるものになる。

# 200 を返してよいのは publish が受理された後だけ

先に 200 を返せばイベントは**永久に失われる**（Slack は 200 を配信成功とみなし再送しない）。
逆に失敗を返すのも無料ではない —— 失敗の累積こそが購読の自動無効化の条件で、それがこの
ADR が消そうとしている病気である。したがって順序は publish → 応答で、再試行は Slack の
ack 期限（約 3 秒）の中に収める（`publish::PUBLISH_BUDGET`）。

**メンションでないメッセージにも 200 を返す。** 何も publish しないが、非 200 は
「配信失敗」として数えられるためである。

# Request URL は 2 つ要る

Event Subscriptions の Request URL（メンション・リアクション）と、**Interactivity &
Shortcuts の Request URL**（`block_actions`）は Slack アプリの別の設定項目である。
Socket Mode ではどちらも同じ WebSocket で届いていたので区別が要らなかった。
**片方だけ向けると、メンションは動いたまま承認・リポジトリ選択のボタンだけが一切届かない。**

後者は `application/x-www-form-urlencoded` の `payload` に JSON が入る別形式で、
JSON として読むと全押下がパースエラーになる（症状は「ボタンが壊れている」に見える）。

# モジュール構成

| モジュール | 責務 |
|---|---|
| `registry` | 登録表（`path_token` → signing secret / slack user id / トピック 2 本）。**リポジトリには入らない** —— 利用者の Secret Manager にあり、マウントファイルか環境変数で届く。起動時に、空の表・`path_token` 重複・空フィールド・トピック 2 本が同名、を拒否する（どれも実行時の症状が「無言で動かない」もの） |
| `signature` | Slack の署名方式（`v0:{ts}:{body}` の HMAC-SHA256）と 5 分の窓。`verify` が定数時間比較を使っていることはテストがソースに対して固定する |
| `project` | 生の配信 → publish されるレコード。**totsuka 側の `gateway_contract::project` と独立した実装**で、両者を突き合わせるのが適合スイート。`mentions_user` / `extract_subteam_ids` / `delivery_id` / `decode_interactivity_payload` を持つ |
| `publish` | Pub/Sub REST への publish と、メタデータサーバからのトークン取得。**サービスアカウントキーは存在しない**（Cloud Run のリビジョンの SA でトークンが降ってくる）。base64 エンコードは自前 |
| `http` | 経路・検証・射影・publish の順序と、応答の組み立て。`received_at` の RFC 3339 生成も自前（日付ライブラリを 1 つも足さないため） |

# 置き場と、totsuka との関係

**同一リポジトリの workspace 外**（ADR-0072 決定 9）。ルート `Cargo.toml` の
`[workspace] exclude` で外している —— `plugins/` は arch-lint が「totsuka プラグインで
あること」を要求し、`crates/` に入れると全員の `cargo build --workspace` に乗るためである。
代償として totsuka と型を共有できないので、合意は `contracts/slack-event-gateway/` の
**適合テストスイート**だけになる（[ADR-0072](/decisions/adr-0072-slack-event-gateway.md) 決定 7）。
**このゲートウェイをフォークした実装も、スイートを通せば適合している。**

CI は `ci.yml` の `gateway` ジョブ 1 本（fmt / clippy / test）。`--workspace` は除外
ディレクトリに届かないので、これが無いと**このコードには CI が一切かからない**。

# 関連

- [ADR-0072](/decisions/adr-0072-slack-event-gateway.md) — 設計判断の正本
- [task-source-slack プラグイン](/components/task-source-slack.md) — 消費側（`gateway` モジュール）
- [設定リファレンス](/development/config-reference.md) — `event_source` と `[slack.gateway]`
