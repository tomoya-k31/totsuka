# components

パッケージ/サービス単位の責務・公開インターフェース・依存先。

* [okf-search](okf-search.md) - docs/ の frontmatter（type/status/owner/resource/tags と OKF v0.2 の generated/verified/sources/stale_after）でconceptを絞り込むCLIスクリプトと、絞り込み結果をAIが読んで抽出するokf-searchスキル。
* [orchestrator-core](orchestrator-core.md) - totsuka のコア。ヘキサゴナルアーキテクチャの domain（ドメイン・ステートマシン）/ ports（TaskSource・AgentIde・LlmRouter・SecretStore 等の trait）/ adapters（JSON-RPC ブリッジ・SQLite・Keychain）を担う。
* [orchestrator-cli](orchestrator-cli.md) - totsuka の CLI エントリポイント（bin: totsuka）。§5.1 のコマンド体系（init / setup / run / status / task / focus / plugin / config / logs / doctor / completion）と共通フラグ（--config / --debug / --json）を提供する。
* [plugin-protocol](plugin-protocol.md) - プラグイン開発者向けに公開する型定義クレート。JSON-RPC 2.0（NDJSON）エンベロープ・plugin.toml マニフェスト・capabilities・§11 メソッド型・Task 共通スキーマ・プロトコルバージョニングを提供する、プラグイン境界の単一の正。
* [plugin-sdk](plugin-sdk.md) - task_source プラグイン作成用のヘルパークレート。単一 writer タスクの stdio ランタイム・JSON-RPC dispatch ボイラープレート（TaskSourceHandler）・task/submit クライアント（バックオフ再送）・ポーリング型ソース向け poll_loop を提供する。
* [task-source-github](task-source-github.md) - GitHub Issues / ProjectsV2 をタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。GraphQL で fetch→正規化、ProjectsV2 ステータス書き戻し、Issue コメント publish を行う。
* [task-source-notion](task-source-notion.md) - Notion データベースをタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。プロパティマッピングで任意の DB 構造を Task へ正規化し、ステータス書き戻しとページ本文への結果追記を行う。
* [task-source-slack](task-source-slack.md) - 自分宛の Slack メンションをタスク化し本人名義で代理返信する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。設定スキーマ・TokenGuard（auth.test + apps.connections.open + 任意の bot probe）・Web API / Socket Mode クライアント・メンション検知と Task 正規化・プラグイン内 3 段階リポジトリ解決・下書き提示・承認フロー・bot ナッジ DM 通知（#305）に加え、manifest 雛形・CLI レベル E2E・運用ドキュメントまで完備（エピック#102 完了）。
* [agent-ide-herdr](agent-ide-herdr.md) - herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/設計プレビューを担う。
* [agent-ide-orca](agent-ide-orca.md) - orca を Agent IDE として接続する公式 agent_ide プラグイン。プロトコル面は herdr プラグインと同一で、orca 固有の起動・状態取得を orca CLI（--json）ラップとして隠蔽する。design_preview は非宣言（capability を正直に宣言）。
* [notifier-macos](notifier-macos.md) - Orchestrator のイベント（waiting_input / done / failed / pending / escalated / verification_pending）を macOS 通知センターへ配送する公式 notifier プラグイン。バックエンド選択（osascript / terminal-notifier click-to-focus）、ワークフロー×イベント別フィルタ、fire-and-forget 配送。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
