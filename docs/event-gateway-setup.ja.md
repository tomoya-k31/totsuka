> 🌐 [English](event-gateway-setup.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/event-gateway-setup.md sha256:ccbdac86a42b582d87360855142427d2bfd43edd34b62a8aca6a3a2bebec6998 -->

# Event Gateway 構築手順

**`event_source = "gateway"` を選んだ場合だけのページである。** ソケット接続で運用するなら
ここは一切要らない —— [Slack セットアップ](slack-setup.ja.md) だけで完結する。

ゲートウェイがあるのは、ソケット接続では **`totsuka run` のプロセス自身がそれを握る**ためである。
totsuka が止まっている間に届いたメンションは失われ、長く止まったままだと Slack がそのアプリの
イベント購読そのものを止める。ゲートウェイは totsuka の代わりに受けてキューに積むので、
Slack が配信するために動き続けるものが無くなる。対価は GCP プロジェクト 1 つと月 1 ドル程度。

これが消すのは**「totsuka が止まっていること」が配信失敗の原因になる経路**であって、配信失敗そのものではない。
ゲートウェイが落ちていれば、Request URL が誤っていれば、publish に失敗すれば、いずれも配信は失敗する。

## 0. まず組織ポリシーを 1 つ確認する

**ここが通らないと設計そのものを変えることになる**ので、何より先に確認する。

Slack は IAM プリンシパルになれないので、Cloud Run は認証なしの呼び出しを受ける必要がある。
通常は `allUsers` に `roles/run.invoker` を付けるが、**ドメイン制限共有がそれを拒否する** ——
2024 年 5 月以降に作られた組織では既定で有効である。

サポートされている代替が `--no-invoker-iam-check`（OpenTofu では `invoker_iam_disabled`）で、
Google がまさにこの状況向けに文書化しているものである。**組織ポリシーの変更は一切要らない。**

ただし管理者はこれ自体も塞げる:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

**既定では未適用**なので、大半の組織はそのまま通る。**適用されている**場合、この構成は成立しない ——
外部ロードバランサも助けにならない（Serverless NEG 経由でも Cloud Run には認証情報なしで到達するので、
結局 `allUsers` が要る）。ソケット接続に戻るか、組織の管理者に相談すること。

## 1. GCP プロジェクト

課金が有効なプロジェクトを 1 つ。既存のものに相乗りしてもよいが、**API を有効化し、
サービスアカウントと IAM を作る**ので、専用にしたほうが影響範囲を考えやすい。

```bash
gcloud auth login
gcloud config set project <PROJECT_ID>
```

## 2. 利用者ごとに 4 つの値

**先に Slack アプリを作る。** 順番がややこしいのは、Request URL に入れるホスト名が
`tofu apply` の結果であり、`tofu apply` に入れる signing secret が Slack アプリの結果だからである。
一周しないように、こう割る:

1. **アプリだけ作る**（[Slack セットアップ](slack-setup.ja.md) の手順 1 の 1〜3）。
   `manifest.gateway.yml` の `<gateway-host>` は**プレースホルダのままでよい** —— Slack が
   Request URL を検証するのは**保存時**で、manifest から作る時点ではまだ検証されない
2. その時点で signing secret は発行済みなので、下の 4 つが揃う
3. `tofu apply`（手順 3）でホスト名が出る
4. **アプリに戻って Request URL を 2 箇所に入れる**（手順 4）。ここで初めて検証が走る

| 値 | 出どころ |
|---|---|
| Slack user id（`U…`） | Slack のプロフィール → **⋯** → メンバー ID をコピー |
| signing secret | Slack アプリ → Basic Information → App Credentials → Signing Secret |
| パストークン | **生成する。考えない** —— `openssl rand -hex 24` |
| Google プリンシパル | その人のキューを読める identity —— 手順 5 で `gcloud auth application-default login` を実行するアカウントを、`user:alice@example.com` の形で書く |

**パストークンは資格情報である。** 公開エンドポイントの手前には IAM も IP 許可リストも無く、
立ちはだかるのは推測不能なパス・署名・5 分のタイムスタンプ窓だけである。選んだ単語ではなく
ランダム文字列であること。

**Slack アプリは利用者ごとに作る。** 1 つのアプリを複数人がインストールする形も Slack は持つが、
2 人が同じチャンネルにいるとそこへの投稿が 1 通のイベントとして届き、誰宛かを知るのに
**毎メッセージ追加の API 呼び出し**が要る。

## 3. apply

```bash
cd services/slack-event-gateway/tofu
cp terraform.tfvars.example terraform.tfvars
# 手順 2 の値で埋める
tofu init
tofu plan
tofu apply
```

全員で共有する Cloud Run サービス 1 つ（ルーティングはパスで行う）、利用者ごとに Pub/Sub の
トピック 2 本とサブスクリプション 2 本、Secret Manager の登録表、そしてサービスが全トピックに
publish できる一方で**各利用者は自分のキューだけを読める** IAM が立つ。

**サービスアカウントキーは作られない。** サービスはメタデータサーバからトークンを得て、
各利用者は自分の Google アカウントを使う。

### シークレットと state

**`terraform.tfvars` と state ファイルの両方に、全利用者の Slack signing secret が入る。**
OpenTofu は state を暗号化しないので、デプロイ担当だけが読めるバケットに置き、
**そのバケットへのアクセス = 全員の Slack アプリへのアクセス**とみなすこと。gitignore は
git に入れないというだけの話である。

## 4. Slack に Request URL を入れる

```bash
tofu output -json request_urls
```

利用者ごとの URL を、その人の Slack アプリの**2 箇所**に貼る。

| Slack アプリの設定 | 運ぶもの |
|---|---|
| Event Subscriptions → Request URL | メンション、リアクション |
| Interactivity & Shortcuts → Request URL | 承認・リポジトリ選択のボタン |

**前者だけだと、メンションは動いたままボタンが一切届かない** —— 症状から原因に辿り着けない
種類の失敗である。保存時に Slack が URL を検証するので、**保存できた時点で疎通は取れている**。
保存できないなら、ゲートウェイが動いていないか URL が違う。

## 5. totsuka の設定

```bash
tofu output -json totsuka_config
```

**これは `[slack]` テーブル全体ではなく、そこに足すキーである。**

先に `totsuka setup` を通し（[Slack セットアップ](slack-setup.ja.md) の手順 3）、
`user_token` と `target_user_id` を含む `[slack]` を書かせること。**`setup` は既に存在する
`[slack]` テーブルには触らない**ので、このブロックだけを先に貼ると必須キーが永久に入らない。

`setup` が書いたものに足す:

```toml
[slack]
# …setup が書いた user_token / target_user_id はそのまま…
event_source = "gateway"

[slack.gateway]
project                    = "<PROJECT_ID>"
subscription               = "slack-event-gateway-<key>-events"
block_actions_subscription = "slack-event-gateway-<key>-block-actions"
```

`setup` が書いた `app_token` の行は消してよい —— WebSocket を開かないためである。
`setup` の最後に走る `doctor` が App-Level Token を要求して落ちるのは**この編集の前だから**で、
編集後に `totsuka doctor` を回し直せば緑になる。

そのあと利用者ごとに、その人の機械で 1 回:

```bash
gcloud auth application-default login
```

totsuka は**この identity で**キューを読む。起動時に各キューを 1 回読むので、identity 違い・
権限の欠落・名前の打ち間違いはその場で落ちる —— 放っておくと 3 つとも同じものを生む。
**緑の `doctor` と、1 件も来ないイベント**である。

## 人を増やす

`terraform.tfvars` の `operators` に 1 エントリ足して `tofu apply`。トピック・サブスクリプション・
IAM・登録表がすべて追従し、**既存の利用者のリソースには触らない**。apply は新しいリビジョンも
作るので、完了した時点で新しい表が有効になる。

そのあと、その人について手順 2・4・5 を行う —— その人専用の Slack アプリ、Request URL を
2 箇所、そしてその人の機械での `gcloud auth application-default login`。

## 破棄

```bash
tofu destroy
```

そのまま動く。有効化した API は**有効なまま残す** —— 同じプロジェクトの別のワークロードが
使っているかもしれないためである。

## 費用の見積りが前提にしている 2 つ

月 1 ドル程度という数字は、**この 2 つが守られている限り**の話である。

**`min_instance_count = 0`。** 1 にすると待機時間が課金対象になり、金額が桁で動く。これは
調整項目ではなく、この構成が安い理由である。コールドスタートの代償は 1 秒程度で、Slack の
ack 期限は 3 秒である。

**`max_instances` の上限（既定 4）。** 公開側の関門はすべて**コンテナの中**で評価されるので、
**署名で弾いたリクエストも、そのためのコールドスタートも課金される**。手前に IAM 層が無い以上、
プラットフォーム既定の 100 までスケールするのは費用の前提と合わない。配信が実際に落ち始めたら
上げる —— 先回りでは上げない。

東京（`asia-northeast1`）は Cloud Run の tier 1 で、見積りはこれを前提にしている。tier 2 の
リージョン（`asia-east2` / `asia-northeast3` / `asia-southeast1` 等）は vCPU 秒あたりの単価が上がる。

## 固められないもの、既定で固めないもの

**送信元 IP は制限できない。** Slack は許可リストに使える安定したアドレス一覧を公開していない。
推測で組んだ許可リストは配信を落とし始め、**配信失敗の累積こそが Slack に購読を無効化させる** ——
この仕組みが消そうとしている失敗そのものである。関門は推測不能なパス・署名・5 分の窓で、
いずれもコンテナの中にある。

**VPC Service Controls は既定で有効にしない。** **読む**側を社内ネットワークに閉じたいなら
Pub/Sub にペリメータを張るのが方法だが、意図して足すこと —— 自宅や出張先からキューを引けなくなり、
それは totsuka がまさに想定している使い方である。

**ドメイン制限共有は有効のまま維持する。** この構成は、組織ポリシーを緩めずに動くように作ってある。

---

このページは `ai-docs/operations/event-gateway-setup.md` から生成されている。
