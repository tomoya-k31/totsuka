# references

外部資料の要約ミラー（Citationsの参照先）。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->

* [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](herdr-socket-api.md) - herdr の Socket API（NDJSON・1接続1リクエストの接続モデル・workspace/pane/agent メソッド・events.subscribe・agent_status・pane レイアウト）と統合エージェント capability マトリクスの要約。agent_ide プラグイン（#60/#124/#356）設計の根拠。Claude Code は lifecycle authority を持たず状態は screen manifest 由来（done は発火しない）という制約を含む。
* [orca CLI 制御サーフェス / エージェント capability（外部一次情報ミラー）](orca-cli-control.md) - orca（onorca.dev / stablyai/orca ADE）を CLI から制御する手段（worktree/terminal/automations、tui-idle 状態検知、セレクタ、permission-bypass フラグ、resume/hibernation）の要約。agent_ide プラグイン（#61）設計の根拠。Claude Code は状態が status-line hook 由来の OSC state dots に依存し、構造化 plan/preview API を持たないという制約を含む。
