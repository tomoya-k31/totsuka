---
type: Component
title: orchestrator-cli クレート
description: totsuka の CLI エントリポイント（bin: totsuka）。§5.1 のコマンド体系（init / run / status / task / focus / plugin / config / logs / doctor / completion）と共通フラグ（--config / --debug / --json）を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli, plugin, run, status, doctor, hooks]
timestamp: 2026-07-23T12:00:00Z
status: active
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- `plugin`（#52）: `install <dir> [--yes]` / `uninstall <name>` / `enable <name>` / `disable <name>` / `list [--json]`。install は取得元と SHA-256 を表示し確認を要求（§5.4）、GitHub Release からの取得は v1 未対応（ローカルディレクトリからの install に案内）。
- `run [--watch] [--dry-run]`（#63）: メインループの CLI 表面。設定ロード→`config::validate`（Error があれば起動拒否）→ログ初期化（§5.2）→単一インスタンスロック（F-74、dry-run は読み取り専用のため取得しない）→**フックアセットの書き出し**（core の `hooks::install` 呼び出し、後述、#137/#178）→enabled プラグインを store から起動（起動スペック組み立てとシークレット解決は core の `plugins::spec::plugin_spec` 呼び出し、F-58/64/65、#217）→起動時回復（§5.3、再開不能タスクは `task retry/cancel` を案内）→孤児 worktree 警告（F-24）→[orchestrator-core の run Engine](/components/orchestrator-core.md) に委譲。終了時に summary（fetched/ingested/dispatched/done/failed と waiting/pending/queued の残タスク）を表示。SIGINT は graceful（実行中タスクは状態DBに残し次回回復）。
- `init`（#64）: config.toml 雛形（コメントアウト済みテンプレート）と XDG ディレクトリの生成 + git バージョン確認。既存ファイルは決して上書きしない。
- `status [--json]`（#64）: タスク/worktree 一覧と orchestrator 生存表示。SQLite 直読でプラグインを起動しない（§5.5）。run.lock の PID 生存確認で「not running (stale lock)」を明示（F-74）。
- `task list|show|cancel|retry|verify <id> [--json]`（#64/#138）: `show` は状態・セッション履歴・worktree・イベント全履歴（`StateDb::list_events`）。`cancel`/`retry` は状態DBへのステートマシン遷移で、エージェントセッションとスロットは次回 `run` の回復/再利用（F-44）が引き受ける。retry は failed/cancelled のみ受け付ける。`verify <id> --pass`（`ApproveVerification`→Publishing、次 `run` の recover で publish）/ `--fail --reason <text>`（`VerificationFailed`→Running）は `verification = "human"` の検収（`verifying` 状態のみ受付、D-01/D-07、#138）。
- `focus <task-id>`（#155, F-94）: 通知クリックの実行先（terminal-notifier `-execute` が呼ぶ）。実行中 Orchestrator の hook/制御 UDS へ [`POST /focus`](/apis/claude-events.md) し、対象タスクの pane を前面化する（pane フォーカスは Orchestrator 所有のプラグイン経由が唯一の整合経路 = session_id 不透明契約 F-37、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。**縮退は常に静か（exit 0）**: 設定なし・Orchestrator 停止中（socket 無し）・pane 消失はいずれも短い note を出して正常終了する — クリック経路を壊さない（アプリ前面化は `-activate` が別途担う）。socket パス解決は doctor のプローブと共通ヘルパ（`common::hook_socket_path`）。
- `config validate [--offline] / show [--redacted]`（#64）: validate はオフライン検証（schema/静的参照/ワークフロー意味論）+ `--offline` でなければ enabled プラグインを一時起動して `config/validate` を委譲（F-59/63）。show は config.toml と plugins/*.toml を表示し、`--redacted` で token/secret/password/api_key を含むキーの値をマスク。
- `logs [-f] [--task <id>]`（#64): JSON Lines ログ（§5.2）の整形表示・追尾（日次ローテーション追随）・タスク別フィルタ。
- `doctor [--json]`（#64/#141）: git / config / state DB / **hooks（core の `hooks::install` によるアセット書き出し + フック系プローブ一式、後述）** / プラグイン（インストール+ライブ疎通 probe）/ LLM キー解決 / 孤児 worktree（F-24、TTY では対話確認つき掃除提案）。失敗チェックは「原因 + 次のアクション」で報告し非ゼロ終了。`doctor` は `run` と同じ書き出しを実行するため、フル run なしでフック一式をマテリアライズする手段も兼ねる。
- `completion <shell>`: clap_complete によるシェル補完生成（zsh / bash / fish 等）。

# フックアセットの書き出し（#137、#178 で core へ移動）

フックスクリプト + workflow 別 settings のレンダリングサブシステム（旧 `src/hooks/`、エピック #131 の描画側）は **#178 で [orchestrator-core](/components/orchestrator-core.md) の `hooks` モジュールへ移動した**（描画・受信・マーカー定数を単一クレートに閉じるため。詳細は core 側の `hooks` 行を参照）。CLI に残るのは薄い呼び出しのみ:

- `run` / `doctor` 起動時に `orchestrator_core::hooks::install`（スクリプト 0700 + settings 0600 の冪等書き出し）を呼ぶ。
- `run` は `orchestrator_core::hooks::settings_path` で workflow 別 `--settings` パスを引いて `HookRuntime.settings_paths` を組み立てる。
- `doctor` の `check_hook_assets` は `install`（自己修復）→ `verify_assets`（書き込みなし検査）の結果を `Check` へ変換する（後述）。

## doctor のフック系プローブ（#141）

`doctor` に**フック機構専用のプローブ**を追加する（既存の `hooks` アセットチェックを複製せず**拡張**する形。既存の `Check::ok`/`Check::fail`「原因 + 次のアクション」パターンに従う）。詳細な切り分け手順は [フックのトラブルシューティング](/operations/hook-troubleshooting.md)。

- `check_hook_socket` — UDS への**自己 POST → 200**（受信サーバ・Bearer・0600 権限の疎通）。
- `check_hook_assets` — スクリプト + `orchestrator-*.json` の存在・**0700/0600 パーミッション**・**内容ハッシュ一致**（既存の `hooks` アセットチェックを拡張。実体は core の `hooks::install` / `hooks::verify_assets` 呼び出し、#178）。
- `check_hook_token` — `[hooks].auth_token_ref` が解決できる（keychain/env 参照切れの検出）。**#209 で未設定の扱いを条件付きに変更**: `cfg.workflows` の `agent` を静的マニフェストで引き、`Capabilities::hook_capable()`（= `resume_session || diagnostics_snapshot`）な agent を使う workflow が 1 つでもあれば **`Check::fail`**（該当 workflow / agent 名を detail に列挙）、無ければ従来どおり `Check::warn`。doctor で唯一、構成によって severity が変わるチェック。plugin の enabled 状態や参照整合性は既存の validate / `plugin:*` チェックの責務としてここでは重ねない。
- `check_hook_deps` — `curl` + `jq` の存在（H-14。無いとフックが送信不能で全て spool 行き）。
- `check_spool` — `spool_dir` の書き込み可否と**バックログ件数**（backlog > 0 は warning、[hook-security](/security/hook-security.md) N-05 の滞留検出）。

- 共通フラグ: `--config <path>`（設定ファイル上書き = F-66 の最上位レイヤ）、`--debug`（run のログレベルを debug に引き上げ）。`--json` は全読み取り系コマンドに用意。
- **設定ロードの一元化（#208、[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)）**: `Cx::load_config(&env)` が `config.toml` パース → core の `apply_env_overrides`（F-66 第 2 層 `TOTSUKA_*`）まで行い、`run` / `config` / `focus` / `doctor` の 4 コマンドすべてがここを通る。**片方だけに適用しない**理由は `focus` / `doctor` が `[hooks].socket_path` から `run` のバインドしたソケットを解決するためで、`run` のみだと `TOTSUKA_HOOKS_SOCKET_PATH` 設定時に別のソケットを見る。警告は stderr（`--json` の stdout 契約を壊さない）。CLI フラグ（`--debug`）は**この後**に適用されるため「CLI > env」が適用順で成立する。例外は `plugin enable`/`disable` のローカルローダで、ファイル編集用のため raw のまま維持（env で編集結果を汚染しない）。`config show` はファイル内容表示を維持しつつ、有効な env オーバーライドを末尾に一覧表示する（`--redacted` 時は `is_secret_key` で値をマスク）。
- UX 規約（§7）: エラーは「原因 + 次のアクション」（`→` 区切り）。用語は [glossary](/glossary/index.md) に準拠。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
