# operations

障害対応Playbook・デプロイ手順・アラート別トリアージ。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [セットアップ Playbook（新マシン / 開発機 / ローテーション / 復旧）](setup-playbook.md) - ゼロから totsuka が動くまでを通しで示す導入手順。新マシン（tarball 配置 → totsuka setup → シークレット登録 → doctor → run）、開発機（クローン → --from-source）、トークンローテーション、中断・失敗時の復旧を扱う。
* [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md) - totsuka 日常運用の手引き。doctor の読み方、worktree 掃除ポリシーと孤児掃除、run 停止・回復、よくある問題の切り分け。
* [リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）](release-runbook.md) - totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリと同梱プラグインの自動ビルド・署名・GitHub Releases 配布、Release PR の CI/ブランチ保護を通すトークン運用（GitHub App / PAT / admin）、Gatekeeper（ad-hoc 署名）の扱い。
* [Slack セットアップ Quickstart（task-source-slack）](slack-quickstart.md) - manifest からの Slack アプリ作成 → トークン発行 → Keychain 登録 → totsuka setup → doctor → run --watch までの導入手順と、手で書く場合のフォールバック、トークン失効・スコープ変更時の対処。
* [フック完了判定のトラブルシューティング](hook-troubleshooting.md) - Claude Code フック方式の運用手引き。スプールバックログ（doctor hook-spool チェックでの検出・drain/確認・corrupt 隔離ファイル）、Escalated タスクの対応手順（pane スナップショット確認・herdr pane での解消・次 Stop での自然復帰・fail アウト）、human 検収での totsuka task verify --pass/--fail 操作を、doctor のフックプローブ参照つきで整理する。
* [Codex ツールのセットアップと hooks trust 運用](codex-tool-setup.md) - リポジトリ/ワークフローを Codex CLI で動かすための一回きりのセットアップ手順（インストール確認・config 設定・hooks trust・対象リポジトリの trust）と、trust が壊れた場合の復旧手順。
* [OpenCode ツールのセットアップと運用](opencode-tool-setup.md) - リポジトリ/ワークフローを OpenCode で動かすためのセットアップ（インストール確認・config 設定・アセット自動配置）と、Codex/Claude と異なる縮退（block 不可・指示が可視・llm 検収不可）の運用上の注意。
* [click-to-focus セットアップ（terminal-notifier / bundle id / 切り分け）](click-to-focus-setup.md) - 通知クリックで対象タスクの herdr pane を開く F-94 の導入手順。terminal-notifier の導入、plugins/notifier-macos.toml の backend / activate_bundle_id / click_command 設定、bundle id の調べ方、動作確認、クリックが効かない・通知が出ないときの切り分け表。
* [herdr サイドバーに repo / タスクを出す（一回きりの設定）](herdr-sidebar-setup.md) - totsuka が dispatch 時に報告する repo / task / mode のメタデータトークンをサイドバーに出すための ~/.config/herdr/config.toml スニペットと、反映手順・確認方法・出ないときの切り分け。totsuka はこのファイルを書き換えないので手作業になる。
<!-- okf:index:end -->
