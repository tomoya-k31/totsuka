# development

環境構築・コーディング規約・ブランチ戦略・レビュー規約。

* [ログ規約（JSON Lines・機密マスキング）](logging-conventions.md) - totsuka の構造化ログ規約。JSON Lines 1行1イベント、task_id 相関、機密マスキング（フィールド denylist＋値パターン）、log_prompts、日次ローテーションと世代保持。
* [設定リファレンス（config.toml）](config-reference.md) - config.toml と plugins/{name}.toml の全キー・デフォルト値・意味の一覧。シークレット参照、ワークフロー、出力ポリシー、掃除ポリシー、並列上限を含む。
* [設定例集（config.toml / plugins/*.toml）](config-examples.md) - そのまま貼って動く config.toml の完全版注釈付き例と、選択肢を持つキー（kind・mode・output・verification・cleanup・trigger・シークレット参照・並列上限）の選び分け基準、および最小構成／GitHub Projects／Slack／設計→実装ハンドオフのシナリオ別レシピ。
* [プラグイン開発ガイド](plugin-dev-guide.md) - totsuka プラグインの作り方。plugin-protocol クレートの型、JSON-RPC(NDJSON/stdio) メソッド、plugin.toml マニフェスト、capability 宣言、ビルド手順（Cargo バイナリ名と plugin.toml の name 不一致時の対処）、install/enable の流れ、参照実装。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
