# Decisions (ADR)

アーキテクチャ上の意思決定記録。1決定=1ファイル、`adr-NNNN-<slug>.md` 形式。

* [ADR-0001 OKFによるドキュメント管理の採用](adr-0001-adopt-okf.md) - リポジトリ内ドキュメントをOKF v0.1準拠のKnowledge Bundleとして管理する決定。
* [ADR-0002 Rust workspace 構成と CI 品質ゲート](adr-0002-rust-workspace-ci.md) - totsuka を Rust edition 2024 のヘキサゴナル workspace（core/cli/plugin-protocol）として構成し、clippy deny warnings・rustfmt・cargo-audit/deny・llvm-cov を CI 品質ゲートに据える決定。
* [ADR-0003 Slack メンション代理返信アシスタントの設計](adr-0003-slack-reply-assistant.md) - task-source-slack をコア無変更のプラグイン内完結で実装する決定。リポジトリ解決はプラグイン内 3 段階、イベントはバッファ + 短周期 tasks/fetch、トークンはユーザートークン（xoxp）のみで本人名義返信 + 承認フロー必須。
* [ADR-0004 Claude Code フック完了シグナルの受信をコア driving adapter に置く](adr-0004-hook-completion-signal.md) - Claude Code の完了検知を screen-manifest からフック機構へ移すにあたり、UDS 受信サーバを orchestrator-core の driving adapter（ports::SignalPort + adapters::hook_uds）側に置き、herdr プラグイン内には置かない決定。llm 検収はセッション内 prompt 型 Stop フックで行う。
* [ADR-0005 通知 click-to-focus は terminal-notifier + session/focus 委譲で実現する](adr-0005-click-to-focus.md) - 通知クリックで対象タスクの herdr pane を開く F-94 を、terminal-notifier（-execute/-activate）+ `totsuka focus` + 制御 UDS + agent_ide への `session/focus` 委譲（0.1.4 additive、pane_control 相乗り）で実現する決定。UNUserNotificationCenter 自前 .app・alerter・NotifyParams への pane_id 追加は不採用。
