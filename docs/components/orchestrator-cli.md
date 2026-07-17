---
type: Component
title: orchestrator-cli クレート
description: totsuka の CLI エントリポイント（bin: totsuka）。§5.1 のコマンド体系（init / run / status / task / plugin / config / logs / doctor / completion）と共通フラグ（--config / --debug / --json）を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli, plugin, run, status, doctor, hooks]
timestamp: 2026-07-18T12:00:00Z
status: active
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- `plugin`（#52）: `install <dir> [--yes]` / `uninstall <name>` / `enable <name>` / `disable <name>` / `list [--json]`。install は取得元と SHA-256 を表示し確認を要求（§5.4）、GitHub Release からの取得は v1 未対応（ローカルディレクトリからの install に案内）。
- `run [--watch] [--dry-run]`（#63）: メインループの CLI 表面。設定ロード→`config::validate`（Error があれば起動拒否）→ログ初期化（§5.2）→単一インスタンスロック（F-74、dry-run は読み取り専用のため取得しない）→**フックスクリプト + settings のレンダリング**（後述、#137）→enabled プラグインを store から起動（`plugins/{name}.toml` のシークレット解決済み設定を `initialize` へ、F-58/64/65）→起動時回復（§5.3、再開不能タスクは `task retry/cancel` を案内）→孤児 worktree 警告（F-24）→[orchestrator-core の run Engine](/components/orchestrator-core.md) に委譲。終了時に summary（fetched/ingested/dispatched/done/failed と waiting/pending/queued の残タスク）を表示。SIGINT は graceful（実行中タスクは状態DBに残し次回回復）。
- `init`（#64）: config.toml 雛形（コメントアウト済みテンプレート）と XDG ディレクトリの生成 + git バージョン確認。既存ファイルは決して上書きしない。
- `status [--json]`（#64）: タスク/worktree 一覧と orchestrator 生存表示。SQLite 直読でプラグインを起動しない（§5.5）。run.lock の PID 生存確認で「not running (stale lock)」を明示（F-74）。
- `task list|show|cancel|retry|verify <id> [--json]`（#64/#138）: `show` は状態・セッション履歴・worktree・イベント全履歴（`StateDb::list_events`）。`cancel`/`retry` は状態DBへのステートマシン遷移で、エージェントセッションとスロットは次回 `run` の回復/再利用（F-44）が引き受ける。retry は failed/cancelled のみ受け付ける。`verify <id> --pass`（`ApproveVerification`→Publishing、次 `run` の recover で publish）/ `--fail --reason <text>`（`VerificationFailed`→Running）は `verification = "human"` の検収（`verifying` 状態のみ受付、D-01/D-07、#138）。
- `config validate [--offline] / show [--redacted]`（#64）: validate はオフライン検証（schema/静的参照/ワークフロー意味論）+ `--offline` でなければ enabled プラグインを一時起動して `config/validate` を委譲（F-59/63）。show は config.toml と plugins/*.toml を表示し、`--redacted` で token/secret/password/api_key を含むキーの値をマスク。
- `logs [-f] [--task <id>]`（#64): JSON Lines ログ（§5.2）の整形表示・追尾（日次ローテーション追随）・タスク別フィルタ。
- `doctor [--json]`（#64/#141）: git / config / state DB / **hooks（フックスクリプト + settings のレンダリング + フック系プローブ一式、後述）** / プラグイン（インストール+ライブ疎通 probe）/ LLM キー解決 / 孤児 worktree（F-24、TTY では対話確認つき掃除提案）。失敗チェックは「原因 + 次のアクション」で報告し非ゼロ終了。`doctor` は `run` と同じレンダリングを実行するため、フル run なしでフック一式をマテリアライズする手段も兼ねる。
- `completion <shell>`: clap_complete によるシェル補完生成（zsh / bash / fish 等）。

# フックスクリプト + orchestrator settings のレンダリング（#131 H-01/H-03, #137）

Claude Code の完了判定を screen-manifest からフック機構へ置換する基盤（エピック #131）の CLI 側実装。`src/hooks/` に module 化。

- **静的スクリプト 5 本**（`hook-common.sh` / `on-stop.sh` / `on-notification.sh` / `on-session-start.sh` / `on-session-end.sh`）を `include_str!` でバイナリ同梱し、`run` / `doctor` 起動時に `$XDG_DATA_HOME/totsuka/hooks/` へ **0700・内容ハッシュ冪等**で書き出す（SHA-256 一致なら書き換えず、バージョンアップ時のみリフレッシュ）。
- **workflow 別 `orchestrator-<workflow>.json`** を同ディレクトリへ **0600** でレンダリング。`Stop` には常に `on-stop.sh`（command 型）を配線し、`verification = "llm"` の workflow のみ `prompt` 型フック（`workflow.rubric` か既定 rubric + マーカー規約）を追加する（D-01）。`Notification` / `SessionStart` / `SessionEnd` は各 command 型フック。
- **ジョブ固有値はファイルに書かない**。job_id / エンドポイント / トークン / スプールディレクトリは env（`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` / `TOTSUKA_HOOK_SPOOL_DIR`）で [agent-ide-herdr](/components/agent-ide-herdr.md) が注入する（[plugin-protocol](/components/plugin-protocol.md) 0.1.3 `HookLaunchSpec`）。これにより同一の `--settings` パスが `claude --resume` を跨いで再利用できる（H-03）。
- `on-stop.sh` は `set -uo pipefail`（`-e` は使わない = fail-open D-09）。①`jq`/`curl` 欠落 → 生 JSON をスプールへ追記し exit 0 ②`background_tasks` 非空 → `heartbeat` を POST し exit 0（中間 Stop、R-02）③`last_assistant_message` の**最後の** `<<STATUS:...>>` マーカー（`reason="..."` 属性込み、D-12）を抽出し `stop`/該当 status を POST ④マーカー欠落 & `stop_hook_active != true` → `UNKNOWN` を POST の上、stdout に `{"decision":"block",...}`（是正方法つき、R-03）⑤マーカー欠落 & `stop_hook_active == true` → `UNKNOWN` の POST のみ（再 block しない）。POST は `hook-common.sh` の `post_event`（`curl --unix-socket` + Bearer + `--max-time 5 --retry 2`）で行い、失敗時は NDJSON 1 行を `$TOTSUKA_HOOK_SPOOL_DIR/<epoch>-<pid>.jsonl` へ追記（E-07/H-11）。ペイロードは常に compact JSON（`jq -c`）で NDJSON 1 行 = 1 オブジェクトを保つ。stdout は block JSON 以外を出さない（H-13）。
- 回収側（スプールの再投入 = Engine 統合）は本コンポーネントのスコープ外（#138）。

## doctor のフック系プローブ（#141）

`doctor` に**フック機構専用のプローブ**を追加する（既存の `hooks` アセットチェックを複製せず**拡張**する形。既存の `Check::ok`/`Check::fail`「原因 + 次のアクション」パターンに従う）。詳細な切り分け手順は [フックのトラブルシューティング](/operations/hook-troubleshooting.md)。

- `check_hook_socket` — UDS への**自己 POST → 200**（受信サーバ・Bearer・0600 権限の疎通）。
- `check_hook_assets` — スクリプト + `orchestrator-*.json` の存在・**0700/0600 パーミッション**・**内容ハッシュ一致**（既存の `hooks` アセットチェックを拡張）。
- `check_hook_token` — `[hooks].auth_token_ref` が解決できる（keychain/env 参照切れの検出）。
- `check_hook_deps` — `curl` + `jq` の存在（H-14。無いとフックが送信不能で全て spool 行き）。
- `check_spool` — `spool_dir` の書き込み可否と**バックログ件数**（backlog > 0 は warning、[hook-security](/security/hook-security.md) N-05 の滞留検出）。

- 共通フラグ: `--config <path>`（設定ファイル上書き = F-66 の最上位レイヤ）、`--debug`（run のログレベルを debug に引き上げ）。`--json` は全読み取り系コマンドに用意。
- UX 規約（§7）: エラーは「原因 + 次のアクション」（`→` 区切り）。用語は [glossary](/glossary/index.md) に準拠。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
