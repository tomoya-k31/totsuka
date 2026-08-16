# releases

リリースノート・互換性情報・マイグレーション手順。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [アップグレードとロールバック（state.db）](upgrade-and-rollback.md) - totsuka のバージョンアップ時に state.db のマイグレーションを適用する手順と、バックアップから戻すロールバック手順。schema v7 時点。バージョン不整合エラー（SchemaTooNew / SchemaOutdated）の読み方も含む。
<!-- okf:index:end -->
