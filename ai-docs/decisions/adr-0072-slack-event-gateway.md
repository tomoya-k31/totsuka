---
type: Decision
title: ADR-0072 Slack イベント受信を Events API + Cloud Run + Pub/Sub へ移譲する
description: "totsuka 停止中の取りこぼしと Slack による購読の自動無効化を、Socket Mode リレーではなく Events API への転換で解決する決定。常時稼働ホストを持たない Cloud Run scale-to-zero + Pub/Sub 構成とし、プールには座標と文字列判定フラグだけを書いて本文を保存しない。Socket Mode は event_source で併存させ、保持は Pub/Sub 7 日・起票窓は totsuka 側。複数人は利用者ごとのパスとトピックで分離し、クラウドに置く資格情報は signing secret のみ。受け口は workspace 外の同居プロジェクトとして公式イメージを配る。グループメンション対応とスキーマ契約もここで決定。"
resource: https://github.com/tomoya-k31/totsuka/issues/652
tags: [decision, slack, gcp, cloud-run, pubsub, event-delivery, cost, multi-tenant, adr]
generated: { by: claude-code/opus-5, at: 2026-09-12T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: slack-events-api
    resource: https://docs.slack.dev/apis/events-api/
    title: Slack — The Events API（Socket Mode と Request URL は排他）
  - id: slack-rate-limit-2025
    resource: https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps/
    title: Slack — Rate limit changes for non-Marketplace apps
  - id: cloud-run-pricing
    resource: https://cloud.google.com/run/pricing
    title: Google Cloud — Cloud Run pricing
  - id: cloud-run-websockets
    resource: https://docs.cloud.google.com/run/docs/triggering/websockets
    title: Google Cloud — Using WebSockets with Cloud Run
  - id: pubsub-pricing
    resource: https://cloud.google.com/pubsub/pricing
    title: Google Cloud — Pub/Sub pricing
  - id: gce-free-tier
    resource: https://cloud.google.com/free/docs/compute-getting-started
    title: Google Cloud — Compute Engine 無料枠
  - id: oci-always-free
    resource: https://docs.oracle.com/en-us/iaas/Content/FreeTier/freetier_topic-Always_Free_Resources.htm
    title: Oracle Cloud — Always Free Resources（アイドル回収規定）
---

# Status

stable。設計判断は確定済み。実装は未着手で、[#652](https://github.com/tomoya-k31/totsuka/issues/652) の子 issue に分割する。

**前提条件が 1 つ未検証のまま残っている。** Slack は GCP の IAM 認証を喋れないため Cloud Run を `allUsers` で公開する必要があるが、会社の GCP 組織でドメイン制限共有ポリシー（`constraints/iam.allowedPolicyMemberDomains`）が有効だとこの付与が拒否される。回避策の外部ロードバランサは月 18 ドル程度かかり、本 ADR が前提にしている「月 1 ドル前後」が 20 倍になる。**実装着手前にこの可否を確認すること。**

# Context

`plugins/task-source-slack/src/socket_mode.rs` は Socket Mode の WebSocket を `totsuka run` のプロセス自身が握る。totsuka が動いていない間、Slack はイベントを配信できず、これが 2 つの形で表面化する。

**停止中のメンションは完全に失われる。** 起動しても取り戻す手段が無い。チャンネル監視だけは ADR-0068 の backfill が埋めるが、メンション経路・リアクション経路には同等の仕組みが無い。

**Slack が購読を自動で無効化する（実際に踏んだ）。** ノート PC 運用では夜間・週末・出張のあいだプロセスが止まり、その間の配信試行はすべて失敗として数えられる。「1 時間あたり 1,000 イベント未満なら無効化しない」という免除枠はこの構成を守らない。`manifest.yml` は user event として `message.channels` / `message.groups` を購読しており、対象は**運用者が参加している全チャンネルの全メッセージ**なので、平日日中は容易に 1,000/h を超える。さらに、無効化されたことを読み返す API が存在しないため、totsuka 側から検知できない。`socket_mode.rs` の `eventless_warning` が唯一の症状ベースの手掛かりで、見ていなければ気づかない。

運用形態の前提として、個人 Slack は常駐マシンで動かすが、会社 Slack を見るマシンは止まることがある。両者は**別ワークスペース**であり、別の Slack アプリである。

# Decision

## 1. Socket Mode リレーではなく Events API への転換を採る

issue の当初案は常時稼働ホストに Socket Mode の中継プロセスを置くものだった。これを採らず、**Slack アプリの配信経路そのものを HTTP Request URL に切り替える**。

Socket Mode と Request URL は Slack アプリ単位で排他であり、どちらかを選ぶ[^slack-events-api]。HTTP を選べば**常時 WebSocket を握るプロセスが要らなくなる**。これが本 ADR の中心で、以下の帰結がすべてここから出る。

- 常時稼働ホストがゼロ台になる（費用・運用・監視の対象が消える）
- 受け口は scale-to-zero できるので、イベントが来ていない間の課金が無い
- クラウドに置く資格情報が App-Level Token（`xapp-`）から **signing secret だけ**に変わる
- リレー案が最重要視していた「二重接続でイベントが振り分けられる」危険が、構造的に存在しなくなる

代償は公開受信口を作ることで、issue が掲げていた「公開の受信口を作らない」という前提を意図的に破棄する。守り方は HMAC 署名検証であり、これは Slack 自身が標準として用意している経路である。

## 2. Socket Mode は `event_source` で残す

`[slack] event_source = "socket" | "gcp_pubsub"`（既定 `socket`）を導入する。排他は Slack アプリ単位なので、個人 Slack のアプリは Socket Mode のまま、会社 Slack のアプリだけ Request URL にでき、totsuka 側はマシンごとに設定を書き分けるだけで済む。

選んだ経路が要求する資格情報だけを必須にする。`gcp_pubsub` では `app_token` は使われないので、必須検証から外す。

**リレー障害時に自動で Socket Mode へ切り替えるフォールバックは作らない。** Slack アプリ設定上そもそも両立不可能であり、切り替えは Slack 側の手作業を伴う明示操作である。

## 3. 常時稼働ホストを持たない構成にする

```text
Slack ──HTTPS POST（署名付き）──> [Cloud Run 受け口] ──> [Pub/Sub]
                                    scale-to-zero          保持 7 日
                                                              │ REST long-poll pull
                                                              ▼
                                                        [totsuka run]
```

totsuka から Slack への送信（返信投稿・リアクション・`response_url` への応答）は**すべて totsuka のプロセスから slack.com への直接通信**で、クラウドは経路に入らない。`xoxp-` / `xoxb-` は手元の Keychain から動かさない。

**Cloud Run で Socket Mode を回す案は採れない。** outbound の常時接続には `min-instances >= 1` と CPU 常時割り当てが要り、待ち時間が課金対象になって月 10 ドル以上のオーダーに変わる。それ以上に決定的なのは、`min-instances = 1` が「常に 1 台だけ」を保証しないことで[^cloud-run-websockets]、デプロイ時の新旧並走やインスタンス置換で 2 台になった瞬間、Slack が各ペイロードをいずれか 1 本にしか送らないためイベントの約半分が無言で消える。

## 4. プールには座標と文字列判定フラグだけを書く

受け口は POST で本文を受け取るが、**本文を保存も転送もログ出力もしない**。やってよいのは定数文字列との一致判定だけで、外部 API への発信を持たない（LLM 呼び出しを含まない）。

経路ごとに本文が要るかを整理すると、本文が必要なのはメンション判定の 1 ビットだけである。

| 経路 | 判定に必要なもの | 本文 |
|---|---|---|
| リアクション | `reaction` / `item.channel` / `item.ts` / `item_user` | 不要 |
| チャンネル監視 | `channel` / `thread_ts` | 不要 |
| メンション | 宛先タグの有無 | この 1 ビットのみ |
| 承認ボタン | `action_id` / `value` / `response_url` / `container.channel_id` | 不要 |

承認ボタンについては `plugins/task-source-slack/src/approval.rs` が読む 4 フィールドだけを射影する。下書き本文を含む `message` ブロックは渡さない。

**この判断は「保存しない」ではなく「保存できない」形にする。** スキーマに本文フィールドを置かないので、受け口の実装を差し替えても本文が漏れる経路が作れない。

## 5. 保持は Pub/Sub 側 7 日、起票する窓は totsuka 側で決める

Pub/Sub のサブスクリプション保持期間は 10 分から 7 日の範囲で設定でき、期限切れの未 ack メッセージは自動的に落ちる。**TTL は設定値ひとつでコードが要らない。**

保持は上限の 7 日を取り、実際に起票する窓は totsuka 側の `drain_max_age_hours`（既定 24）と `drain_limit` で決める。ADR-0068 の `watch_backfill_max_age_hours` / `watch_backfill_limit` と同じ考え方で、名前もそれに揃える。ポリシーが手元にあるので、出張明けに拾いたければ設定を一時的に上げるだけでよく、クラウドの再デプロイが要らない。窓の外のメッセージは ack して捨てる。

`block_actions` は**別トピックにして保持を数分**にする。`response_url` は 30 分で失効するので、停止中に押されたボタンは復帰時には応答先が死んでおり、遅れて処理する意味が無い。

## 6. 複数人は利用者ごとに分離し、Cloud Run は 1 サービスに集約する

Slack アプリは利用者ごとに分ける。1 つのアプリを複数人がインストールする形も Slack は持つが、同じチャンネルに複数の利用者がいるとイベントが 1 通にまとめられ、誰宛かを知るのに `apps.event.authorizations.list` を毎イベント呼ぶ必要が出る。全参加チャンネルの全メッセージにこの追加 API 呼び出しは乗せられない。

ルーティングは**推測不能なパス**で行う。

```text
https://<service>.run.app/slack/e/<opaque-token>   ← 利用者ごとに 1 本
```

受け口はパスから利用者を引き、その利用者の signing secret で検証し、その利用者のトピックへ publish する。本文中の `api_app_id` で引く案は「検証前の本文を信じて鍵を選ぶ」形になるので採らない。

Pub/Sub は**トピック数・サブスクリプション数に課金しない**[^pubsub-pricing]ので、利用者ごとにトピックを分けても費用は増えず、IAM で「自分のサブスクリプションだけ pull できる」分離が効く。1 トピックに集約してサブスクリプションフィルタで振り分ける案は、**フィルタで除外されたメッセージにも delivery 料金がかかる**ため、費用でも分離でも劣る。

Cloud Run を 1 サービスに集約するのは**運用上の選択であって費用上の利点は無い**（サービス数に課金されない）。代償は、1 プロセスが全員の signing secret を持ち全員の本文をメモリ上で扱うことである。分離したくなれば利用者ごとにサービスを分けても費用は変わらない。

totsuka 側の pull は**各利用者の Google アカウントの ADC** を使い、自分のサブスクリプションにだけ `roles/pubsub.subscriber` を付ける。サービスアカウントキーを配らない。

## 7. スキーマを契約とし、totsuka 側を正とする

受け口は利用者の管理下で動くので、totsuka は相手のコードを信用できない。**Pub/Sub メッセージのスキーマを totsuka 側の正**として定義し、ADR-0055 と同じ規律を適用する（実行時は寛容＝未知フィールドを無視、`v` が未知なら安全に拒否してログに出す）。

```json
{
  "v": 1,
  "kind": "message",
  "channel": "C0123",
  "ts": "1757640000.000100",
  "thread_ts": null,
  "user": "U_SENDER",
  "flags": { "mentions_me": true },
  "subteam_ids": ["S0ABCDEF"],
  "received_at": "2026-09-12T04:00:00Z"
}
```

`kind` ごとの追加フィールドは、`reaction` が `reaction` / `item_user`、`block_actions` が `action_id` / `value` / `response_url` / `container_channel` のみ。**本文・`text`・下書きブロックはスキーマに存在しない。**

受け口が workspace 外の独立プロジェクトになる（決定 9）ため型を直接共有できない。リポジトリに JSON fixture を 1 組置き、受け口側のテストと totsuka 側のテストが同じ fixture を読む。どちらかがズレれば CI が落ちる。

## 8. メンションの範囲を個人 + ユーザーグループまで広げる

現在の `mention.rs` は `<@U_ME>` と `<@U_ME|` の 2 パターンだけを見ており、グループメンションを扱っていない。これを**ユーザーグループ（`<!subteam^S…>`）まで**広げる。`<!here>` / `<!channel>` / `<!everyone>` は対象外とする — 名指しではなく「この場にいる人へ」であり、タスク化すると雑音が支配的になる。

**所属判定は totsuka 側に残す。** エッジで真偽値にするには運用者の所属グループ一覧をクラウド設定に置く必要があり、メンバー変更のたびに更新が要り、陳腐化するとメンションを黙って取りこぼす。代わりに受け口は本文から**グループ ID だけを抽出**して `subteam_ids` に載せ、自分が属するかの判定は totsuka が行う。他人個人宛の `<@U_OTHER>` は記録しない。これにより登録表は `パス → (signing secret, user ID, トピック)` のまま増えない。

所属は起動時に `usergroups.list`（`include_users=true`）を 1 回呼んで解決する。メンバー変更は稀で再起動で追従できるため、定期更新は入れない。これは `manifest.yml` の user scope に `usergroups:read` の追加を要求し、**再インストールにより `xoxp-` と `xoxb-` の両方が再発行される**。

対象外と決めた以上 `broadcast` はスキーマに置かない。スキーマは追加に寛容なので、方針が変われば `v: 1` のまま足せる。

## 9. 受け口は同一リポジトリの workspace 外に置き、公式イメージを配る

totsuka は macOS 向けの OSS であり、この機能はオプションである。一方 OpenTofu で構築を自動化するにはコンテナイメージの URL が要り、OpenTofu 単体ではソースからビルドできない。**公式イメージを ghcr.io に出し、OpenTofu の `image` 変数の既定値にする。** 会社利用では自社 Artifact Registry に差し替える。

イメージを配る以上、その中身は totsuka が保守する一級の成果物になる。ソース・`Dockerfile`・ghcr への push ワークフローはこのリポジトリに入る。GCP プロジェクト ID・Slack アプリ ID・signing secret・登録表の中身は入らない（利用者の Secret Manager にある）。

**ルート `Cargo.toml` の `exclude` で workspace から外す。** `plugins/` 配下は arch-lint が `plugin.toml` と一致する bin をちょうど 1 つ持つことを要求するので置けず、`crates/` のメンバーにすると全員の `cargo build --workspace` に乗る。`ci.yml` にそのディレクトリ専用の `fmt` / `clippy` / `test` ジョブを足す。ベースイメージは distroless か scratch にして、増える脆弱性対応の面を最小化する。

## 10. 実費の前提

tier 1 の公開レート（CPU 0.000024 ドル/vCPU 秒、メモリ 0.0000025 ドル/GiB 秒、リクエスト 0.40 ドル/100 万）[^cloud-run-pricing]で、0.25 vCPU・512MiB・1 リクエスト 100ms として見積もる。東京は tier 2 で約 1.4 倍。

| 月間イベント数 | Cloud Run（東京） | Pub/Sub | 合計 |
|---|---|---|---|
| 15 万件 | 0.17 ドル | 0.02 ドル | 約 0.2 ドル |
| 60 万件 | 0.68 ドル | 0.07 ドル | 約 0.8 ドル |
| 150 万件 | 1.7 ドル | 0.17 ドル | 約 2 ドル |

Secret Manager は 6 バージョンまで無料（以降 0.06 ドル/本/月）。Pub/Sub の保管は最初の 24 時間が無料で、座標だけなら 7 日分溜めても月 0.01 ドル未満。**`min-instances` を 0 に保つことがこの金額の唯一の前提**であり、1 以上にした瞬間に桁が変わる。

# Consequences

- **停止中のイベントが失われなくなり、購読の自動無効化が構造的に起きなくなる。** 受け口が常に 200 を返すため、配信失敗が積み上がらない。
- **クラウドに置く資格情報が signing secret だけになる。** 本人名義で投稿できるトークンを外部ホストに置かずに済むという、リレー案が目指した性質をより強く満たす。
- **drain 前に削除・編集されたメッセージは復元できない。** 座標のみ保存の代償として受け入れる。
- **受け口の停止は元の病気を呼び戻す。** Cloud Run の可用性はノート PC より桁違いに高いが、不良デプロイで 60 分間 95% 失敗すれば購読は無効化される。段階的リビジョン移行を運用手順に含める。
- **1 サービス集約は全員の signing secret と本文をひとつのプロセスに集める。** 費用上の利点は無いので、分離が必要になったらいつでも分けられる。
- **`conversations.history` のレート制限に依存が生まれる。** 社内アプリなので Tier 3 が維持されるが、Marketplace 外に配布した瞬間 1 リクエスト/分になる[^slack-rate-limit-2025]。配布形態を変えるときの制約として記録する。
- **Slack アプリの再インストールが要る。** Request URL への切り替えと `usergroups:read` の追加で、`xoxp-` と `xoxb-` が再発行される。
- **監視対象が増える。** コンテナのベースイメージが `cargo audit` / `cargo deny` の外側の新しい脆弱性対応対象になる。

# 不採用案

**Socket Mode リレーを常時稼働 VM に置く（issue の当初案）。** GCE の Always Free `e2-micro` なら外部 IP を含めて 0 ドルで動く[^gce-free-tier]が、us-west1 / us-central1 / us-east1 限定で、VM 1 台の運用（OS 更新・再起動復帰・死活監視）が増える。Events API への転換がホストそのものを消せる以上、常時稼働を維持する理由が無い。

**Oracle Cloud の Always Free。** 東京リージョンが使えて魅力的に見えるが、7 日連続で CPU・ネットワーク・メモリの利用率が閾値を下回るとアイドル判定で回収される規定がある[^oci-always-free]。「WebSocket を 1 本張って待つだけ」のリレーはこの条件にほぼ確実に該当する。

**1 トピックにサブスクリプションフィルタで振り分ける。** フィルタで除外されたメッセージにも delivery 料金がかかり、IAM による利用者間分離も弱い。

**gRPC の `streamingPull` で totsuka が受ける。** 遅延は最小になるが tonic 一式を引き込み、ビルド時間を作り込んできた workspace に効く。REST の `pull` はメッセージが無ければサーバ側で待つので、承認ボタンの体感遅延は問題にならない。

**受け口を別リポジトリに切る。** スキーマの正が 2 リポジトリに分かれ、ズレを CI で捕まえる仕掛けを別途作る必要がある。

**公式イメージを出さずサンプルのみにする。** 供給鎖の責任を負わずに済むが、OpenTofu による自動化という目的と両立しない。

# 関連

- [ADR-0003 Slack メンション代理返信アシスタントの設計](/decisions/adr-0003-slack-reply-assistant.md) — トークン方針と承認フローの原型
- [ADR-0068 チャンネル監視トリガ](/decisions/adr-0068-channel-watch-trigger.md) — backfill の窓の考え方と命名
- [ADR-0055 herdr Socket API のスキーマ検査](/decisions/adr-0055-herdr-schema-typed-wire.md) — 実行時は寛容・CI は厳格の規律
- [ADR-0006 1Password バックエンド](/decisions/adr-0006-onepassword-secret-backend.md) — CLI へのシェルアウトで資格情報を解決する前例
- [task-source-slack](/components/task-source-slack.md)

[^slack-events-api]: Slack — The Events API
[^cloud-run-websockets]: Google Cloud — Using WebSockets with Cloud Run
[^pubsub-pricing]: Google Cloud — Pub/Sub pricing
[^cloud-run-pricing]: Google Cloud — Cloud Run pricing
[^slack-rate-limit-2025]: Slack — Rate limit changes for non-Marketplace apps
[^gce-free-tier]: Google Cloud — Compute Engine 無料枠
[^oci-always-free]: Oracle Cloud — Always Free Resources
