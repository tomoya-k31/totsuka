# glossary

ドメイン用語・社内略語。1用語=1ファイル。

* [Task（タスク）](task.md) - タスクソース由来の作業単位。共通スキーマ（plugin-protocol の Task 型）に正規化され、状態DBの1行として9状態のステートマシン（F-71）を遷移する。
* [Task Source（タスクソース）](task-source.md) - タスクの供給元（GitHub / Notion 等）。task_source プラグインが tasks/fetch・task/update_status・result/publish を実装して接続する。
* [Agent IDE（エージェントIDE）](agent-ide.md) - コーディングエージェントを動かす実行環境（herdr / orca 等）。agent_ide プラグインが task/dispatch・session/attach・state/subscribe を実装して接続する。
* [Notifier（ノーティファイア）](notifier.md) - waiting_input / done / failed / pending イベントを人間へ届ける通知プラグイン。配送は fire-and-forget でタスク実行に影響しない（F-93）。
* [worktree（ワークツリー）](worktree.md) - タスク専用の git 作業ディレクトリ。「1 task = 1 repo = 1 worktree = 1 branch」の正規化単位で、完了後は掃除ポリシー（immediate / retention_days / manual）が適用される。
* [dispatch（ディスパッチ）](dispatch.md) - キュー済みタスクをエージェントに割り当てる操作。スロット確保 → worktree 準備 → task/dispatch RPC → セッションID永続化までを指す。
* [Workflow（ワークフロー）](workflow.md) - source × trigger × mode × agent × output の名前付き束ね（F-80）。タスクは定義順の first-match で最大1つのワークフローに割り当てられる（F-81）。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
