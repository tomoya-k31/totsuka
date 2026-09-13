---
type: IaC Module
title: Event Gateway の OpenTofu モジュール
description: services/slack-event-gateway/tofu/ の構成。Cloud Run 1 サービス・利用者ごとの Pub/Sub トピックとサブスクリプション 2 組・Secret Manager の登録表・利用者を自分のキューだけに閉じる IAM を tofu apply で立てる。min-instances 0 と max-instances 上限が費用の前提であること、invoker_iam_disabled が組織ポリシーを緩めずに公開する唯一の手段であること、IP 制限と VPC Service Controls を既定に入れない理由を含む。
resource: https://github.com/tomoya-k31/totsuka/tree/main/services/slack-event-gateway/tofu
tags: [gcp, cloud-run, pubsub, secret-manager, iam, opentofu, terraform, slack, cost]
generated: { by: claude-code/opus-5, at: 2026-09-14T05:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# なぜリポジトリが IaC を持つのか

totsuka は OSS で、この機能はオプションである。**実運用のデプロイ先は利用者が用意する**
（会社なら org のリポジトリ、個人なら非公開リポジトリでもよい）。リポジトリが提供するのは
**構築手順と、それを自動化する OpenTofu** に限る —— GCP プロジェクト ID も Slack アプリ ID も
登録表の中身も、ここには入らない（[ADR-0072](/decisions/adr-0072-slack-event-gateway.md) 決定 6・9）。

# 作るもの

| リソース | 単位 | 備考 |
|---|---|---|
| Cloud Run サービス | **全利用者で 1 つ** | 集約は運用上の選択で、費用上の利点は無い（サービス数に課金されない）。代償は 1 プロセスが全員の signing secret を持つこと |
| Pub/Sub トピック | 利用者 × 2 | 通常・`block_actions`。トピック数・サブスクリプション数に課金されないので、分けても費用は増えない |
| Pub/Sub サブスクリプション | 利用者 × 2 | 保持は 7 日 / 35 分 |
| Secret Manager シークレット | 1 | 登録表。サービスには**ファイルとしてマウント**する |
| サービスアカウント | 1 | publish 権限のみ。**キーは作らない** |
| IAM | 利用者 × 2 | 各利用者に**自分のサブスクリプションだけ** `roles/pubsub.subscriber` |

# 費用の前提は 2 つあり、どちらも変数で縛ってある

**`min_instance_count = 0`。** 1 以上にすると待ち時間が課金対象になり、月額が桁で変わる
（決定 10）。これは調整項目ではなく、この設計が月 1 ドル程度で済む理由そのものである。
遅延の代償も小さい —— コールドスタートは 1 秒程度で、Slack の ack 期限は 3 秒である。

**`max_instances` の上限（既定 4）。** 公開側の関門は 3 つとも**コンテナの中**で評価される
（決定 11）ので、**署名で弾いたリクエストも、そのためのコールドスタートも課金される**。
`invoker_iam_disabled` によって手前の IAM 層が無い以上、既定の 100 までスケールできる状態は
費用の前提と噛み合わない。既定の 4 は「利用者数人 × 1 人が追える範囲のチャンネル」という
本設計の前提から取った（Cloud Run の既定同時実行数は 1 インスタンスあたり 80）。

# 公開の仕方

Slack は IAM プリンシパルになれないので Cloud Run は公開が要る。ドメイン制限共有が有効な
組織では `allUsers` への `roles/run.invoker` 付与が拒否されるため、**`invoker_iam_disabled = true`**
を使う —— Google がドメイン制限共有下での手段として明示しているもので、**組織ポリシーには一切触れない**。

**ただし管理者は `constraints/run.managed.requireInvokerIam` でこの無効化自体を制限できる。**
既定では未適用だが、**適用されていればこの構成は成立しない**（外部ロードバランサも助けにならない ——
Serverless NEG 経由でも Cloud Run には認証情報なしで到達する）。apply の前に確認する:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

# 既定に入れないもの

**送信元 IP による制限。** Slack は許可リストに使える安定したアドレス一覧を公開していない。
推測で組めば配信を落とし始め、**配信失敗の累積こそが Slack に購読を無効化させる条件**である ——
この仕組みが消そうとしている失敗そのものになる。

**VPC Service Controls。** pull 側（経路 B）を社内ネットワークに限定したいなら Pub/Sub に
ペリメータを張るのが方法だが、**意図して足すものであって既定ではない** —— 自宅や出張先から
キューを引けなくなり、それは本設計がまさに想定している使い方だからである。

# 運用上の注意

- **`terraform.tfvars` と state ファイルの両方に全利用者の signing secret が入り、このモジュールに逃げ道は無い**（登録表を変数から組み立てることが「1 人足して apply」を成立させている当のものだから）。OpenTofu は state を暗号化しないので、デプロイ担当だけが読めるバケットに置き、**そのバケットへのアクセス = 全利用者の Slack アプリへのアクセス**とみなす。`.gitignore` は `terraform.tfvars` という名前ではなく `*.tfvars` を弾く ——`prod.auto.tfvars` のような名前はごく普通で、守りたいのは名前ではなく中身である
- **`.terraform.lock.hcl` は commit する。** 秘密は入らず、プロバイダの版とハッシュだけである。`Cargo.lock` を commit しているのと同じ理由で、手元の `tofu init` が CI の検証したものと同じ版を引くようにする（`versions.tf` の制約も `~> 6.0` で上限を切ってある）
- リソースは**位置ではなく `key`** で識別しているので、利用者を 1 人足しても既存の
  リソースは作り直されない
- `deletion_protection = false` を明示している。provider の既定は true で、そのままだと
  `tofu destroy` が**利用者が選んだ覚えのない設定**を理由に失敗する
- **state の置き場はモジュールが決めない。** backend を宣言していない（利用者の組織の
  バケットを知らないため）ので、**選ばなければローカルファイルになり、そこに全利用者の
  signing secret が平文で入る**。README は最初の `init` より前に `backend.tf` を置く手順を
  持つ。あとから `-migrate-state` で移せるが、その時点でローカルのコピーは既に存在している
- **シークレットのバージョンは `ABANDON`（`DISABLE` ではない）。** どちらも旧版を残すが、
  **Secret Manager は無効化されたバージョンからの読み取りを拒否する** —— マウントが版を
  正確に指している以上、rollout 中に旧リビジョンのインスタンスが自分の表を読めなくなる。
  `ABANDON` は管理から外すだけで読める状態を保つ。`destroy` は親シークレットを消すので
  バージョンも道連れになり、掃除は効く
- **シークレットのマウントは `latest` ではなく版を正確に指す。** `latest` だと
  サービス側の引数が 1 つも変わらないため**リビジョンが作られず**、利用者を足しても
  稼働中のインスタンスは回収されるまで古い表を配り続ける。さらに、サービスと版のあいだに
  グラフ上の辺が無いので**初回 apply が版より先にリビジョンを作って失敗しうる**
  （しかも 2 回目は通るので再現しない）
- サブスクリプションの `expiration_policy.ttl` を空にしている。既定は 31 日 pull が無いと
  サブスクリプションを消すので、**長期休暇の人のキューが黙って消え**、復帰後の症状は
  「totsuka が何も受け取らない」になる

# 関連

- [ADR-0072](/decisions/adr-0072-slack-event-gateway.md) — 設計判断の正本
- [slack-event-gateway](/components/slack-event-gateway.md) — 立てる対象のサービス
