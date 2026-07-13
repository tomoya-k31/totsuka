# quality

テスト戦略・E2Eシナリオ・既知の不具合パターン。

* [テスト戦略（自動結合テスト / E2E / モックプラグイン）](test-strategy.md) - totsuka のテスト層（ユニット・実プロセス結合・バイナリE2E）とモックプラグインによるシナリオ注入、フレーク対策、CI 品質ゲートの定義。
* [リリース前手動チェックリスト（実機結合）](release-checklist.md) - CI で自動化できない実機（herdr / orca）・設計プレビュー・通知・waiting_input 応答の目視確認手順。リリース前に実施する。
* [既知の不具合・制約パターン](known-issues.md) - テスト・運用で判明した既知の制約（LLM VCR 未対応、recovery 時の成果物欠落、worktree テストのフレーク要因など）と回避策。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
