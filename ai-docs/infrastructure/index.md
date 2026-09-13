# infrastructure

GCPプロジェクト構成・環境・IaCモジュール・Secret方針。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [Homebrew tap（tomoya-k31/homebrew-tap）](homebrew-tap.md) - totsuka を brew install で配れるようにするための tap リポジトリ。formula のインストールレイアウトがなぜ bundled plugins の探索順と一致するのか、リリースジョブが何を書き換えるのか、HOMEBREW_TAP_TOKEN のスコープ、bump が失敗したときの復旧、そして public 化までステップを止めている可視性ゲート。
* [Event Gateway の OpenTofu モジュール](slack-event-gateway-tofu.md) - services/slack-event-gateway/tofu/ の構成。Cloud Run 1 サービス・利用者ごとの Pub/Sub トピックとサブスクリプション 2 組・Secret Manager の登録表・利用者を自分のキューだけに閉じる IAM を tofu apply で立てる。min-instances 0 と max-instances 上限が費用の前提であること、invoker_iam_disabled が組織ポリシーを緩めずに公開する唯一の手段であること、IP 制限と VPC Service Controls を既定に入れない理由を含む。
<!-- okf:index:end -->
