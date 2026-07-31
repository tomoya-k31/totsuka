# glossary

ドメイン用語・社内略語。1用語=1ファイル。

* [Task（タスク）](task.md) - タスクソース由来の作業単位。共通スキーマ（plugin-protocol の Task 型）に正規化され、状態DBの1行として9状態のステートマシン（F-71）を遷移する。
* [Task Source（タスクソース）](task-source.md) - タスクの供給元（GitHub / Notion 等）。task_source プラグインが task/submit（push）・task/update_status・result/publish を実装して接続する。
* [Agent IDE（エージェントIDE）](agent-ide.md) - コーディングエージェントを動かす実行環境（herdr / orca 等）。agent_ide プラグインが task/dispatch・session/attach・state/subscribe を実装して接続する。
* [Notifier（ノーティファイア）](notifier.md) - waiting_input / done / failed / pending イベントを人間へ届ける通知プラグイン。配送は fire-and-forget でタスク実行に影響しない（F-93）。
* [worktree（ワークツリー）](worktree.md) - タスク専用の git 作業ディレクトリ。「1 task = 1 repo = 1 worktree = 1 branch」の正規化単位で、完了後は掃除ポリシー（immediate / retention_days / keep_7d / keep_28d / manual）が「判定 → pane 解放 → 削除」の3段で適用される。
* [dispatch（ディスパッチ）](dispatch.md) - キュー済みタスクをエージェントに割り当てる操作。スロット確保 → worktree 準備 → task/dispatch RPC → セッションID永続化までを指す。
* [Workflow（ワークフロー）](workflow.md) - source × trigger × mode × agent × output の名前付き束ね（F-80）。タスクは定義順の first-match で最大1つのワークフローに割り当てられる（F-81）。
* [エフェメラル承認フロー](ephemeral-approval.md) - エージェントの返信案をスレッド内エフェメラル + self-DM 記録の 2 面に提示し、承認ボタン押下時のみ本人名義で送信する task-source-slack の仕組み。勝手に送信しないための防波堤。
* [会話継続（conversation continuity）](conversation-continuity.md) - 1 スレッド = 1 会話を 1 タスクとして扱い、追いメンションを同じタスクへの追加メッセージとして取り込むことで worktree・ブランチ・エージェントセッションを共有する仕組み。#242 でタスク同一性そのものを会話単位に変えた。
* [AI Tool（AI ツール）と 2 軸モデル](ai-tool.md) - pane 内で起動する AI エージェント CLI（Claude Code / Codex / OpenCode）。pane を管理する agent プラグイン（herdr 等）とは直交する軸で、[tools] レジストリと tool フィールド（workflow > repo > default_tool > 組み込み claude）で選択される。
* [pane（ペイン）](pane.md) - エージェント CLI が実際に動くターミナル区画（herdr の pane）。dispatch 時に worktree を cwd、label を totsuka + source task id として作られ、pane_control capability 越しの session/focus・session/release・session/list で制御され、寿命は worktree の掃除ポリシーに連動する。
* [click-to-focus（クリックで pane を開く）](click-to-focus.md) - 通知をクリックすると、その通知を出したタスクの herdr pane が前面に来る機能（F-94）。terminal-notifier の -activate（GUI 前面化）+ -execute（totsuka focus → 制御 UDS /focus → agent_ide の session/focus 委譲）の 2 段で実現し、縮退はすべて静か。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
