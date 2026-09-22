* **Creation**: [ADR-0092](/decisions/adr-0092-git-timeout.md) — `SystemGitRunner` の全 git 呼び出しに上限（既定 300 秒、`[worktree].git_timeout_secs` で上書き、`0` で無効）を設け、超えたらプロセスグループごと SIGKILL して `TimedOut` で失敗させ、dispatch の自動再キューに乗せる（#764）。`spawn_blocking` での非同期化は不採用
* **Update**: [運用ガイド](/operations/operations-guide.md) — 「SSH の keepalive（推奨設定）」節と、git のタイムアウトの FAQ を足した
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `adapters` に `git`（`DEFAULT_GIT_TIMEOUT` / `with_timeout`）を足した
* **Update**: [設定リファレンス](/development/config-reference.md) — `[worktree].git_timeout_secs` を足した
