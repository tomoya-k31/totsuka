# references

外部資料の要約ミラー（Citationsの参照先）。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](herdr-socket-api.md) - herdr の Socket API（NDJSON・1接続1リクエストの接続モデル・workspace/pane/agent メソッド・events.subscribe・agent_status・pane レイアウト）と統合エージェント capability マトリクスの要約。agent_ide プラグイン（#60/#124/#356）設計の根拠。protocol 17 で agent.start が manifest 駆動（kind + 既存 pane）へ、プロンプト投入が agent.prompt へ変わった破壊的変更を含む。Claude Code は lifecycle authority を持たず状態は screen manifest 由来（done は発火しない）という制約を含む。
* [orca CLI 制御サーフェス / エージェント capability（外部一次情報ミラー）](orca-cli-control.md) - orca（onorca.dev / stablyai/orca ADE）を CLI から制御する手段（worktree/terminal/automations、tui-idle 状態検知、セレクタ、permission-bypass フラグ、resume/hibernation）の要約。agent_ide プラグイン（#61）設計の根拠。Claude Code は状態が status-line hook 由来の OSC state dots に依存し、構造化 plan/preview API を持たないという制約を含む。
* [herdr サイドバー設定（[ui.sidebar.*] のトークン語彙）](herdr-sidebar-config.md) - herdr の左サイドバー（spaces / agents）の行構成を決める [ui.sidebar.*].rows の書き方。組み込みトークンの一覧、$name によるメタデータ参照、rows_by_agent によるエージェント種別ごとの差し替え、インラインスタイル、1 パネル 16 行・1 行 16 トークンの上限（report_metadata 側の 16 とは別物）を、herdr 0.7.5 / protocol 17 の実機確認から記録する。
<!-- okf:index:end -->
