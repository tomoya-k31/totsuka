# glossary

ドメイン用語・社内略語。1用語=1ファイル。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [Task（タスク）](task.md) - タスクソース由来の作業単位。共通スキーマ（plugin-protocol の Task 型）に正規化され、状態DBの1行として9状態のステートマシン（F-71）を遷移する。
* [Task Source（タスクソース）](task-source.md) - タスクの供給元（GitHub / Notion 等）。task_source プラグインが task/submit（push）・task/update_status・result/publish を実装して接続する。
* [Agent IDE（エージェントIDE）](agent-ide.md) - コーディングエージェントを動かす実行環境（herdr / orca 等）。agent_ide プラグインが task/dispatch・session/attach・state/subscribe を実装して接続する。
* [Notifier（ノーティファイア）](notifier.md) - waiting_input / done / failed / pending イベントを人間へ届ける通知プラグイン。配送は fire-and-forget でタスク実行に影響しない（F-93）。
* [worktree（ワークツリー）](worktree.md) - タスク専用の git 作業ディレクトリ。「1 task = 1 repo = 1 worktree = 1 branch」の正規化単位で、完了後は掃除ポリシー（immediate / retention_days / keep_7d / keep_28d / manual）が「判定 → pane 解放 → 削除」の3段で適用される。
* [dispatch（ディスパッチ）](dispatch.md) - キュー済みタスクをエージェントに割り当てる操作。スロット確保 → worktree 準備 → task/dispatch RPC → セッションID永続化までを指す。
* [Workflow（ワークフロー）](workflow.md) - source × trigger × mode × agent × output の名前付き束ね（F-80）。タスクは定義順の first-match で最大1つのワークフローに割り当てられる（F-81）。mode / output / verification は profile の 4 原型でまとめて指定することもできる。
* [エフェメラル承認フロー](ephemeral-approval.md) - エージェントの返信案をスレッド内エフェメラル + self-DM 記録の 2 面に提示し、承認ボタン押下時のみ本人名義で送信する task-source-slack の仕組み。勝手に送信しないための防波堤。
* [会話継続（conversation continuity）](conversation-continuity.md) - 1 スレッド = 1 会話を 1 タスクとして扱い、追いメンションを同じタスクへの追加メッセージとして取り込むことで worktree・ブランチ・エージェントセッションを共有する仕組み。#242 でタスク同一性そのものを会話単位に変えた。
* [AI Tool（AI ツール）と 2 軸モデル](ai-tool.md) - pane 内で起動する AI エージェント CLI（Claude Code / Codex / OpenCode）。pane を管理する agent プラグイン（herdr 等）とは直交する軸で、[tools] レジストリと tool フィールド（workflow > repo > default_tool > 組み込み claude）で選択される。
* [pane（ペイン）](pane.md) - エージェント CLI が実際に動くターミナル区画（herdr の pane）。dispatch 時に worktree を cwd、label を totsuka + source task id として作られ、pane_control capability 越しの session/focus・session/release・session/list で制御され、寿命は worktree の掃除ポリシーに連動する。
* [click-to-focus（クリックで pane を開く）](click-to-focus.md) - 通知をクリックすると、その通知を出したタスクの herdr pane が前面に来る機能（F-94）。terminal-notifier の -activate（GUI 前面化）+ -execute（totsuka focus → 制御 UDS /focus → agent_ide の session/focus 委譲）の 2 段で実現し、縮退はすべて静か。
* [要対応（Attention）](attention.md) - 人間が動かさない限り永久に進まない非終端タスクの集合。pending / waiting_input / verifying / escalated / queued+wait_reason の 5 状態からなり、メニューバーのバッジ（F-109）が数える対象。終端状態を含めないのは、含めると数字が単調増加して 0 に戻らなくなるため。
* [チャンネル監視トリガ（channel watch）](channel-watch.md) - 特定チャンネルへのトップレベル投稿そのものをトリガにして 1 投稿 = 1 タスクを起こす仕組み。メンションもリアクションも要らないぶん「投稿できる人」が実行できる人になるため、既定の起動者は操作者本人だけで、trigger.from が唯一の明示的な緩和口になる。会話継続の対象外。
* [起動時バックフィル（startup backfill）](startup-backfill.md) - チャンネル監視ソースが起動時に、監視チャンネルの直近 N 件かつ年齢上限以内を無条件に再送してプラグイン停止中の取りこぼしを回収する仕組み。台帳が重複を Duplicate として無害化するため永続カーソルを持たず、取りすぎ側に倒してある。
<!-- okf:index:end -->
