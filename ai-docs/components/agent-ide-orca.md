---
type: Component
title: agent-ide-orca プラグイン
description: orca を Agent IDE として接続する公式 agent_ide プラグイン。herdr プラグインと同じ契約（tool_launch をそのまま起動・hook で完了報告・exit の deadman・pane_control・diagnostics_snapshot）を、orca CLI（--json）の端末操作で実現する。セッションは Orchestrator の worktree に開いた orca 端末。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-orca
tags: [rust, crate, plugin, agent-ide, orca, cli, terminal, hooks]
generated: { by: claude-code/opus-5, at: 2026-09-19T04:00:00+09:00 }
verified:
  - { by: claude-code/opus-5, at: 2026-09-19T03:46:00+09:00 }
stale_after: 2027-03-19
status: stable
owner: tomoya-k31
---

# 責務

orca を totsuka の Agent IDE として接続する公式プラグイン（F-30〜F-38）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、**Orchestrator から見た契約は [agent-ide-herdr](/components/agent-ide-herdr.md) と同じ** — 同じメソッド・同じ capability・同じ完了経路（hook）を持つ。orca 固有の手段はプラグイン内に閉じる（F-32）。設計の根拠は [ADR-0081](/decisions/adr-0081-orca-herdr-parity.md)、orca 側の事実は [orca CLI 制御サーフェス](/references/orca-cli-control.md) の実測節。

orca は公開 REST/ソケット API を持たず、**`orca` CLI（`--json`）ラップが公式推奨**。JSON-RPC は stdout、診断ログは stderr。

**前提: 対象リポジトリが orca に登録されていること**（`orca repo add --path <repository>`）。orca は登録済みリポジトリの git worktree を自分で見つけるので、Orchestrator が切った worktree もそのまま `path:` で引ける。未登録なら dispatch はその旨のエラーで失敗する。こうした worktree は orca から見ると external worktree で、リポジトリ設定 `externalWorktreeVisibility` が `show` なら GUI のサイドバーでプロジェクト配下に `{repo}: {title}` の名前で表示され、選べばエージェントのタブが見える（GUI で確認済み）。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `cli` | `OrcaCli` trait（`run(args) → result`）＋ `ProcessCli`。orca の `--json` envelope（`{id, ok, result}` / `{id, ok: false, error: {code, message}}`）を剥がし、`ok: false` は `OrcaError::Orca { code }` にする。**トップレベルの `id` は CLI リクエストの id** で、変更前のプラグインはこれを worktree id と取り違えていた。1 回の呼び出しは `request_timeout_secs` で打ち切り（`kill_on_drop`）、`terminal wait --timeout-ms` と `terminal send --wait-submit` にはその待ち時間＋10 秒を与える |
| `error` | `OrcaError`。orca のエラーコードで判定する: `is_missing`（`terminal_handle_stale` / `*_not_found`）・`is_exited`（`terminal_exited`）・`is_gone`（どちらか）・`is_wait_timeout`（`timeout`）。`WorktreeUnknown`（repo 未登録の案内）・`MissingToolLaunch`・`SessionUnresumable` |
| `launch` | `tool_launch` を `terminal create --command` の文字列にする: `exec env 'K=V' … 'program' 'arg' …`。orca は `--command` をログインシェルに**打ち込む**ので全語を単一引用符でクォートし、`exec` でシェルを置き換えて端末の寿命をエージェントに一致させる。クォートは実際の `sh` で読み戻すテストで固定 |
| `config` | `[orca]` = `orca_bin` / `request_timeout_secs`（既定 30）/ `[orca.layout]`（`shell` 既定 **false**・`direction` は `horizontal` / `vertical` の閉じた集合）/ `[orca.identity]`（`enabled` 既定 true）。`deny_unknown_fields`。廃止キー（`agent` / `setup` / `repo_selector` / `plan_prompt_prefix` / `poll_interval_ms`）は `removed_keys_in` が名指しで代替を案内する |
| `state` | orca の worktree `status`（state dots 由来）→ `AgentState`。**`session/attach` 専用**で、完了判定には使わない。`active` など不明値は呼び出し側が渡す前値（`running`）を保つ |
| `agent` | `OrcaAgent<C: OrcaCli>`。下のメソッド写像のすべて |
| `server` | JSON-RPC ディスパッチ `Server<F: CliFactory>`。herdr と同じメソッド集合。`SessionUnresumable` → `SESSION_UNRESUMABLE`、`MissingToolLaunch` → `INVALID_PARAMS` |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を直列化 |

# メソッド写像

| メソッド | orca CLI |
|---|---|
| `task/dispatch` | `terminal create --worktree path:<worktree_path> --title "totsuka <task_id>" --command "exec env … <tool_launch>"` → `worktree set --comment "totsuka <task_id>" [--display-name "<repo>: <title>"]`（所有マーカーと identity、best-effort）→ `terminal split`（`layout.shell` のときのみ）→ `terminal wait --for tui-idle` → `terminal show` で `agentIdentity` が出るまで待つ（最大 30 秒）→ `terminal send --text <prompt> --enter --wait-submit 60`。`session_id` = 端末 handle。途中で失敗したらタブを閉じてから失敗を返す |
| `task/cancel` | `terminal close --tab`（`terminal_handle_stale` は成功扱い）。**worktree は消さない** — Orchestrator のもの |
| `session/attach` | `terminal show` の `connected` → 生存。state は `worktree ps` のその worktree の `status`、無ければ `running` |
| `session/release` | `terminal show` で `expect_cwd` / `expect_label` を照合 → `terminal close --tab`。終了済み（`connected: false`）は残ったタブを片付けて `gone`。handle 消失と不一致のときは、`session/list` に**別の handle で**同じ worktree（`expect_cwd`）か同じラベル（`expect_label` — `doctor` はこちらだけを送る）の端末があれば `refused`、無ければ `gone` |
| `session/list` | `terminal list` の各端末の `worktreeId` を `worktree list --repo id:<repoId>` の comment と突き合わせ、comment が `totsuka ` で始まる worktree の端末を 1 worktree 1 行（エージェントを認識している端末を優先）|
| `session/focus` | `terminal switch`（`terminal_exited` / stale は `focused: false`） |
| `diagnostics/snapshot` | `terminal read --screen`、描画できなければ（`source: screen-unavailable`）`terminal read --limit 200`。失敗は `text: None` |
| `state/subscribe` | `terminal wait --for exit` を繰り返す deadman。満たされたら／handle が消えたら、**`terminal show` で裏を取ってから** `failed` を 1 回送って終了（`connected: true` なら誤報として待ち直す — 実機 e2e で `wait` が生きている端末を「消えた」と答え、動いていたタスクが 5 秒で `failed` にされた）。orca の `timeout` は再試行、それ以外の失敗が 5 回続いたら `failed` |

## プロンプトを「認識後」に送る理由

`tui-idle` だけでは Claude の入力受付に間に合わない。実測で、`tui-idle` が満たされた瞬間（起動約 4 秒）に送ったプロンプトは `provider: "unsupported"` の生キー入力として出て行き、**Claude に届かなかった**。`agentIdentity` が出てから送ると `provider: "claude"`・`stages: [input_accepted, turn_started]` で、複数行の本文が 1 ターンとして届く。orca が統合を持たないツールは認識されないので、30 秒で諦めて送る（警告のみ）。

## 所有マーカーを worktree の comment に置く理由

タブタイトルは作業中のエージェントが OSC で書き換え続ける。`terminal create --title` は数秒、`terminal rename` も
5 秒保たなかった（実機 e2e、2026-09-19）。worktree の comment を書くのは orca の CLI と利用者だけで、worktree は
タスクごとなので、そこに `totsuka <task_id>` を置く。代償は、人間が同じ worktree に開いた端末も所有端末に数えうること。

## deadman がすべての終了を `failed` にする理由

orca の `exitCode` は実際の終了コードを反映しない（`exit 3` が `exitCode: 0`・`exitCause: unknown` と報告された）。herdr のように「0 なら正常」とは言えない。対話型エージェントは完了しても終了しないし、hook が完了させたタスクへの `failed` は Orchestrator が無視する。

# capability negotiation（F-33）

herdr と同じ `pane_control` / `state_stream` / `hook_completion` / `diagnostics_snapshot` を宣言する（`plugin.toml` と `capabilities_result` の一致は結合テストが検査）。`hook_completion` により Orchestrator は `job_id` と hook 設定入りの `tool_launch` を渡し、完了は Claude Code の Stop/SessionEnd hook が報告する。`pane_control` により worktree 掃除時の `session/release`（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）・`doctor` の孤児検出（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）・通知クリックの `session/focus`（[ADR-0005](/decisions/adr-0005-click-to-focus.md)）が orca にも届く。

# テスト

- 単体: envelope の解釈（`id` を漏らさない・`ok: false` のコード・envelope 無しの失敗）、`terminal wait` の打ち切り時間、エラー分類、`exec env` の組み立てと `sh` による読み戻し、廃止キーの案内、状態写像、表示名の文字境界での切り詰め、`resume_failure` の狭さ。
- 結合（`tests/integration.rs`、fake orca CLI に実測の応答形を返させる）: capability 宣言と `plugin.toml` の一致、dispatch の引数（`path:` セレクタ・`exec env`・タイトル・`--wait-submit`）と**呼び出し順**（`tui-idle` → `show` → `send`、rename はしない）、`worktree create` / `worktree rm` を呼ばないこと、`tool_launch` 欠落、repo 未登録、resume 失敗の `SESSION_UNRESUMABLE` と後片付け、認識されないエージェント（一時停止クロックで 30 秒）、deadman、attach / cancel / release（一致・消失・終了済み・不一致）/ list / focus / snapshot、`config/validate`（`runtime.reachable`）。
- **実機（orca 1.4.205 + Claude Code 2.1.277）**: ビルドしたバイナリを stdio で駆動し、dispatch → プロンプトが 1 ターンとして届き応答 → `session/list` に出る → `diagnostics/snapshot` → `session/release` で閉じる → deadman が `failed` → attach が `attached: false`、まで通した。**Orchestrator を含む通し（hook による完了報告）は未実施**で、[live-e2e](/quality/release-checklist.md) の orca 節で確認する。

# 依存

- `plugin-protocol`（プラグイン境界）、`plugin-sdk`（tracing 初期化のみ）、`tokio`（`process` / `io-std`、dev では `test-util`）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。
- `toml` は外した（プロンプト既定値の `defaults.toml` を読むためだけの依存だったが、`plan_prompt_prefix` ごと廃止した）。

# 関連

- [ADR-0081 orca プラグインを herdr と同じ契約で駆動する](/decisions/adr-0081-orca-herdr-parity.md)
- [agent-ide-herdr](/components/agent-ide-herdr.md)（同じ契約の対プラグイン）
- [orca CLI 制御サーフェス / エージェント capability（外部一次情報ミラー）](/references/orca-cli-control.md)
- [plugin-protocol](/components/plugin-protocol.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38](/product/orchestrator-spec.ja.md)
