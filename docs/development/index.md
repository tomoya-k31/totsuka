# development

環境構築・コーディング規約・ブランチ戦略・レビュー規約。

* [ログ規約（JSON Lines・機密マスキング）](logging-conventions.md) - totsuka の構造化ログ規約。JSON Lines 1行1イベント、task_id 相関、機密マスキング（フィールド denylist＋値パターン）、log_prompts、日次ローテーションと世代保持。
* [設定リファレンス（config.toml）](config-reference.md) - config.toml と plugins/{name}.toml の全キー・デフォルト値・意味の一覧。シークレット参照、設定スキーマのバージョニング方針、ワークフロー、出力ポリシー、掃除ポリシー、並列上限、[hooks]・検収設定、task-source-slack の plugins/slack.toml を含む。
* [設定例集（config.toml / plugins/*.toml）](config-examples.md) - そのまま貼って動く config.toml の完全版注釈付き例と、選択肢を持つキー（kind・mode・output・verification・cleanup・trigger・シークレット参照・並列上限）の選び分け基準、TOTSUKA_* 環境変数オーバーライドの対応表、および最小構成／GitHub Projects／Slack／設計→実装ハンドオフのシナリオ別レシピ。
* [依存関係ハイジーン（未使用依存と Cargo.lock ドリフトの検出）](dependency-hygiene.md) - cargo-machete による毎 PR の未使用依存チェックの運用、誤検知の抑制手順（package.metadata.cargo-machete）、高精度な cargo-shear / cargo-udeps の定期手動実行手順、および cargo metadata --locked による Cargo.lock ドリフト検出（宣言はあるが lock に無い、という逆方向のドリフト）。
* [プラグイン開発ガイド](plugin-dev-guide.md) - totsuka プラグインの作り方。plugin-protocol クレートの型、JSON-RPC(NDJSON/stdio) メソッド、plugin.toml マニフェスト、capability 宣言、開発ループ（plugin install --from-source）とビルド手順（bin 名 = plugin.toml の name という不変条件）、install/enable の流れ、参照実装。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
