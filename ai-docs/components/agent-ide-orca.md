---
type: Component
title: agent-ide-orca プラグイン
description: orca を Agent IDE として接続する公式 agent_ide プラグイン。プロトコル面は herdr プラグインと同一で、orca 固有の起動・状態取得を orca CLI（--json）ラップとして隠蔽する。pane_control は非宣言（capability を正直に宣言）。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-orca
tags: [rust, crate, plugin, agent-ide, orca, cli, worktree]
generated: { by: claude-code/opus-5, at: 2026-08-20T00:00:00Z }
status: stable
owner: tomoya-k31
---

# 責務

orca を totsuka の Agent IDE として接続する公式プラグイン（F-30〜F-38）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、Orchestrator 側のプロトコルは [agent-ide-herdr](/components/agent-ide-herdr.md) と同一。orca 固有の起動・状態取得手段をプラグイン内に隠蔽する（F-32）。詳細設計は一次情報ミラー [orca CLI 制御サーフェス](/references/orca-cli-control.md) に準拠する。

orca は公開 REST/ソケット API を持たず、**`orca` CLI（`--json`）ラップが公式推奨**。実行単位は git worktree、エージェントは worktree 端末で TUI プロセスとして起動する。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `[orca]`（= `InitializeParams.config`）を型付け。`orca_bin` / `agent`（既定 claude）/ `setup`（run\|skip\|inherit）/ `repo_selector`（未設定時は dispatch の worktree_path を `path:` セレクタ化）/ `plan_prompt_prefix`（plan モードでプロンプト前置, F-36）/ `poll_interval_ms`。`worktree_name` で task id を orca 安全名に正規化。`deny_unknown_fields`。**#317: プロンプト文の組み込みデフォルトは Rust の文字列リテラルではなく `plugins/agent-ide-orca/src/defaults.toml`（`include_str!` で埋め込み、`LazyLock` で初回参照時に parse）に置く** — 文言調整をコード変更ではなくデータファイルの編集にするため（エピック [#311](https://github.com/tomoya-k31/totsuka/issues/311)）。上書き口は従来どおり `[orca]` の `plan_prompt_prefix` で、キーもシグネチャも `compose_prompt` の挙動も不変。orca は claude の `--permission-mode plan` や codex の `--sandbox read-only` に相当する構造的な plan API を持たないため、**この前置きテキストが plan 意図の唯一の強制手段**である点に注意（末尾の空行は後続のタスクプロンプトとの区切りとして意味を持つ） |
| `state` | orca の粗い3値状態（OSC state dots 由来）→ totsuka 正規化状態の写像（`working→running`・`waiting→waiting_input`・`done`/`idle`(tui-idle)→`done`・異常終了/timeout→`failed`・不明は前値維持, F-32）、`blocked` 時の terminal 出力からの質問 best-effort 抽出（F-35） |
| `cli` | `OrcaCli` trait（`run(args)→JSON`）＋ `ProcessCli`（`orca` サブプロセスを spawn し `--json` を parse）。ロジックを fake orca でテストするための seam |
| `agent` | `OrcaAgent<C: OrcaCli>`。`dispatch`（`worktree create --agent … --prompt … --json`→worktree id を session_id に, F-31/F-37）/ `attach`（`worktree show` で生存確認・消失は `attached:false`, 弱い吸収）/ `cancel`（`worktree rm --force`, 冪等）/ `start_state_stream`（`worktree ps` を poll し state dot を写像、`terminal wait --for tui-idle` で pacing, F-38） |
| `server` | JSON-RPC ディスパッチ `Server<F: CliFactory>`。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を直列化。`ProcessFactory`（設定の orca_bin から `ProcessCli`）を配線 |

# メソッド写像（F-32）

- `task/dispatch` → `orca worktree create --repo <sel> --name <n> --agent claude --prompt "…" --setup <mode> --json`（作成＋起動＋初回プロンプトを1コマンド）。plan モードは orca に構造化 API が無いため、プロンプト前置で意図を伝える（縮退可）
- `task/cancel` → `orca worktree rm --worktree id:<id> --force --json`
- `session/attach` → `orca worktree show --worktree id:<id> --json`（弱い吸収。orca が Agent Session History と `claude --resume` を保持するため内部対応表は最小）
- `state/subscribe` → `orca worktree ps --json` の state dot を poll・写像し、`orca terminal wait --for tui-idle` で pacing。「承認待ち idle」は `waiting` state で `done` と切り分ける

# capability negotiation（F-33）

orca CLI で確実に対応できる `state_stream` のみを宣言し、**`pane_control` も `hook_completion` も宣言しない**（pane 制御サーフェスを持たず、完了は state stream で報告する）（orca は pane 制御サーフェスを持たない）。かつては `design_preview` も非宣言だったが、その capability 自体がプロトコル 0.4.0 で削除された（#411、[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。Orchestrator は宣言された機能のみ要求するため、未対応機能があってもワークフローは成立する。`pane_control` 非宣言のため、0.1.4 の `session/focus` に加え **0.2.1 の `session/release`（worktree 掃除時の pane 解放, #210, [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）も 0.2.2 の `session/list`（doctor 孤児 pane 検出, #211, [ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）も呼ばれない** — Orchestrator は pane 解放をスキップして worktree だけ削除し、doctor の pane チェックも本プラグインには行わない（本プラグイン変更なし）。

# Claude Code / orca 固有の制約

状態は Orca 注入の status-line hook が発行する OSC state dots 由来の粗い3値で native な権威報告ではない。native な `failed` は無く端末異常終了・timeout から導出。結果は構造化ペイロードで返らないため、成果物は worktree 実体ファイルを直接読むのが堅牢（端末 scrollback パースより）。無人実行は orca の Yolo（Claude は `--dangerously-skip-permissions`）前提で使い捨て worktree に依存。詳細は [orca CLI 制御サーフェス リファレンス](/references/orca-cli-control.md) 参照。

# テスト

- 状態写像（3値＋異常→failed・大小無視・不明は前値維持）・worktree 名正規化・repo セレクタ・plan プロンプト前置・質問抽出は単体テスト。
- **fake orca CLI**（サブコマンド別レスポンス）に対して initialize→dispatch→state/subscribe→状態ストリーム（`running`→`waiting_input`（質問付き）→`done`、異常 state→`failed`）を結合テスト（`tests/integration.rs`）。session/attach 成功・worktree 消失（`attached:false`）、cancel の冪等、capability 宣言（`pane_control` 非宣言）、`config/validate`（`orca status` 疎通）を検証。
- 実バイナリを stdio で fake `orca` スクリプトに接続して疎通確認済み。
- **実機との手動疎通チェックリストは issue #61 のコメントに整理**（状態が OSC state dots 由来である前提での遅延・取りこぼし・「承認待ち idle」誤検知の観点を含む）。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`process`/`io-std`）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。
- `toml`（#317）— 埋め込みの `src/defaults.toml`（プロンプトのデフォルト）を読むためだけに使う。プラグインが受け取る実際の設定は `initialize` 経由の JSON のままで、この依存はバイナリに焼き込まれたフォールバック用。ワークスペースに既にある crate（`plugin-protocol` 等が使用）なので新しいライセンス面・監査面は増えない。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [agent-ide-herdr](/components/agent-ide-herdr.md)（プロトコル面が同一の対プラグイン）
- [orca CLI 制御サーフェス / エージェント capability（外部一次情報ミラー）](/references/orca-cli-control.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
