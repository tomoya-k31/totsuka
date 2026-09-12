---
type: Runbook
title: Event Gateway 構築手順（event_source = "gateway"）
description: GCP 側の構築手順。着手前の組織ポリシー確認、OpenTofu による Cloud Run / Pub/Sub / Secret Manager / IAM の一括構築、Slack の Request URL 2 箇所の設定、totsuka 側の config、人を増やす手順、破棄、費用の前提。Socket Mode を使う読者はこのページを読む必要がない。
resource: https://github.com/tomoya-k31/totsuka/tree/main/slack-event-gateway/tofu
tags: [slack, gateway, gcp, cloud-run, pubsub, secret-manager, opentofu, runbook, cost]
generated: { by: claude-code/opus-5, at: 2026-09-14T01:00:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/event-gateway-setup.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/event-gateway-setup.md docs/event-gateway-setup.ja.md -->

# このページを読む必要がある人

`event_source = "gateway"` を選んだ人だけである。**Socket Mode で運用するなら、ここから先は
一切要らない** —— [Slack セットアップ Quickstart](/operations/slack-quickstart.md) だけで完結する。

方式の選び分けは Quickstart の手順 0 にある。要点だけ繰り返すと、**この構成が解くのは
「totsuka が止まっている間にメンションが失われ、止まりすぎると Slack が購読そのものを
無効化する」**という問題であり、対価は GCP プロジェクト 1 つと月 1 ドル程度である
（[ADR-0072](/decisions/adr-0072-slack-event-gateway.md)）。

# 0. 着手前に、組織ポリシーを 1 つ確認する

**ここが通らないと構成そのものを考え直すことになる**ので、何より先に確認する。

Slack は IAM プリンシパルになれないので、Cloud Run は認証なしで呼べる必要がある。通常は
`allUsers` に `roles/run.invoker` を付けるが、**ドメイン制限共有**
（`constraints/iam.allowedPolicyMemberDomains`）が有効な組織ではこの付与が拒否される。
2024-05-03 以降に作られた組織では既定で有効である。

その場合の正規の手段が **`--no-invoker-iam-check`**（OpenTofu では `invoker_iam_disabled = true`）で、
Google がドメイン制限共有下での推奨として明示しているものである。**組織ポリシーは一切緩めずに済む。**

ただし管理者は、この無効化自体を別の制約で塞げる:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

**既定では未適用**なので大半の組織では素通りする。適用されていた場合、この構成は成立しない ——
**外部ロードバランサも助けにならない**（Serverless NEG 経由でも Cloud Run には認証情報なしで到達するので、
結局 `allUsers` が要る）。その場合は Socket Mode に戻るか、IT 部門に相談すること。

# 1. GCP プロジェクトを用意する

課金が有効なプロジェクトを 1 つ。既存プロジェクトに相乗りしてもよいが、**このモジュールは
API を有効化し、サービスアカウントと IAM を作る**ので、影響範囲を切りたいなら専用にするほうが楽である。

```bash
gcloud auth login
gcloud config set project <PROJECT_ID>
```

# 2. 利用者ごとの値を用意する

利用者 1 人につき 3 つ。

| 値 | 出どころ |
|---|---|
| Slack user id（`U…`） | Slack のプロフィール → … → メンバー ID をコピー |
| signing secret | Slack アプリ → Basic Information → App Credentials → Signing Secret |
| パストークン | **生成する。考えない** —— `openssl rand -hex 24` |

**パストークンは資格情報である。** 公開エンドポイントの手前には IAM も IP 許可リストも無いので、
「推測不能なパス・署名・5 分のタイムスタンプ窓」の 3 つだけが関門になる（ADR-0072 決定 11）。
選んだ単語ではなくランダム文字列であること。

**Slack アプリは利用者ごとに別々に作る。** 1 つのアプリを複数人がインストールする形も Slack は
持つが、同じチャンネルに複数の利用者がいるとイベントが 1 通にまとめられ、誰宛かを知るのに
毎イベント追加の API 呼び出しが要る（決定 6）。

# 3. `tofu apply`

```bash
cd slack-event-gateway/tofu
cp terraform.tfvars.example terraform.tfvars
# 手順 2 の値で埋める
tofu init
tofu plan
tofu apply
```

立つもの:

- Cloud Run 1 サービス（**全利用者で共有**。ルーティングはパスで行う）
- 利用者ごとに Pub/Sub トピック 2 本とサブスクリプション 2 本
- Secret Manager の登録表（サービスにはファイルとしてマウント）
- IAM —— サービスは全トピックに publish、**各利用者は自分のサブスクリプションだけ**を読める

**サービスアカウントキーは作られない。** ゲートウェイは Cloud Run のメタデータサーバから、
各利用者は自分の Google アカウントからトークンを得る。

## `terraform.tfvars` と state の扱い

**どちらにも全利用者の signing secret が入る。** OpenTofu は state を暗号化しないので、
**デプロイ担当だけが読めるバケットに置き、そのバケットへのアクセス = 全利用者の Slack アプリへの
アクセスとみなすこと。** `.gitignore` は `*.tfvars` を弾くが、それは git に入れない話でしかない。

# 4. Slack 側に Request URL を入れる

```bash
tofu output -json request_urls
```

出た URL を、利用者ごとの Slack アプリの**2 箇所**に貼る。

| Slack アプリの設定 | 運ぶもの |
|---|---|
| Event Subscriptions → Request URL | メンション、リアクション |
| Interactivity & Shortcuts → Request URL | 承認・リポジトリ選択のボタン |

**片方だけだと、メンションは動いたまま承認ボタンだけが一切届かない。** Socket Mode ではどちらも
同じ WebSocket で届いていたため区別が要らなかった箇所で、症状から原因に辿り着きにくい。

保存時に Slack が `url_verification` を投げるので、**保存できた時点で疎通は取れている**。
保存に失敗するなら、ゲートウェイが動いていないか URL が違う。

# 5. totsuka 側の設定

```bash
tofu output -json totsuka_config
```

出た値を `~/.config/totsuka/config.toml` に入れる:

```toml
[slack]
event_source = "gateway"

[slack.gateway]
project                    = "<PROJECT_ID>"
subscription               = "slack-event-gateway-<key>-events"
block_actions_subscription = "slack-event-gateway-<key>-block-actions"
```

キーの意味は [設定リファレンス](/development/config-reference.md)。`app_token`（`xapp-`）は
**要らない** —— WebSocket を開かないので用途が無い。

各利用者の手元で 1 回:

```bash
gcloud auth application-default login
```

totsuka は**この ADC で**自分のキューを引く。起動時に各サブスクリプションへ `pull` を 1 回投げて、
identity 違い・権限の欠落・名前の打ち間違いをその場で落とす（どれも放っておくと
「`doctor` は緑なのにイベントが 1 件も来ない」形で失敗する）。

# 人を増やす

`terraform.tfvars` の `operators` に 1 エントリ足して `tofu apply`。トピック・サブスクリプション・
IAM・登録表がすべて追従し、**既存の利用者のリソースには触らない**。apply は Cloud Run の新しい
リビジョンも作るので、完了した時点で新しい表が有効になる。

そのあと、その人の Slack アプリを作って Request URL を 2 箇所に入れ、手元で
`gcloud auth application-default login` をする（手順 2・4・5 をその人のぶんだけ）。

# 破棄

```bash
tofu destroy
```

そのまま動く。このモジュールが有効化した API は**有効なまま残す** —— 同じプロジェクトの別の
ワークロードが使っているかもしれないためである。

# 費用の前提は 2 つある

月 1 ドル程度という数字は、**この 2 つが守られている限りの話**である（決定 10）。

**`min_instance_count = 0`。** 1 以上にすると待ち時間が課金対象になり、桁が変わる。これは調整項目
ではなく、この構成が安い理由そのものである。コールドスタートの遅延は 1 秒程度で、Slack の
ack 期限は 3 秒なので、実用上の代償は無い。

**`max_instances` の上限（既定 4）。** 公開側の関門は 3 つとも**コンテナの中**で評価されるので、
**署名で弾いたリクエストも、そのためのコールドスタートも課金される**。手前に IAM 層が無い以上、
既定の 100 までスケールできる状態は費用の前提と噛み合わない。配信が実際に落ち始めたら上げる ——
先回りでは上げない。

東京（`asia-northeast1`）は Cloud Run の **tier 1** で、見積りはこのレートである。tier 2 の
リージョン（`asia-east2` / `asia-northeast3` / `asia-southeast1` 等）に置くと単価が上がる。

# セキュリティ上、できないこと・既定でしないこと

**送信元 IP で絞れない。** Slack は許可リストに使える安定したアドレス一覧を公開していない。
推測で組んだ許可リストは配信を落とし始め、**配信失敗の累積こそが Slack に購読を無効化させる条件**
である —— この仕組みが消そうとしている失敗そのものになる。公開側の関門は、推測不能なパス・
署名・5 分の窓の 3 つで、いずれもコンテナの中にある。

**VPC Service Controls は既定で入れない。** 引く側（Pub/Sub）を社内ネットワークに限定したいなら
ペリメータを張るのが方法だが、**意図して足すものである** —— 自宅や出張先からキューを引けなくなり、
それは totsuka がまさに想定している使い方だからである。

**ドメイン制限共有は有効のまま維持する。** この構成は組織ポリシーを緩めずに成立するように
作ってある（手順 0）。

# 関連

- [Slack セットアップ Quickstart](/operations/slack-quickstart.md) — 方式の選択と、Slack 側の全手順
- [ADR-0072](/decisions/adr-0072-slack-event-gateway.md) — 設計判断の正本
- [Event Gateway の OpenTofu モジュール](/infrastructure/slack-event-gateway-tofu.md) — モジュールの中身
- [slack-event-gateway](/components/slack-event-gateway.md) — 立てる対象のサービス
- [Event Gateway（イベントゲートウェイ）](/glossary/event-gateway.md) — 用語
