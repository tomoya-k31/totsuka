# data

テーブル・データモデル・キューの定義と設計意図。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [状態DB（SQLite state.db）スキーマ](state-db.md) - タスク実行状態を永続化する SQLite DB（$XDG_STATE_HOME/totsuka/state.db）の tasks/sessions/events/hook_events/task_messages/schema_migrations スキーマと設計判断。
<!-- okf:index:end -->
