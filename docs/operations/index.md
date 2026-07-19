# operations

障害対応Playbook・デプロイ手順・アラート別トリアージ。

* [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md) - totsuka 日常運用の手引き。doctor の読み方、worktree 掃除ポリシーと孤児掃除、run 停止・回復、よくある問題の切り分け。
* [リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）](release-runbook.md) - totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリの自動ビルドと GitHub Releases 配布、Release PR の CI/ブランチ保護を通すトークン運用（GitHub App / PAT / admin）、Gatekeeper（ad-hoc 署名）の扱い。
* [Slack セットアップ Quickstart（task-source-slack）](slack-quickstart.md) - manifest からの Slack アプリ作成 → トークン発行 → Keychain 登録 → plugin install/enable → doctor → run --watch までの導入手順と、トークン失効・スコープ変更時の対処。
* [フック完了判定のトラブルシューティング](hook-troubleshooting.md) - Claude Code フック方式の運用手引き。スプールバックログ（doctor check_spool での検出・drain/確認・corrupt 隔離ファイル）、Escalated タスクの対応手順（pane スナップショット確認・herdr pane での解消・次 Stop での自然復帰・fail アウト）、human 検収での totsuka task verify --pass/--fail 操作を、doctor のフックプローブ参照つきで整理する。
* [click-to-focus セットアップ（terminal-notifier / bundle id / 切り分け）](click-to-focus-setup.md) - 通知クリックで対象タスクの herdr pane を開く F-94 の導入手順。terminal-notifier の導入、plugins/macos.toml の backend / activate_bundle_id / click_command 設定、bundle id の調べ方、動作確認、クリックが効かない・通知が出ないときの切り分け表。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
