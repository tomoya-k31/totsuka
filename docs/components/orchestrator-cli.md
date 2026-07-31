---
type: Component
title: orchestrator-cli クレート
description: "totsuka の CLI エントリポイント（bin: totsuka）。§5.1 のコマンド体系（init / run / status / task / focus / plugin / config / logs / doctor / completion）と共通フラグ（--config / --debug / --json）を提供する。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli, plugin, run, status, doctor, hooks, security]
generated: { by: human:tomoya-k31, at: 2026-07-28T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- `plugin`（#52）: `install <dir> [--yes] [--enable]` / `install --bundled <name>|--all [--yes] [--enable]` / `uninstall <name>` / `enable <name>` / `disable <name>` / `list [--json]`。install は取得元と SHA-256 を表示し確認を要求（§5.4）、GitHub Release からの取得は v1 未対応（ローカルディレクトリからの install に案内）。
  - **`--bundled`（#345）**: リリース tarball が `totsuka` の隣に置く `plugins/<name>/{<name>, plugin.toml}` からパス指定なしで入れる。探索順は「起動に使われたパスの隣」→「symlink 解決後のパスの隣」の各々について `plugins/` → `../libexec/totsuka/plugins/`（後者は将来の prefix 形式インストール用）。採用したツリーは必ず表示する。**`std::env::current_exe` は macOS で symlink を解決しない**（`_NSGetExecutablePath` は起動に使われたパスを返す。Linux の `/proc/self/exe` とは異なる）ため、`fs::canonicalize` の結果も明示的に探索する — 標準のインストール形（`/usr/local/bin/totsuka` → `/usr/local/lib/totsuka/totsuka` の symlink、プラグインは**リンク先**の隣）はこれが無いと 1 つも見つからない。同梱ツリーが無いのは `cargo install` 由来のビルドでは正常なので、エラー文はディレクトリ install へ案内する。上書き用の `--bundled-dir` は hidden（env 変数を使わないのは、未知の `TOTSUKA_*` が stderr に警告を出し（[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)）stderr を JSON として読む E2E を壊すため）。
  - **`--enable`**: install と enable の概念分離（F-56）は保ったまま、1 コマンドで両方やるオプトイン。`<dir>` 指定でも `--bundled` でも使える。書き込みは `plugin enable` と同じ raw テキスト編集なので、コメント・整形は維持される。**#175: パス解決・設定ロードは他コマンドと同じ `Cx` 経由**（独自の `Locations` は廃止。`--config` / `TOTSUKA_*` env オーバーライドが plugin コマンドにも効く）。設定欠落時: `install` / `uninstall` / `list` は**空設定で続行**（`Cx::load_config_or_default` — 宣言の照合にしか使わないため `totsuka init` 前でも動く）、`enable` / `disable` は**エラー**（編集対象ファイルが無いため。`→ run totsuka init` を案内）。
- `run [--watch] [--dry-run]`（#63）: メインループの CLI 表面。設定ロード→`config::validate`（Error があれば起動拒否）→ログ初期化（§5.2）→単一インスタンスロック（F-74、dry-run は読み取り専用のため取得しない）→**フックアセットの書き出し**（core の `hooks::install` 呼び出し、後述、#137/#178）→enabled プラグインを store から起動（起動スペック組み立てとシークレット解決は core の `plugins::spec::plugin_spec` 呼び出し、F-58/64/65、#217）→起動時回復（§5.3、再開不能タスクは `task retry/cancel` を案内）→孤児 worktree 警告（F-24）→[orchestrator-core の run Engine](/components/orchestrator-core.md) に委譲。終了時に summary（fetched/ingested/dispatched/done/failed と waiting/pending/queued の残タスク）を表示。SIGINT は graceful（実行中タスクは状態DBに残し次回回復）。
- `init`（#64）: config.toml 雛形（コメントアウト済みテンプレート）と XDG ディレクトリの生成 + git バージョン確認。既存ファイルは決して上書きしない。
- `status [--json]`（#64）: タスク/worktree 一覧と orchestrator 生存表示。SQLite 直読でプラグインを起動しない（§5.5）。run.lock の PID 生存確認で「not running (stale lock)」を明示（F-74）。
- `task list|show|cancel|retry|verify <id> [--json]`（#64/#138）: `show` は状態・セッション履歴・worktree・イベント全履歴（`StateDb::list_events`）に加え、**#263（#242）で会話履歴**（`StateDb::list_task_messages` を時系列。1 タスク = 1 [会話](/glossary/conversation-continuity.md)になった以上「そのタスクに何が届いたか」が見えないと監査・デバッグができない）。各行は受信時刻・author・本文プレビューと、エージェントへ渡したか（`→`）まだキューにあるか（`·`）の印。表示は非正規化列（`author`/`body`/`url`）だけを使い **`payload` の JSON をパースしない**（このリポジトリに JSON 検索のロジック（`json_extract` / `LIKE`）は 1 件も存在せず、増やさない方針。`list_task_messages` の SELECT に `payload` 列自体は含まれるが、CLI はそれを触らない）。本文は端末表示でのみ 72 **文字**（バイトではない — 日本語本文が常態）に切り詰め、`--json` には全文が載る。メッセージ 0 件のタスク（1 メッセージ = 1 タスクのソース）では節ごと出ない。`cancel`/`retry` は状態DBへのステートマシン遷移で、エージェントセッションとスロットは次回 `run` の回復/再利用（F-44）が引き受ける。retry は failed/cancelled のみ受け付ける（**#263: `done` に対する両コマンドの案内文を「会話に次のメッセージを送れば継続する」へ統一** — `cancel` が `task retry` を案内する一方 `retry` は `done` を拒否する不整合があり、#242 で `done` の意味が「未処理メッセージが無い」に変わって案内自体も誤りになったため）。`verify <id> --pass`（`ApproveVerification`→Publishing、次 `run` の recover で publish）/ `--fail --reason <text>`（`VerificationFailed`→Running）は `verification = "human"` の検収（`verifying` 状態のみ受付、D-01/D-07、#138）。
- `focus <task-id>`（#155, F-94）: 通知クリックの実行先（terminal-notifier `-execute` が呼ぶ）。実行中 Orchestrator の hook/制御 UDS へ [`POST /focus`](/apis/agent-events.md) し、対象タスクの pane を前面化する（pane フォーカスは Orchestrator 所有のプラグイン経由が唯一の整合経路 = session_id 不透明契約 F-37、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。**縮退は常に静か（exit 0）**: 設定なし・Orchestrator 停止中（socket 無し）・pane 消失はいずれも短い note を出して正常終了する — クリック経路を壊さない（アプリ前面化は `-activate` が別途担う）。socket パス解決は doctor のプローブと共通ヘルパ（`common::hook_socket_path`）。
- `config validate [--offline] / show [--redacted]`（#64）: validate はオフライン検証（schema/静的参照/ワークフロー意味論 + **enabled プラグインのマニフェスト健全性**）+ `--offline` でなければ enabled プラグインを一時起動して `config/validate` を委譲（F-59/63）。**#214**: インストール済みだが `plugin.toml` がパース不能な enabled プラグインは `--offline` でも **error**（exit 非 0）— マニフェスト読み取りはプラグイン起動を伴わないため F-63（`--offline` = プロセスを立ち上げない）は保たれる。「未インストール（capability 不明 → 該当 advisory をスキップ）」とは区別される。このオフライン検証一式は `Cx::validate_config` に集約され、`config validate` / `run` / `doctor` の 3 コマンドが同一の判定を共有する。show は config.toml と plugins/*.toml を表示し、`--redacted` で token/secret/password/api_key を含むキーの値をマスク。
- `logs [-f] [--task <id>]`（#64): JSON Lines ログ（§5.2）の整形表示・追尾（日次ローテーション追随）・タスク別フィルタ。
- `doctor [--json] [--online]`（#64/#141/#267）: git / config / state DB / **同梱プラグイン（`bundled-plugins`。バイナリの隣に何が同梱されているかを表示。`cargo install` 由来のビルドでは同梱ゼロが正常なので、検出できない場合も含めて重大度は warning 止まり = `ok: true` で終了コード契約 0/1/3 は不変）** / **worktree 配置テンプレート（`worktree-location`。明示された `[worktree].location` と `[[repositories]].worktree_location` の `${ENV}` 展開可否のみを検査。既定値は `Paths` 由来で常に解決するのでスキップ。worktree 作成はディスパッチ時なので、未設定変数を放置すると run は正常起動したまま全タスクが `fail_dispatch` する — `check_spool` と同型の事前検出）** / **hooks（core の `hooks::install` によるアセット書き出し + フック系プローブ一式、後述）** / プラグイン（インストール+ライブ疎通 probe）/ **LLM キー（`llm` = 参照の解決可否のみ。`--online` 時のみ `llm-online` = プロバイダが鍵を受理するかの実測。後述）** / 孤児 worktree（F-24、TTY では対話確認つき掃除提案）/ **孤児 pane（#211、[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)。`pane_control` 宣言の agent_ide プラグインを launch → protocol 0.2.2 `session/list` → shutdown で列挙し、`classify_orphan_panes`（純関数・ユニットテスト済み）が label の **source task id**（プロトコル `Task.id` = `source_task_id`、DB 行 id ではない）を文字列照合で DB と突き合わせ — 候補 = 「DB 未知」または「一致する全タスクが終端かつ live worktree なし」、非終端と保持中 worktree の pane は除外（複数一致は保守側に倒す）。TTY では 1 件ずつ `session/release`（列挙した label を `expect_label` の同一性ガードに使用）による解放を提案、`--json`/非 TTY は `panes` チェックの fail で検出のみ報告。対象プラグインが無い構成ではチェック自体を出さず、列挙失敗は warning に留める）**。失敗チェックは「原因 + 次のアクション」で報告し **exit 3**（問題検出。doctor 自体の実行失敗 = 1 と区別、#177）で終了。`doctor` は `run` と同じ書き出しを実行するため、フル run なしでフック一式をマテリアライズする手段も兼ねる。
- `completion <shell>`: clap_complete によるシェル補完生成（zsh / bash / fish 等）。

**doctor の非対話ゲート（#289、[ADR-0019](/decisions/adr-0019-doctor-onepassword-gating.md)）**: `check_onepassword` が**最初に**走り、`op whoami`（プロンプトを出さない）の結果を `OpReadiness`（`NotUsed` / `Ready` / `WouldPrompt`）として後続へ渡す。`WouldPrompt`（`op` が無い・セッション無し）のとき、`op://` の実解決を要する probe — `plugin:{name}`（`plugin_spec` が `plugins/{name}.toml` の全文字列 leaf と task_source の `[llm].api_key_ref` を解決する）、`hook-socket`（`auth_token_ref`）、`panes`（プラグイン起動）— は**実行せず `skipped` として報告**する。判定は**プラグイン単位**（1 つのプラグインの `op://` が他を巻き添えにしない）で、kind はマニフェストではなく config のロスターから読む（プラグインに触れる前に決める必要があるため）。`Check` の 4 つ目の重大度 `skipped` は `ok: true`（exit code に影響しない）かつ `skip_serializing_if` なので `--json` は後方互換。**セッションがあれば従来どおり全て走る。**

**外部由来テキストの無害化（#280 / #297）**: `task list` / `task show` / `status` / `logs` / `doctor` の **human 出力**は、第三者が内容を決められるフィールド（`title` / `body` / `author` / `url` / `source_task_id` / `branch` / `worktree_path` / `session_id` / ログの `message`）を `common::safe()` に通してから印字する。制御文字を**除去ではなく可視のエスケープへ**置き換えるため、`ESC[2J` による画面消去も `ESC[1A` による既印字行の上書き（別タスクの state の偽装）も OSC 8 によるリンク偽装も成立しない。**`--json` は通さない** — `serde_json` が既に `\u00xx` へエスケープしており、重ねると二重エスケープで機械可読値が壊れる。`TaskDetail` / `TaskRow` の構築は JSON 分岐より前にあるので、無害化は**分岐より後の print サイトだけ**に置く（構造体側でやると `--json` を巻き込む）。詳細は [端末出力の信頼境界](/security/terminal-output-sanitization.md)。

**#297 で `doctor` を追加**: pane label は `totsuka {source_task_id}`（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）＝外部が決める id を含み、孤児 worktree のパスは title 由来のブランチ名を含む（`render_branch` が畳むのは `Cc` だけで bidi override は素通りする）。`doctor` は障害調査でこそ読まれるコマンドなので出力を信じたい場面で使われる。適用は **`--json` 分岐より後の human レンダリングループ 1 箇所**（`Check` の `name`/`detail`/`action`）と、TTY 時の対話プロンプト（削除/解放の y/N を答える行そのもの）に置く。これにより git の stderr・tmux / プラグインのエラー文が乗る他の `Check` も同時に覆われる。`safe()` の実体は #297 で `orchestrator_core::terminal` へ移り、`common::safe` はその re-export になった（core 自身の stderr ログ層も同じ関数を使うため）。

# フックアセットの書き出し（#137、#178 で core へ移動）

フックスクリプト + workflow 別 settings のレンダリングサブシステム（旧 `src/hooks/`、エピック #131 の描画側）は **#178 で [orchestrator-core](/components/orchestrator-core.md) の `hooks` モジュールへ移動した**（描画・受信・マーカー定数を単一クレートに閉じるため。詳細は core 側の `hooks` 行を参照）。CLI に残るのは薄い呼び出しのみ:

- `run` / `doctor` 起動時に `orchestrator_core::hooks::install`（スクリプト 0700 + settings 0600 の冪等書き出し）を呼ぶ。
- `run` は `orchestrator_core::hooks::settings_path` で workflow 別 `--settings` パスを引いて `HookRuntime.settings_paths` を組み立てる。
- `doctor` の `check_hook_assets` は `install`（自己修復）→ `verify_assets`（書き込みなし検査）の結果を `Check` へ変換する（後述）。

## doctor のフック系プローブ（#141）

`doctor` に**フック機構専用のプローブ**を追加する（既存の `hooks` アセットチェックを複製せず**拡張**する形。既存の `Check::ok`/`Check::fail`「原因 + 次のアクション」パターンに従う）。詳細な切り分け手順は [フックのトラブルシューティング](/operations/hook-troubleshooting.md)。

プローブの実装は `check_*` 関数、`doctor --json` の `.name` に出るチェック名は括弧内:

- `check_hook_socket`（`hook-socket`） — UDS への**自己 POST → 200**（受信サーバ・Bearer・0600 権限の疎通）。
- `check_hook_assets`（`hooks`） — スクリプト + `orchestrator-*.json` の存在・**0700/0600 パーミッション**・**内容ハッシュ一致**（既存の `hooks` アセットチェックを拡張。実体は core の `hooks::install` / `hooks::verify_assets` 呼び出し、#178）。
- `check_hook_token`（`hook-token`） — `[hooks].auth_token_ref` が解決できる（keychain/env 参照切れの検出）。**#209 で未設定の扱いを条件付きに変更**: `cfg.workflows` の `agent` を静的マニフェストで引き、`Capabilities::hook_capable()`（= `resume_session || diagnostics_snapshot`）な agent を使う workflow が 1 つでもあれば **`Check::fail`**（該当 workflow / agent 名を detail に列挙）、無ければ従来どおり `Check::warn`。**#214**: agent のマニフェストがパース不能で capability を判定できなかった workflow は「hook 非対応」扱いに沈黙させず、warn の detail に **capability 不明**として明示する（マニフェスト破損だけで fail → warn への静かな格下げが起きないようにするため。破損自体は `config` チェックと `plugin:*` チェックが fail として捕捉する）。doctor で唯一、構成によって severity が変わるチェック。plugin の enabled 状態や参照整合性は既存の validate / `plugin:*` チェックの責務としてここでは重ねない。
- `check_hook_deps`（`hook-deps`） — `curl` + `jq` の存在（H-14。無いとフックが送信不能で全て spool 行き）。
- `check_spool`（`hook-spool`） — `spool_dir` の書き込み可否と**バックログ件数**（backlog > 0 は warning、[hook-security](/security/hook-security.md) N-05 の滞留検出）。

## doctor --online: LLM キーのライブプローブ（#267）

`check_llm_key`（`llm`）が答えられるのは「**シークレット参照が解決できるか**」だけで、「**その鍵が API に受理されるか**」ではない。両者は無関係で、実機では `op://` 参照が正しく解決する裏でプロバイダが全リクエストに 401 を返し続けていた。決定の全文は [ADR-0016](/decisions/adr-0016-doctor-online-probe.md)。

- 既定の `doctor` は不変（オフライン・非対話・`op://` を解決しない、[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)）。`--online` を明示したときだけ `check_llm_online`（`llm-online`）が走る。
- 実体は core の **`OpenAiRouter::probe_auth`** — `LlmRouter` の本経路を通さない専用メソッドで、**json_schema を送らず**（構造化出力の受理形はプロバイダ差が大きく、拒否された schema の 400 が「鍵が悪い」に化ける）、**リトライせず**（`max_retries = 0`。不調なプロバイダ相手に doctor が固まらない）、`max_tokens: 1`、レスポンス本文は破棄する（2xx = 鍵が受理された、が問い全体）。
- severity は **401/403 のみ `Check::fail`**。タイムアウト・トランスポート・429・5xx は `Check::warn` に留める — ネットワークの不調で exit 3（[ADR-0012](/decisions/adr-0012-cli-exit-codes-json-errors.md)）になると赤信号が「設定が壊れている」の意味を失う。
- オフラインの `llm` が解決に失敗している場合はプローブしない（投げる鍵が無く、同じ失敗を言い直すだけ）。
- **`--online` は `op://` を実際に解決する**ため生体認証プロンプトが出うる。`--help` にも明記しており、CI / cron からは使わない。
- 共通フラグ: `--config <path>`（設定ファイル上書き = F-66 の最上位レイヤ）、`--debug`（**#176: 全コマンドで有効** — `run` 以外は stderr のみの debug 診断（`logging::init_stderr`、ログファイルは作らない）、`run` は従来どおりファイルログのレベルも debug に引き上げ。global フラグが `run` 以外で無視される clig.dev アンチパターンの解消）。`--json` は主要読み取り系コマンド（status / task list / task show / plugin list / doctor）に用意し、宣言は `common::JsonFlag`（`#[command(flatten)]`）で一元化（#177）。
- **設定ロードの一元化（#208 → #175、[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)）**: `Cx::load_config(&env)` が `config.toml` パース → core の `apply_env_overrides`（F-66 第 2 層 `TOTSUKA_*`）まで行う。**#175 で `plugin` サブコマンド群の独自ローダ（`Locations`）を廃止**し、設定を読むコマンドはすべてここを通る。**片方だけに適用しない**理由は `focus` / `doctor` が `[hooks].socket_path` から `run` のバインドしたソケットを解決するためで、`run` のみだと `TOTSUKA_HOOKS_SOCKET_PATH` 設定時に別のソケットを見る。警告は stderr（`--json` の stdout 契約を壊さない）。CLI フラグ（`--debug`）は**この後**に適用されるため「CLI > env」が適用順で成立する。**設定欠落時のセマンティクスは 2 API で明示的に選ぶ**: `load_config`（欠落 = 「原因 + 次のアクション」エラー。run / config / focus / doctor）と `load_config_or_default`（欠落 = 空設定で続行。plugin install / uninstall / list — `init` 前でも動くべきコマンドのみ。`TOTSUKA_*` レイヤは欠落時も適用されるため、不正なオーバーライド値はファイル有無によらずエラー）。なお `plugin enable`/`disable` は編集対象ファイルの raw テキスト読み（欠落 = エラー）+ `set_plugin_enabled` のまま（コメント・整形を維持し、env レイヤを書き戻さない。宣言済みチェックも同じ raw テキストのパースで行う）。`config show` はファイル内容表示を維持しつつ、有効な env オーバーライドを末尾に一覧表示する（`--redacted` 時は `is_secret_key` で値をマスク）。
- UX 規約（§7）: エラーは「原因 + 次のアクション」（`→` 区切り）。用語は [glossary](/glossary/index.md) に準拠。
- **exit code 体系と機械可読エラー（#177、[ADR-0012](/decisions/adr-0012-cli-exit-codes-json-errors.md)）**: exit code は名前付き定数（`common.rs`）で 4 値 — **0** = 成功 / **1** = 実行時エラー（`EXIT_ERROR`）/ **2** = usage エラー（`EXIT_USAGE`。サブコマンド無し + clap 自身のパース失敗）/ **3** = 診断完走で問題検出（`EXIT_PROBLEMS_FOUND`、現状 `doctor` のみ — 「doctor 自体の失敗 = 1」と区別）。特定 code は `ExitWith { code, message }` を `main` が downcast して取り出す。`--json` 指定時のエラーは stderr へ 1 行 compact JSON `{"error":{"message":"<原因>","action":"<次のアクション>"|null}}`（既存文言の最初の ` → ` で分割）、非 `--json` は従来の `error: 原因 → アクション` 平文。stdout の「parseable output, nothing else」契約はエラー時も不変。`focus` は従来どおり常に exit 0（クリック経路を壊さない）。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
