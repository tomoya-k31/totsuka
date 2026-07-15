# operations

障害対応Playbook・デプロイ手順・アラート別トリアージ。

* [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md) - totsuka 日常運用の手引き。doctor の読み方、worktree 掃除ポリシーと孤児掃除、run 停止・回復、よくある問題の切り分け。
* [リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）](release-runbook.md) - totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリの自動ビルドと GitHub Releases 配布、Release PR の CI/ブランチ保護を通すトークン運用（GitHub App / PAT / admin）、Gatekeeper（ad-hoc 署名）の扱い。
* [Slack セットアップ Quickstart（task-source-slack）](slack-quickstart.md) - manifest からの Slack アプリ作成 → トークン発行 → Keychain 登録 → plugin install/enable → doctor → run --watch までの導入手順と、トークン失効・スコープ変更時の対処。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
