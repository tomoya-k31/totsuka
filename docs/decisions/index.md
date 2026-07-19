# Decisions (ADR)

アーキテクチャ上の意思決定記録。1決定=1ファイル、`adr-NNNN-<slug>.md` 形式。

* [ADR-0001 OKFによるドキュメント管理の採用](adr-0001-adopt-okf.md) - リポジトリ内ドキュメントをOKF v0.1準拠のKnowledge Bundleとして管理する決定。
* [ADR-0002 Rust workspace 構成と CI 品質ゲート](adr-0002-rust-workspace-ci.md) - totsuka を Rust edition 2024 のヘキサゴナル workspace（core/cli/plugin-protocol）として構成し、clippy deny warnings・rustfmt・cargo-audit/deny・llvm-cov を CI 品質ゲートに据える決定。
* [ADR-0003 Slack メンション代理返信アシスタントの設計](adr-0003-slack-reply-assistant.md) - task-source-slack をコア無変更のプラグイン内完結で実装する決定。リポジトリ解決はプラグイン内 3 段階、イベントはバッファ + 短周期 tasks/fetch、トークンはユーザートークン（xoxp）のみで本人名義返信 + 承認フロー必須。
* [ADR-0004 Claude Code フック完了シグナルの受信をコア driving adapter に置く](adr-0004-hook-completion-signal.md) - Claude Code の完了検知を screen-manifest からフック機構へ移すにあたり、UDS 受信サーバを orchestrator-core の driving adapter（ports::SignalPort + adapters::hook_uds）側に置き、herdr プラグイン内には置かない決定。llm 検収はセッション内 prompt 型 Stop フックで行う。
* [ADR-0005 通知 click-to-focus は terminal-notifier + session/focus 委譲で実現する](adr-0005-click-to-focus.md) - 通知クリックで対象タスクの herdr pane を開く F-94 を、terminal-notifier（-execute/-activate）+ `totsuka focus` + 制御 UDS + agent_ide への `session/focus` 委譲（0.1.4 additive、pane_control 相乗り）で実現する決定。UNUserNotificationCenter 自前 .app・alerter・NotifyParams への pane_id 追加は不採用。
* [ADR-0006 シークレット参照に 1Password (op://) を第 2 バックエンドとして追加する](adr-0006-onepassword-secret-backend.md) - 設定のシークレット参照へ op://<vault>/<item>/<field> を第 3 のスキームとして追加し、解決は 1Password CLI（op read）へのシェルアウトで行う決定。SDK/Connect は不採用、v1 は対話アンロック前提（Service Account は後続）、SecretRef の enum 化 + 合成ストアでスキーム振り分け。op は cross-platform のため非 macOS 初の実働バックエンドにもなる。
* [ADR-0007 CI 実行タイミングの再設計（Actions コスト最適化）](adr-0007-ci-cost-optimization.md) - GitHub Actions の無料枠超過を受け、品質ゲートの内容は変えずに実行タイミングを再設計する決定。PR は clippy+rustfmt / test、main への push は coverage(llvm-cov) のみ、audit は日次 cron + 依存ファイル変更 PR に移し、全ワークフローへ concurrency を導入する。
