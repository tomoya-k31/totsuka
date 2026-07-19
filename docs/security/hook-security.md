---
type: Policy
title: Claude Code フック機構のセキュリティポリシー
description: フック完了判定の UDS Bearer トークン管理（keychain 参照・socket 0600 第一層・定数時間比較・herdr env 配送）、スプールファイルの機密保持（N-05: last_assistant_message は機微・$XDG_STATE_HOME 配下・drain 後削除・隔離の注意）、フックアセットの改ざん耐性（N-02: 0700/0600・内容ハッシュ冪等修復・静的埋め込み）を定める。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [security, hook, claude-code, uds, token, keychain, spool, tamper, epic-131]
timestamp: 2026-07-19T12:00:00Z
status: active
owner: tomoya-k31
---

# 前提: フック機構が導入する新しい攻撃面

Claude Code の完了判定は、pane 内の `claude` が発火するフックスクリプトから Unix ドメインソケットへ POST する経路で成立する（[F-100〜F-107](/product/orchestrator-spec.ja.md)、[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)、フロー: [hook-signal-flow](/architecture/hook-signal-flow.md)）。これにより 3 つの守るべき資産が生じる: **UDS への認証**、**スプールに残る機密**、**フックアセットの完全性**。関連する設定は `[hooks]`（`auth_token_ref` / `socket_path` / `spool_dir` / `block_retry_limit`）。

なお、Slack ユーザートークン（xoxp/xapp）の取り扱いは別ドキュメント [Slack ユーザートークンの取り扱いポリシー](/security/slack-user-token.md) が扱う。本ドキュメントはフック経路に固有の資産のみを対象とする。

# 1. UDS Bearer トークンの管理

ローカルの UDS だが、同一ホスト上の他プロセス（別ユーザー・悪性プロセス）からの偽シグナル注入を防ぐため **2 層で認証**する:

- **第一層 = socket パーミッション 0600**: `adapters::hook_uds` は stale ソケットを unlink → bind 後、**0600** を設定する。所有ユーザー以外はそもそも connect できない。socket は既定で `${XDG_RUNTIME_DIR}/totsuka/claude-events.sock`（ユーザー専用の runtime dir）。
- **第二層 = Bearer トークンの定数時間比較（E-03）**: `POST /claude-events` の `Authorization: Bearer <token>` を `[hooks].auth_token_ref` が解決した値と**定数時間で比較**する（タイミング攻撃防止）。不一致は 401。`job_id` 欠落/不正は 400。

トークンの供給と保管:

- `auth_token_ref` は**シークレット参照**（`${ENV}` または `keychain:<service>/<account>`）で書く。設定ファイルに平文で書かない（F-62/65。解決は Orchestrator 側のみ、プラグインに Keychain 権限を渡さない）。
- 解決済みトークンは herdr プラグイン経由で pane に **env（`TOTSUKA_HOOK_TOKEN`）として注入**される（H-02）。フックスクリプトはファイルではなく env からトークンを読むため、`--settings` ファイル（0600 でレンダリング）にトークンは書かれない。これにより 1 本の `--settings` を `claude --resume` を跨いで再利用できる（H-03）一方、トークンはプロセス env に閉じる。
- **ログへ出さない**: トークン・Authorization ヘッダは logging layer で無条件 redact（§5.2）。フックスクリプトの POST も compact JSON のみを stdout の block 用途に限定し、トークンを標準出力へ出さない（H-13）。

トークン失効・ローテーション時は `keychain:` の実体を差し替え、`totsuka doctor` の `check_hook_token`（`auth_token_ref` 解決）と `check_hook_socket`（自己 POST → 200）で疎通を確認する（[hook-troubleshooting](/operations/hook-troubleshooting.md)）。

# 2. スプールファイルの機密保持（N-05）

POST 失敗時、`on-stop.sh` は送信予定の JSON を NDJSON 1 行として `spool_dir` へ退避する（E-07）。**このペイロードには `last_assistant_message`（エージェントの最終応答本文）が含まれ得る**。応答本文はコード断片・設計内容・（マスキング前の）機微情報を含む可能性があるため、機密保持の対象とする:

- 配置は `[hooks].spool_dir`（既定 `${XDG_STATE_HOME}/totsuka/hooks/spool`）= **ユーザー専用の state dir**。状態DB・ログと同じ XDG_STATE_HOME 配下に閉じ、共有 tmp などへは置かない。
- スプールは**滞留させない**: `replay_spool()` が recover 直後と各サイクルで drain し、全行をクリーンに再投入できたファイルは**削除**する。長期保持しない。
- **隔離の注意点**: parse 不能行を含むファイルは削除せず `<name>.corrupt` へ隔離リネームする（部分書き込みでのデータ喪失防止）。`.corrupt` は自動削除されないため、**中身に機微が残り得る**。調査後は手動で内容を確認し削除すること（手順: [hook-troubleshooting](/operations/hook-troubleshooting.md)）。
- スプールに退避された内容も、再投入されれば冪等 UNIQUE 制約（D-05）で重複が落ちるため、機密の**二重永続化**は起きない（`hook_events` の監査 1 レコードに収束）。

# 3. フックアセットの改ざん耐性（N-02）

フックスクリプトと `--settings` は pane で実行される = **実効的にコード実行の入口**。改ざんされると偽の完了通知や任意コマンド実行に繋がるため、次の 3 点で耐性を持たせる:

- **パーミッション**: 静的スクリプト 6 本（`hook-common.sh` / `on-stop.sh` / `on-notification.sh` / `on-session-start.sh` / `on-session-end.sh` / `on-user-prompt-submit.sh`）は `$XDG_DATA_HOME/totsuka/hooks/` へ **0700**、workflow 別 `orchestrator-<workflow>.json` は **0600** で書き出す。所有ユーザー以外は書き換え・読み取りできない。
- **内容ハッシュによる冪等修復**: `run` / `doctor` 起動時に **SHA-256 が一致すれば書き換えず、不一致（ドリフト・改ざん・バージョンアップ）なら上書き**する。起動のたびに正本へ収束するため、外部からの改変は次回起動で自動修復される。
- **静的埋め込み（repo に置かない）**: スクリプトは CLI バイナリに `include_str!` で同梱される。リポジトリの `.claude/` は**使わない**（H-01）。「リポジトリにチェックインされたフックコード」を持たないため、リポジトリ改変や worktree 経由の注入で乗っ取られる面が無い。

ジョブ固有値（job_id / エンドポイント / トークン / スプール dir / プロンプトコンテキスト）は**ファイルに書かず** env で運ぶ（H-02）。`--settings` は workflow 単位で不変・秘密を含まないため、0600 のまま `--resume` を跨いで安全に再利用できる。`TOTSUKA_PROMPT_CONTEXT`（不可視プロンプト注入用）は**タスク由来の指示文＝タスク本文と同格のテキスト**を含み得るが、プロンプトとしてペインに打鍵していた内容と同じ信頼ドメイン（本人プロセスの env）に閉じており、新たな機密面は増やさない。

# 検証

`totsuka doctor` のフック系プローブがポリシーの実効性を点検する（[orchestrator-cli](/components/orchestrator-cli.md) / [hook-troubleshooting](/operations/hook-troubleshooting.md)）:

- `check_hook_assets` — スクリプト + `orchestrator-*.json` の存在・**0700/0600 パーミッション**・**内容ハッシュ一致**
- `check_hook_token` — `[hooks].auth_token_ref` が解決できる
- `check_hook_socket` — UDS への自己 POST が 200（Bearer/権限の疎通）
- `check_hook_deps` — `curl` / `jq` の存在（H-14。無いと送信系フックはスプール退避、`on-user-prompt-submit.sh` は無出力縮退）
- `check_spool` — `spool_dir` の書き込み可否とバックログ件数（>0 は warning）

# 関連

- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [F-100〜F-107 決定的な完了シグナル](/product/orchestrator-spec.ja.md)
- [フックシグナルフロー](/architecture/hook-signal-flow.md)
- [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
- [Slack ユーザートークンの取り扱いポリシー](/security/slack-user-token.md)
