# quality

テスト戦略・E2Eシナリオ・既知の不具合パターン。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [テスト戦略（自動結合テスト / E2E / モックプラグイン）](test-strategy.md) - totsuka のテスト層（ユニット・実プロセス結合・バイナリE2E）とモックプラグインによるシナリオ注入、フレーク対策、CI 品質ゲートの定義。
* [リリース前手動チェックリスト（実機結合）](release-checklist.md) - CI で自動化できない実機（herdr / orca）・設計プレビュー・通知・waiting_input 応答の目視確認手順。リリース前に実施する。
* [既知の不具合・制約パターン](known-issues.md) - テスト・運用で判明した既知の制約（LLM VCR 未対応、recovery 時の成果物欠落、worktree テストのフレーク要因など）と回避策。
<!-- okf:index:end -->
