---
type: Reference
title: orca CLI 制御サーフェス / エージェント capability（外部一次情報ミラー）
description: orca（onorca.dev / stablyai/orca ADE）を CLI から制御する手段（worktree/terminal/automations、tui-idle 状態検知、セレクタ、permission-bypass フラグ、resume/hibernation）の要約。agent_ide プラグイン（#61）設計の根拠。Claude Code は状態が status-line hook 由来の OSC state dots に依存し、構造化 plan/preview API を持たないという制約を含む。
resource: https://www.onorca.dev/docs/cli/reference
tags: [orca, cli, ade, worktree, integration, agent-ide, external]
timestamp: 2026-07-13T00:00:00Z
status: active
owner: tomoya-k31
---

# このドキュメントについて

orca 公式ドキュメント（[CLI reference](https://www.onorca.dev/docs/cli/reference) / [Claude Code in Orca](https://www.onorca.dev/docs/agents/claude-code) /
[Supported agents](https://www.onorca.dev/docs/agents/supported) / [Settings](https://www.onorca.dev/docs/settings) /
[Worktrees](https://www.onorca.dev/docs/model/worktrees) / [Agent hibernation](https://www.onorca.dev/docs/agents/hibernation)）の要約ミラー。
[agent-ide-orca プラグイン（#61）](/product/orchestrator-spec.ja.md) の詳細設計の**単一の根拠**として整備した。
herdr 版（[herdr Socket API ミラー](/references/herdr-socket-api.md)、#60）と対になる。

> ⚠️ orca は日次リリースされる活発な外部ソフトウェア。公開 HTTP/REST API の単体仕様は無く、**CLI ラップが公式推奨パターン**。
> 依存する前に `orca status --json` でランタイム稼働を確認し、各コマンドの `--json` 出力スキーマは実機（`orca <cmd> --help` / 実行）で確認すること（未知フィールドは寛容に扱う）。
> 「orca」は同名プロジェクトが多数存在する。参照時は必ずドメイン（`onorca.dev`）/ リポジトリ（`stablyai/orca`）を確認する。

# 制御モデル（herdr との差分）

| 項目 | orca | 対比: herdr |
|---|---|---|
| トランスポート | **`orca` CLI**（`--json` で構造化出力）。公開ソケット/REST API は無い | Unix ソケット / 名前付きパイプ上の NDJSON |
| 実行単位 | **git worktree**（タスク＝独立チェックアウト）。エージェント端末は worktree にスコープ | workspace / tab / pane |
| エージェント起動 | worktree 端末で任意の CLI エージェントを **TUI プロセス**として起動 | pane 内でプロセスをホスト |
| セッション概念 | 弱いが存在する。**Agent Session History** と **resume フラグ**（`claude --resume` 等）を保持 | 明示的な「セッション開始」メソッドは無い |
| リモート | `orca serve --pairing-address <addr>` ＋ `orca environment add`（ベータ） | ソケットパス解決順で対応 |

# 主要コマンド

セレクタ（`--repo` / `--worktree` 共通）: **`id:<id>`** / **`active`** / **`current`** / **`path:/abs`** / **`branch:<name>`** / **`issue:<n>`**。
リモート環境を指す場合は `active`/`current` ではなくサーバ側の明示セレクタを使う（ローカル FS と誤認されるため）。

| 分類 | コマンド | 用途 / totsuka メソッド対応 |
|---|---|---|
| ランタイム | `orca status --json` | 稼働確認。プラグイン起動時の疎通（F-59 相当） |
| dispatch | `orca worktree create --repo <sel> --name <n> --agent claude --prompt "…" --setup run\|skip\|inherit --json` | **`task/dispatch`**。worktree 作成＋エージェント起動＋初回プロンプト投入を 1 コマンドで。`--issue <n>` で Issue 紐付け可 |
| 一覧 | `orca worktree ps --json` / `orca worktree list --repo <sel> --json` / `orca worktree current --json` / `orca worktree show --worktree <sel> --json` | 冪等性チェック（同名重複回避）・状態取得 |
| メタ | `orca worktree set --worktree <sel> --comment "…" --json` | 補助情報付与 |
| cancel / cleanup | `orca worktree rm --worktree id:<id> --force --json` | **`task/cancel`**・掃除。**CLI が存在**（削除は dir＋branch を確認付きで除去） |
| 入力 | `orca terminal send --terminal <h> --text "…" --enter --json` | 追加ターン投入（構造化 API ではなく端末送信） |
| 出力読取 | `orca terminal read --terminal <h> --cursor <c> --limit 1000 --json` | ログ断片取得（F-38）。カーソルで増分読取 |
| 完了検知 | `orca terminal wait --terminal <h> --for tui-idle --timeout-ms 300000 --json` | **状態検知の主手段**。ただし「idle」は承認待ち停止の可能性もあるため `worktree ps` の state と併用 |
| 端末 | `orca terminal list --json` / `create --command "…"` / `split --direction …` / `close --terminal <h>` | 補助端末（テスト実行等） |
| 定期実行 | `orca automations create --name … --trigger daily\|"cron" --time HH:MM --prompt … --provider claude\|codex --repo <sel> --json`、`run` / `runs` / `edit` / `remove` | orca 側スケジューラ。totsuka 側トリガと二重管理になるため **#61 では未使用**が原則 |

> 補足: 公式 CLI overview は「tracked multi-agent work は plain terminal prompt でなく **Orchestration** を使え」と案内する。
> orca 内蔵の Orchestration 概念があるが、totsuka は自前オーケストレータなので **worktree + terminal の低レベル CLI にマップする**方針。

# 状態検知と totsuka 正規化状態の対応

orca は Claude を **TUI プロセス**として扱い、状態は **Orca が起動時に注入する status-line hook が発行する OSC title イベント → "state dots"** から導出する。
`orca terminal wait --for tui-idle` と `worktree ps` の state はこの信号に依存する。粗い3値（working / done / waiting）で、構造化イベントストリームではない。

| orca 状態（state dots / tui-idle） | totsuka 正規化 | 備考 |
|---|---|---|
| working | `running` | エージェントがターン実行中 |
| waiting（承認/入力待ちで停止） | `waiting_input` | 質問検知（F-35）の起点。OSC 由来のため best-effort |
| done（tui-idle 到達＝ターン完了） | `done` | ただし「承認待ちで idle」と区別が要る → `worktree ps` state 併用 |
| （native failed 状態なし） | `failed` | orca に構造化 `failed` は無い。端末異常終了・timeout 等から導出 |

# Claude Code 固有の制約（要注意 / #61 の「注意」への回答）

[Supported agents マトリクス](https://www.onorca.dev/docs/agents/supported)上、**Claude Code はむしろ最上位の "Deep integration: usage, hot-swap, hooks"**（Codex より hooks の分だけ厚い）。
つまり「Claude だと一部限られる」の実体は Claude 固有の格下げではなく、**orca が全エージェントを TUI プロセスとして扱う構造そのもの**に由来する。#61 の capability 宣言・状態設計はこれを前提にする。

- **構造化 plan / preview API が無い**: orca は plan モード成果物を構造化して返す手段を持たない。よって **`design_preview` capability は宣言しない**
  （#61 の設計例が正しいことを裏付ける）。plan は Claude CLI 側の plan/permission-mode を起動引数で付与して実現する（orca はプロセスホストのみ）。
- **状態は OSC state dots 由来の粗い3値**: Claude の状態は Orca 注入の status-line hook が発行する OSC title に依存。フック非発火時（古い CLI・statusline を上書きする独自設定）は状態が劣化する。native な権威報告ではない。
- **結果は構造化ペイロードで返らない**: dispatch 完了時に構造化された成果物は返らない。**worktree は実 FS 上の実体**なので、`<worktree-path>/…` の出力ファイルを直接読むのが端末 scrollback パースより堅牢。
- **入力は端末送信のみ**: 追加ターンは `terminal send --text … --enter`。構造化 multi-turn API は無い。
- **session/attach（F-37）は成立**: Claude は resumable（`claude --resume <sessionId>`）。orca は launch cmd/args/env と Agent Session History を保持するため、**プラグイン内部のセッション対応表は「弱い吸収」で十分**（herdr のような完全な内製代替は不要）。
- **hibernation の影響**: done かつ idle 30分（既定、1分〜24時間）で自動 hibernate。Claude は resumable なので対象になるが、foreground 復帰で自動 resume。長時間 dispatch では state 正規化が hibernate→resume を跨いでも破綻しないよう扱う。
- **Claude Agent Teams はデフォルト無効**（`orca claude-teams` で有効化）。#61 は **out of scope**。

# 無人実行（Agent Permissions）

Settings → Agents → Agent Permissions は **Yolo** / **Manual** の 2 値。
Yolo は各 CLI の permission-bypass フラグを**事前入力**する — **Claude は `--dangerously-skip-permissions`**（Codex は `--dangerously-bypass-approvals-and-sandbox`、Gemini/Cursor 等は `--yolo`）。
無人 dispatch では Yolo 前提。ただし worktree が使い捨てである設計（実験は隔離チェックアウト内・merge 前に diff 破棄可）に依存する点に留意。個別エージェントの引数を上書きすると、そのエージェントは global 切替の対象外になる。

# リモート / ヘッドレス

- `orca serve --pairing-address <LAN/Tailscale/SSH 先> [--mobile-pairing]` でウィンドウ無しにランタイム起動（フォアグラウンド、Ctrl-C 停止）。ペアリング URL を出力。
- クライアント側は `orca environment add --name <n> --pairing-code '<orca://pair?...>'`、以降 `orca worktree create --environment <n> …`。
- Remote Orca Servers は**ベータ**。サーバ/クライアントは同一ネットワーク経路（LAN・Tailscale・SSH フォワード・トンネル）で疎通必須。
- バックエンド/Web から起動したい場合、公式は「orca のリモート CLI を使うか、サーバ上で CLI を叩く小さな認証済みサービスを自前で用意」を案内。**JSON 出力のパースは薄いアダプタ層に隔離**しておくのが安全（CLI フラグ仕様が変わりうるため）。

# 設計上の注意点（#61 反映用サマリ）

| 項目 | 内容 |
|---|---|
| 冪等性 | dispatch 前に `orca worktree ps --json` で同名 worktree の重複確認 |
| クリーンアップ | `orca worktree rm --worktree id:<id> --force --json`（研究レポート時点の未確認項目が解決） |
| 完了検知 | `terminal wait --for tui-idle` 単独では「承認待ち idle」を誤検知しうる → `worktree ps` state 併用 |
| 結果取得 | worktree 実体ファイルを直接読む（scrollback パースより堅牢） |
| capability | `design_preview` は非宣言。宣言するのは orca CLI で確実に対応できるものだけ（F-33） |
| バージョン変動 | 日次リリース。`--json` パースは薄いアダプタに分離 |

# Citations

1. Orca — CLI reference. https://www.onorca.dev/docs/cli/reference （2026-07-13 参照）
2. Orca — CLI overview. https://www.onorca.dev/docs/cli/overview （2026-07-13 参照）
3. Orca — Claude Code in Orca. https://www.onorca.dev/docs/agents/claude-code （2026-07-13 参照）
4. Orca — Supported agents（permission-bypass フラグ・capability 一覧）. https://www.onorca.dev/docs/agents/supported （2026-07-13 参照）
5. Orca — Settings（Agent Permissions: Yolo/Manual）. https://www.onorca.dev/docs/settings （2026-07-13 参照）
6. Orca — Worktrees（worktree モデル・start-from・rm）. https://www.onorca.dev/docs/model/worktrees （2026-07-13 参照）
7. Orca — Agent hibernation（resume 対象・idle 窓）. https://www.onorca.dev/docs/agents/hibernation （2026-07-13 参照）
8. Orca — Agent hooks & memory. https://www.onorca.dev/docs/agents/hooks-memory （2026-07-13 参照）
