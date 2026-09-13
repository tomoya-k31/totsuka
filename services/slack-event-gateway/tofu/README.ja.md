> 🌐 [English](README.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

# Event Gateway — OpenTofu モジュール

`tofu apply` で一式が立つ。このファイルはモジュール自身のリファレンスである。
Slack アプリ側の設定は別の手順で、サービスが何であるか・Request URL が 2 つ要ることは
ゲートウェイ本体の README に書いてある。

## 作るもの

| | |
|---|---|
| Cloud Run | 全利用者で共有する 1 サービス。`min_instance_count = 0`、`invoker_iam_disabled = true` |
| Pub/Sub | **利用者ごとに**トピック 2 本とサブスクリプション 2 本（メッセージ・リアクション用と、ボタン押下用） |
| Secret Manager | 登録表。サービスにはファイルとしてマウントする |
| IAM | サービスは全トピックに publish、**各利用者は自分の 2 本だけ**を読める |

サービスアカウントキーは作らない。組織ポリシーにも触れない。

## apply する前に

**組織ポリシーを 1 つ確認すること。** 公開エンドポイントは `invoker_iam_disabled` で
成立している —— ドメイン制限共有が `allUsers` を拒否する場合の Google 公式の回答である。
管理者はこれ自体も塞げる:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

既定では未適用である。**適用されている**なら、この構成は成立しない ——
外部ロードバランサも助けにならない（Serverless NEG 経由でも Cloud Run には
認証情報なしで到達するため）。

## 先に state の置き場を決める

**backend を指定しない `tofu init` は state をローカルファイルに書く。** そのファイルには
全利用者の Slack signing secret が平文で入る。このモジュールが backend を宣言していないのは
意図的で（利用者の組織のバケットを知らないため）、**選ばなければ既定のローカルになる**。

使い捨ての実験でないなら、デプロイ担当だけが読めるバケットに置くこと。**最初の `init` より前に**
このファイルの隣に `backend.tf` を作る:

```hcl
terraform {
  backend "gcs" {
    bucket = "my-tofu-state"     # バージョニング有効・均一アクセス制御
    prefix = "slack-event-gateway"
  }
}
```

あとから移すこともできる（`tofu init -migrate-state`）が、その時点で**ローカルのコピーは
既に存在してしまっている** —— 秘密を含んだまま。

## apply

```bash
cd services/slack-event-gateway/tofu
cp terraform.tfvars.example terraform.tfvars   # 中身を埋める
tofu init
tofu plan
tofu apply
```

出力を読む:

```bash
tofu output -json request_urls     # Slack の Request URL **2 箇所とも**に貼る
tofu output -json totsuka_config   # 各利用者の [slack.gateway] ブロック
```

## 人を増やす

`operators` に 1 エントリ足して `tofu apply`。トピック・サブスクリプション・IAM・登録表が
すべて追従する。他に編集するものは無く、既存の利用者のリソースにも触らない ——
リソースは位置ではなく `key` で識別しているためである。

apply は Cloud Run の新しいリビジョンも作るので、完了した時点で新しい表が有効になる。
マウントが `latest` ではなく**シークレットの版を正確に指している**のはそのためで、
`latest` だとサービス側の引数が 1 つも変わらず、リビジョンが作られず、稼働中の
インスタンスは回収されるまで古い表を配り続ける。

## 破棄

```bash
tofu destroy
```

そのまま動く。Cloud Run の `deletion_protection` は off にしてある。このモジュールが
有効化した API は有効なまま残す —— 同じプロジェクトの別のワークロードが使っているかも
しれないためである。

**登録表は一緒に消える。** シークレットの古いバージョンは、構成が生きているあいだは破棄ではなく
**放棄**される（それがロールバックを可能にしている）が、`destroy` は親シークレットを消し、
それが全バージョンを道連れにする。作り直す予定があるなら `terraform.tfvars` を取っておくこと。

## このモジュールがやらないこと 2 つ

**送信元 IP で絞らない。絞れない。** Slack は許可リストに使える安定したアドレス一覧を
公開していない。推測で組んだ許可リストは配信を落とし始め、配信失敗の累積こそが Slack に
購読を無効化させる条件である —— ゲートウェイが消そうとしているまさにその失敗になる。
この経路の関門は、推測不能なパス・署名・5 分のタイムスタンプ窓の 3 つで、いずれも
コンテナの中にある。

**VPC Service Controls を有効にしない。** pull 側を社内ネットワークに限定したいなら
Pub/Sub にペリメータを張るのが方法だが、**既定ではなく意図して足すもの**である ——
自宅や出張先からキューを引けなくなり、それは totsuka がまさに想定している使い方だからである。
既定で入れると、稀なケースを固めるために普通のケースを壊すことになる。

## シークレットと state

`terraform.tfvars` には全利用者の Slack signing secret が入り、**state ファイルにも同じものが入る**
（OpenTofu は暗号化しない）。どちらも gitignore 済みだが、それは git に入れないというだけの話で、
本題は上の「先に state の置き場を決める」のほうである。

**state バケットへの読み取り権限は、全利用者の Slack アプリへの読み取り権限**とみなすこと。
