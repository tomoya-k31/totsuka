---
type: Spec
title: totsuka — ローカルAIエージェント Orchestrator 要件定義（v1）
description: totsuka Orchestrator CLI の要件定義 — タスクソース/Agent IDE/Notifier プラグイン、git worktree ライフサイクル、ワークフロー、並列実行制御、v1 スコープ。
tags: [orchestrator, requirements, plugin, worktree, cli, rust]
timestamp: 2026-07-20T18:00:00+09:00
status: draft
owner: tomoya-k31
---

> 🌐 [English](orchestrator-spec.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

# totsuka — ローカルAIエージェント Orchestrator 要件定義書

- ステータス: Draft v0.2（`orchestrator-requirements.md` より取り込み）
- プロダクト名: totsuka(十束剣)
- 作成日: 2026-07-12
- 対象プラットフォーム: macOS(将来 Linux / Windows)
- 実装言語: Rust

---

## 1. 概要

Notion タスクや GitHub Projects に紐づく Issue などのタスク管理ツールを**タスクソースプラグイン**として接続し、その情報を元にローカルの clone 済みリポジトリに対して herdr / orca などの **Agent IDE プラグイン**へ指示を発行する Orchestrator CLI アプリケーション。git worktree により同一リポジトリ上でもコンフリクトしない並列実行を実現し、詳細設計・実装をエージェントに委譲する。

基本原則として **1 task = 1 repo = 1 worktree** の正規化を採用する(Nanatsusaya のモデルを踏襲)。出力は必ずしも PR とは限らず、ワークフロー定義(§4.9)に従い PR 作成・タスクソースへの書き戻し等に分岐する。各タスクの「完了」の定義もワークフローごとの定義に従う。本アプリはそのローカル・シングルマシン版という位置付けであり、サーバサイドのイベントバスや常駐サービスは持たない。

## 2. 目的

| 目的 | 成功指標(例) |
|---|---|
| 人間を要件定義・設計レビュー・実装レビューに専念させる | 1タスクあたりの人間の関与時間を「レビューのみ」に限定できる比率 |
| 並列実行による開発スループット向上 | 同時進行タスク数(目標: 開発者1人あたり3〜5並列) |
| ツール差し替えの自由度確保 | Agent IDE / タスクソースの追加がコア改修なしで可能 |
| チーム展開 | 設定ファイル配布のみで他メンバーが導入完了できる |

## 3. スコープ

> 「何を決めれば良いのか」への回答: スコープでは **In / Out / 前提** の3点を確定させます。ここが曖昧だと機能要件が際限なく膨らむため、v1で作らないものを明文化するのが最重要です。

### 3.1 In Scope(v1)

- タスクソースプラグイン: GitHub Issues / Projects、Notion の2種
- Agent IDE プラグイン: herdr、orca の2種
- Notifier プラグイン(macOS 通知の公式プラグインを同梱)
- git worktree のライフサイクル管理(作成・ブランチ命名・掃除)
- リポジトリ自動選択(ルールベース + LLM フォールバック、AI Gateway 経由)
- 並列実行制御(グローバル / リポジトリ単位の上限)
- プラグインの install / uninstall / enable / disable
- XDG Base Directory 準拠の設定・状態・ログ管理
- CLI によるステータス確認・操作

### 3.2 Out of Scope(v1)

| 項目 | 理由 |
|---|---|
| GUI / Web ダッシュボード | ターミナル起動が前提。将来 TUI を P2 として検討 |
| PR レビュー自動化・マージ判断・マージ追跡 | 人間のレビュー領域。PR 作成後の追跡は行わない |
| Linux / Windows の動作保証 | 抽象化のみ実施、実装・テストは対象外 |
| 常駐デーモン / サーバ運用 | ローカル起動のライフサイクルに限定 |
| クラウド同期・チーム間の状態共有 | 状態はローカル完結。共有は GitHub / Notion 側に委ねる |
| エージェント自体の実装(コード生成ロジック) | Agent IDE プラグインへ完全委譲 |
| リポジトリの clone / 認証管理 | clone 済みが前提。git 認証は既存環境を利用 |

### 3.3 前提条件

- 対象リポジトリはローカルに clone 済みで、設定ファイルにパス登録されている
- herdr / orca 等の Agent IDE は利用者が別途インストール済み
- macOS 14 以降、git 2.40 以降

## 4. 機能要件

優先度は MoSCoW(M: Must / S: Should / C: Could / W: Won't in v1)で表記。

### 4.1 タスク取得(タスクソースプラグイン)

| ID | 要件 | 優先度 |
|---|---|---|
| F-01 | タスクソースをプラグインとして接続し、タスク一覧・詳細を正規化された共通スキーマ(Task)で取得できる | M |
| F-02 | GitHub Issues / Projects プラグイン(GraphQL API、ステータス列の読み取り) | M |
| F-03 | Notion プラグイン(データベースのプロパティマッピングを設定で定義) | M |
| F-04 | プラグインごとに出力(フィールドマッピング・フィルタ条件)を設定ファイルで定義できる | M |
| F-05 | タスク完了・進行中などのステータスをソース側へ書き戻せる(双方向同期) | S |
| F-08 | **複数人利用時の取り込み確認・制御はタスクソースプラグインの役割**とする(厳密な排他制御までは不要)。例: assignee の有無・実行中ステータスの確認により、他メンバーが着手中のタスクを取り込まない | M |
| F-06 | ポーリング間隔の設定(webhook はローカルアプリのため v1 では非対応) | S |
| F-07 | **結果の書き戻し(`result/publish` RPC)**: 詳細設計の成果物(設計ドキュメント等)を Issue コメント / Notion ページ本文などソース側へ記載できる。記載先・フォーマットの実現はタスクソースプラグインの責務 | M |

**Task 共通スキーマ(案)**: `id, source, title, body, repo_hint, labels, priority, status, url, assignee`

### 4.2 リポジトリ選択

| ID | 要件 | 優先度 |
|---|---|---|
| F-10 | タスクに repo 指定(Notion プロパティ / Issue の所属リポジトリ等)があればそれを優先する | M |
| F-11 | 未指定の場合、設定内のリポジトリ概要 + リポジトリルートの README(先頭 N 行)を材料に LLM で分類する | M |
| F-12 | LLM 呼び出しは OpenAI 互換 API とし、`base_url` を差し替えることで OpenRouter / LiteLLM 等の AI Gateway を指定できる | M |
| F-13 | モデル名・max_tokens・タイムアウトを設定可能(安価モデル前提、例: haiku 級) | M |
| F-14 | LLM には structured output で `{repo, confidence, reason}` を返させる。confidence は self-reported の参考値と割り切り、複数候補が拮抗した場合に人間へ確認を求める(タスクを pending 状態にする) | S |
| F-15 | README 要約はキャッシュし(XDG_CACHE_HOME)、README の hash 変更時のみ再生成 | C |

### 4.3 worktree 管理

| ID | 要件 | 優先度 |
|---|---|---|
| F-20 | タスク開始時に対象リポジトリへ worktree を作成する(1 task = 1 worktree = 1 branch) | M |
| F-21 | ブランチ命名規則を設定可能(デフォルト: `agent/{source}-{task_id}`) | M |
| F-22 | worktree の配置先を設定可能(デフォルト: `{repo}/../.worktrees/{branch}` または XDG_STATE_HOME 配下) | M |
| F-23 | タスク完了・キャンセル時の worktree 掃除(即時 / 保持期間指定 / 手動)をポリシーとして設定可能 | M |
| F-24 | 起動時に孤児 worktree(状態DBに対応タスクが無いもの)を検出し、`doctor` コマンドで掃除を提案 | S |
| F-25 | worktree 作成直前に `git fetch` を行い、`origin/{default_branch}` からブランチを切る(stale なローカルブランチ起点を防ぐ)。ベースブランチはリポジトリ設定で上書き可能 | M |

### 4.4 Agent IDE 連携(Agent IDE プラグイン)

| ID | 要件 | 優先度 |
|---|---|---|
| F-30 | Agent IDE をプラグインとして抽象化し、設定でタスク種別ごと・リポジトリごとに使用エージェントを切り替えられる | M |
| F-31 | 指示発行インターフェース: worktree パス・タスク本文・実行モード(`plan` / `implement`)・追加コンテキストを渡す | M |
| F-32 | エージェントの状態(idle / running / waiting_input / done / failed)を取得できる。herdr は Socket API、orca は各自の手段をプラグイン内に隠蔽。**注:** herdr + Claude Code の完了検知はフック機構(§4.11、F-100–F-107)へ置換され、herdr の状態ストリームは `pane.exited` デッドマン検知のためだけに残す | M |
| F-33 | **Capability negotiation**: プラグインは自身の対応機能(`plan_mode`, `design_preview`, `pane_control`, `state_stream` 等)を宣言し、Orchestrator は対応機能のみ要求する | M |
| F-36 | `plan` モードでは、プラグインが各エージェントの plan / 読み取り主体モードへマッピングして実行する。成果物(設計ドキュメント)を構造化された結果として Orchestrator へ返却する(ワークフローの出力ポリシーに従い書き戻しに使用) | M |
| F-37 | **セッション管理**: dispatch 時にエージェントのセッション識別子(会話履歴 ID)を取得し、タスクと紐付けて状態DBへ永続化する。`session/attach` を agent_ide プラグインの必須メソッドとし、Orchestrator 再起動時・タスク再開時に既存セッションへ re-attach できる | M |
| F-38 | エージェントの実行ログはプラグインが `state/subscribe` の notification に断片として載せ、Orchestrator が task_id 付きで永続化する(`logs --task <id>` の情報源)。**注:** herdr + Claude Code の完了検知はフック機構(§4.11、F-100–F-107)へ置換され、herdr の状態ストリームは `pane.exited` デッドマン検知のためだけに残す | M |
| F-34 | 詳細設計モードでは、対応プラグインに対し設計プレビューの表示(別 pane / サイド画面)を要求できる。表示方法の実現はプラグイン側の責務 | S |
| F-35 | エージェントからの人間への質問(waiting_input)を検知し、`status` に表示するとともに Notifier プラグイン(§4.10)へイベントを配送する。**注:** herdr + Claude Code の完了検知はフック機構(§4.11、F-100–F-107)へ置換され、herdr の状態ストリームは `pane.exited` デッドマン検知のためだけに残す | M |

### 4.5 並列実行制御

| ID | 要件 | 優先度 |
|---|---|---|
| F-40 | グローバル同時実行上限数の設定 | M |
| F-41 | リポジトリ単位の同時実行上限(worktree でコンフリクトはしないが、CI・レビュー負荷の調整用) | M |
| F-42 | Agent IDE プラグイン単位の上限(ツール側のセッション数制約に対応) | S |
| F-43 | キューイングと優先度制御(タスクの priority を尊重、FIFO フォールバック) | S |
| F-44 | 実行中タスクの個別キャンセル・リトライ。リトライ時、worktree が存在しなければ作り直す。存在する場合は worktree を維持したまま、タスクに紐付く前回のエージェントセッション ID(F-37)を指定して会話を再開する | M |
| F-45 | 同時実行上限のカウント対象は `dispatched → running → verifying → publishing` の状態のみとする(`verifying` = human 検収待ち。エージェント作業は終了しているが出力確定前のためスロットを保持する)。`waiting_input` や `escalated`(人間対応待ち) 等の待機状態はスロットを解放し、再開時にスロットを再取得する(待ちによる実質デッドロックの防止) | M |

### 4.6 プラグインシステム

| ID | 要件 | 優先度 |
|---|---|---|
| F-50 | プラグイン種別: `task_source` / `agent_ide` / `notifier` の3種(将来の種別追加が可能な設計) | M |
| F-51 | プラグインは**別プロセスとして起動し、JSON-RPC 2.0 over stdio(将来 Unix socket)で通信**する | M |
| F-52 | install / uninstall / enable / disable / list をサブコマンドで提供 | M |
| F-53 | プラグインマニフェスト(`plugin.toml`: 名称・種別・バージョン・対応プロトコルバージョン・capabilities)を必須とする | M |
| F-54 | プロトコルバージョンの互換性チェック(不一致時は明示的なエラー) | M |
| F-55 | プラグインの配布形式: v1 はローカルパス / GitHub Release からのバイナリ取得。レジストリは W | S |
| F-56 | **install(バイナリの存在)と enabled(設定の宣言)を分離**する。install 先は `$XDG_DATA_HOME/totsuka/plugins/{name}/`、有効/無効は config.toml の `[plugins.{name}] enabled` フラグで宣言的に管理する | M |
| F-57 | `plugin enable / disable` は config.toml の `enabled` を書き換える編集ヘルパーとする(toml_edit によりコメント・整形を保持)。設定ファイルの直接編集でも同一の結果になること | M |
| F-58 | disable 中のプラグインはプロセスを起動しない。disable 中のプラグインを参照する設定(リポジトリのデフォルトエージェント等)は `config validate` でエラーとする | M |
| F-59 | プラグイン固有設定の検証は、プラグイン側に必須の `config/validate` RPC メソッドを設けて委譲する。`config validate` 実行時、有効な全プラグインを一時起動して検証させる(socket 疎通確認などスキーマで表現できない検証を可能にするため) | M |

**プラグイン設定の設計方針(宣言的 + CLI は編集ヘルパー)**

設定ファイルを single source of truth とし、git 管理・チーム配布した設定がそのまま動作状態を決める。

| 場所 | 責務 |
|---|---|
| `$XDG_DATA_HOME/totsuka/plugins/{name}/` | バイナリ + manifest(install / uninstall の対象) |
| `config.toml` の `[plugins.{name}]` | 有効/無効のロスター + 共通項目(`kind`, `max_concurrency`, `timeout_secs`, `log_level` 等。Orchestrator が解釈する) |
| `plugins/{name}.toml` | プラグイン固有設定。Orchestrator は中身を解釈せず、JSON-RPC の initialize params としてそのまま渡す |

```toml
# config.toml
[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3
timeout_secs = 120
```

```toml
# plugins/herdr.toml(固有設定)
socket_path = "${XDG_RUNTIME_DIR}/herdr.sock"
design_preview = "side_pane"
```

**プラグイン方式の比較(決定根拠)**

| 方式 | ABI 安定性 | 言語自由度 | 障害隔離 | 備考 |
|---|---|---|---|---|
| dylib (cdylib) | ✗ Rust ABI 不安定、abi_stable 依存 | Rust のみ実質 | ✗ クラッシュ巻き込み | 不採用 |
| WASM (extism 等) | ○ | ○ | ○ | ソケット・プロセス起動などホスト I/O が重く、Agent IDE 操作に不向き |
| **サブプロセス + JSON-RPC** | ◎ プロセス境界 | ◎ 任意言語 | ◎ | **採用**。MCP / LSP と同型で、herdr Socket API との親和性も高い |

### 4.7 設定

| ID | 要件 | 優先度 |
|---|---|---|
| F-60 | 設定は TOML、`$XDG_CONFIG_HOME/totsuka/config.toml`(未設定時 `~/.config/totsuka/`) | M |
| F-61 | リポジトリ定義: パス・概要文(LLM 選択用)・デフォルトエージェント・上限数 | M |
| F-62 | シークレット(Notion / GitHub / AI Gateway の API キー)は設定ファイル平文禁止。環境変数参照(`${ENV_VAR}` 展開)と macOS Keychain 参照をサポート | M |
| F-63 | `config validate` による静的検証(スキーマ・パス存在・プラグイン整合性)に加え、有効プラグインへの検証委譲(F-59)を行う。`--offline` フラグでプラグイン起動・疎通を伴う検証をスキップし静的検証のみ実行(CI 用途) | M |
| F-64 | プラグイン個別設定は `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml` に分離する(共通項目は config.toml、固有項目は個別ファイル。F-56 の設計方針参照) | M |
| F-65 | シークレット参照(`${ENV_VAR}` / `keychain:` prefix)の解決は **Orchestrator 側で行い**、解決済みの値を initialize params でプラグインへ渡す。プラグインに Keychain アクセス権限を持たせない | M |
| F-66 | 設定の優先順位: CLI フラグ > 環境変数 > `plugins/{name}.toml` > `config.toml` のデフォルト値 | M |

### 4.8 状態管理

| ID | 要件 | 優先度 |
|---|---|---|
| F-70 | タスク実行状態を SQLite(`$XDG_STATE_HOME/totsuka/state.db`)へ永続化し、アプリ再起動後に実行中タスクの状態を復元できる | M |
| F-71 | 状態遷移を明示的なステートマシンとして実装する。共通遷移: `queued → dispatched → running → publishing → done / failed / cancelled`。`running` の実体(plan / implement)と `publishing` の実体(PR 作成 / ソース書き戻し)はワークフロー定義(§4.9)が決める | M |
| F-72 | 各遷移をイベントログとして記録(監査・デバッグ用) | S |
| F-73 | 取り込みの冪等性: `(source, source_task_id)` のユニーク制約により同一タスクの二重取り込みを防止する | M |
| F-74 | `run` の多重起動防止: `$XDG_STATE_HOME/totsuka/` のロックファイル + PID で制御。`status` はプロセス生存確認を行い、run が停止中なら「orchestrator not running」と stale 状態を明示する | M |

### 4.9 ワークフロー定義(トリガー × 実行モード × 出力ポリシー)

同一プラグインバイナリの上に、**「どの条件のタスクを、どのモードで実行し、結果をどこへ出すか」を組み合わせた名前付き設定 = ワークフロー**を任意個定義できる。例: 同じ GitHub Issue プラグインに対し「詳細設計ワークフロー」と「実装ワークフロー」を並存させる。いくつ定義するかは利用者次第。

| ID | 要件 | 優先度 |
|---|---|---|
| F-80 | ワークフロー = `source(タスクソースインスタンス) × trigger(取り込み条件) × mode(plan / implement) × agent × output(出力ポリシー)` の名前付き設定。config.toml に `[[workflows]]` として任意個定義できる | M |
| F-81 | trigger は Issue / Projects のステータス列・ラベル、Notion のプロパティ値等で指定する。1タスクは同時に1ワークフローにのみマッチすること(複数マッチは `config validate` で警告、優先順位は定義順) | M |
| F-82 | `mode = "plan"`(詳細設計): worktree は作成する(コードベース参照のため)が、**push・PR 作成は行わない**。エージェントは plan モードで実行し、成果物として設計ドキュメントを返す | M |
| F-83 | 出力ポリシー `output`: `pull_request`(push + PR 作成)/ `source`(タスクソースプラグインの `result/publish` で Issue コメント・Notion ページ等へ記載)/ `none`。タスクソースプラグインは対応可能な出力を capability として宣言し、実現方法はプラグイン側で実装する | M |
| F-84 | `on_success` / `on_failure`: 完了時にソース側のステータスを遷移させる(例: 「設計待ち → 設計レビュー待ち」)。この**ソース上のステータス遷移が plan → 人間レビュー → implement のハンドオフ機構**となり、設計と実装の間に人間のレビューが自然に挟まる | M |
| F-85 | plan モードの worktree 掃除ポリシーは implement と別に設定可能(設計のみなら即時掃除がデフォルト) | S |
| F-86 | `output = "pull_request"` 時の push・PR 作成は **Orchestrator の責務**(gh CLI または GitHub API)。エージェントの責務はコミットまでとし、この境界をプラグインプロトコル仕様に明記する | M |

**設定例**

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"                        # 結果を Issue コメントへ記載
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
```

### 4.10 通知(Notifier プラグイン)

| ID | 要件 | 優先度 |
|---|---|---|
| F-90 | Notifier プラグインは Orchestrator からのイベント(`waiting_input` / `done` / `failed` / `pending`(リポジトリ選択の人間確認待ち))を JSON-RPC notification として受信し、通知手段を実装する | M |
| F-91 | 公式プラグインとして macOS 通知センター向け Notifier を v1 で同梱する | M |
| F-92 | ワークフローごと・イベント種別ごとに通知の有効/無効を設定できる | S |
| F-93 | 通知の失敗は本体のタスク実行に影響させない(fire-and-forget、エラーはログのみ) | M |
| F-94 | **クリック可能な通知 → pane フォーカス(click-to-focus)**: 通知をクリックすると GUI ターミナルが前面化し、その通知を出したタスクの pane がフォーカスされる。送出は `terminal-notifier` バックエンド(`-activate <bundle-id>` + `-execute 'totsuka focus <task_id>'` + `-group totsuka-<task_id>`。Sequoia 15.x+ で壊れるため `-sender` は `-activate` と併用しない)。フォーカス経路は `totsuka focus` → 制御 UDS `POST /focus` → 当該タスクの agent_ide プラグインの `session/focus`(プロトコル 0.1.4、`pane_control` 宣言時のみ。session id は不透明のまま、F-37)。縮退はすべて静か: terminal-notifier 未導入は osascript へフォールバック、Orchestrator 停止中・pane 消失はアプリ前面化のみ成立(ADR-0005 参照) | S |

### 4.11 決定的な完了シグナル(Claude Code フック)

Claude Code は Lifecycle Authority を持たないため、herdr の screen-manifest(画面パターン認識)由来の完了検知は構造的にロスが避けられない(遅延・取りこぼし・誤検知)。そこで完了は **Claude Code のフックを介して決定的に**通知する: herdr の pane が `claude --settings <hooks_dir>/orchestrator-<workflow>.json [--resume <sid>]` を起動し、command 型の `Stop` / `Notification` / `SessionStart` / `SessionEnd` フックが Unix ドメインソケット経由で Orchestrator へ POST する(`verification = "llm"` のワークフローは追加で、rubric をセッション内で適用する prompt 型 `Stop` フックも持つ)。本節がこの機構の要件のホームであり、エンドツーエンドの流れは `architecture/hook-signal-flow.md`、配置の意思決定は ADR-0004、設定面は `[hooks]`(`auth_token_ref` / `socket_path` / `spool_dir` / `block_retry_limit`)とワークフロー別の `verification` / `timeout_secs` / `rubric` キーが担う。

| ID | 要件 | 優先度 |
|---|---|---|
| F-100 | **UDS 受信**: Orchestrator は完了シグナルを Unix ドメインソケット(モード `0600`)上でコアの driving adapter(`adapters::hook_uds`、自作の `UnixListener` + 最小 HTTP/1.1)で受信する。`POST /claude-events`、`Authorization: Bearer` を `[hooks].auth_token_ref` と定数時間比較、body 上限 1 MiB、`job_id` 必須(欠落は `400`)。受信側は即 `200` を返し非同期に処理し、JSON body を `ports::SignalPort` 経由で `domain::signal::AgentSignal` へ正規化する | M |
| F-101 | **ステータスマーカー規約**: 完了はアシスタント応答の最終行のマーカーで自己申告する(同一行に複数あれば最後が勝つ): `<<STATUS:COMPLETED>>` / `<<STATUS:NEEDS_INPUT reason="...">>` / `<<STATUS:FAILED reason="...">>`(正準形は二重カッコだが、実エージェントが区切りを正規化するためパーサは単一 `<STATUS:...>` も受理する)。マーカー欠落 & `stop_hook_active=false` ⇒ `Stop` フックが `block` して Claude に再出力させる。`stop_hook_active=true` ⇒ block せず `UNKNOWN` を POST。`background_tasks` が非空なら heartbeat のみ(中間 Stop、完了ではない) | M |
| F-102 | **検収**(`verification = "llm"`(既定) / `"human"` / `"none"`): `llm` はセッション内 prompt 型 `Stop` フック(rubric)を実行 — `COMPLETED` 受信で Engine は直ちに Publishing へ進む。`human` はタスクを `Verifying` に留め `totsuka task verify --pass/--fail` を待つ。`none` は直接 publish する | M |
| F-103 | **エスカレーション**: 連続 3 回の `UNKNOWN` stop(DB から再計算 — フックの自己申告は信用しない。`[hooks].block_retry_limit`、既定 3) OR 最後のシグナルから 30 分の沈黙(ワークフロー `timeout_secs` で上書き) OR 相関の異常 ⇒ タスクを `Escalated`(非終端)へ遷移し、notifier 通知と `diagnostics/snapshot`(herdr `pane.read`)を伴う | M |
| F-104 | **スプール + at-least-once + 冪等**: POST 失敗時、フックは 2 回リトライ後 `spool_dir` へ NDJSON 1 行を追記する。Engine の `replay_spool()` が `recover()` 時と各サイクルで再投入する。`hook_events UNIQUE(job_id, claude_session_id, prompt_id, event)` が重複/順序前後の POST(多重発火・スプール再送・curl リトライ)を落とす。壊れたスプール行は削除せず `.corrupt` へ隔離する | M |
| F-105 | **会話継続**: `Task.thread_key`(`channel:thread_ts`)が会話を相関する。同一スレッド内の追いメンションは**新規タスク**だが、先行タスクの `claude_session_id` を `task/dispatch(resume_session_id)` → `claude --resume` で引き継ぐ(worktree は破棄済みなら新規作成 = セッションだけ使い回す)。最新セッション勝ち、異なるスレッドは決して相互 resume しない。シグナルは自身の `job_id` のタスクへ配路され、共有セッション id から宛先を推測しない(E-09) | M |
| F-106 | **デッドマン**: herdr の `events.subscribe` ストリームは `pane.exited` デッドマン検知のみへ縮退する。herdr プロセスのクラッシュは `Failed` として表面化する | M |
| F-107 | **pane の後処理**: `Done` の pane は自動クローズ(冪等な `task/cancel`)、`Failed` / `Escalated` の pane は診断のため保持する | M |

## 5. 非機能要件

### 5.1 起動・CLI

- ターミナルから単一バイナリで起動。デーモン化しない(フォアグラウンド実行、`run` 中は TUI ライクなサマリ表示は将来検討)。
- 起動時間: 1秒以内(`status` 等の読み取り系)。

**CLI コマンド体系(提案)**

| コマンド | 用途 |
|---|---|
| `init` | 設定ファイルの雛形生成、環境チェック |
| `run [--watch]` | タスク取り込み（push、`task/submit`）〜ディスパッチのメインループ実行(デフォルトはワンショット、`--watch` は push を受け続けたまま shutdown まで常駐 — 未決事項 #2 は解決済み) |
| `status [--json]` | 実行中 / キュー / 待機中タスクと worktree の一覧 |
| `task list / show <id> / cancel <id> / retry <id>` | タスク個別操作 |
| `plugin list / install / uninstall / enable / disable` | プラグイン管理 |
| `config validate / show [--redacted]` | 設定検証・表示(シークレットはマスク) |
| `doctor` | 環境診断(git バージョン、孤児 worktree、プラグイン疎通、API キー疎通) |
| `logs [-f] [--task <id>]` | ログ閲覧・追尾 |
| `completion <shell>` | シェル補完生成 |
| 共通フラグ | `--debug`, `--json`, `--dry-run`, `--config <path>` |

`--json` は全読み取り系コマンドに用意し、他ツール(jq、CI、将来のTUI)からの利用を可能にする。`--dry-run` は protocol 0.2.0 以降、副作用ゼロの no-op になった: task_source は必要時に取得されるのではなく自ら push するため、事前にプレビューできる対象が無い — 実行結果を見るには `--dry-run` なしで起動する。

### 5.2 ログ

- `$XDG_STATE_HOME/totsuka/logs/` に出力。日次ローテーション + 世代数設定。
- 構造化ログ(JSON Lines)、`tracing` クレートを使用。人間向けには `logs` コマンドで整形表示。
- レベル: error / warn / info / debug / trace。`--debug` で debug 以上を出力。
- **機密情報のマスキングを必須とする**: API キー・トークン・Authorization ヘッダは logging layer で無条件に redact。プロンプト本文は debug 以上でのみ出力し、設定で無効化可能。

### 5.3 信頼性・回復性

- 異常終了(SIGKILL 含む)後の再起動で、状態DBから実行中タスクとエージェントセッション ID(F-37)を復元し、`session/attach` による再接続を試みる。再接続不能な場合のみ「継続確認 / 失敗マーク」を人間に委ねる。
- タスクソース・AI Gateway への API 呼び出しは指数バックオフ付きリトライ。
- プラグインプロセスのクラッシュを検知し、タスクを failed へ遷移(Orchestrator 本体は巻き込まれない)。

### 5.4 セキュリティ

- シークレットは Keychain または環境変数のみ。状態DB・ログ・キャッシュに書き込まない。
- プラグインは任意コード実行であるため、install 時に取得元とチェックサムを表示し確認を求める。
- 外部送信はタスクソース API / AI Gateway / Agent IDE のみ。テレメトリは収集しない。

### 5.5 性能

- 100 タスク / 10 リポジトリ規模で `status` 応答 500ms 以内。
- 並列上限まで worktree 作成がボトルネックにならないこと(worktree 作成は直列化しない)。

### 5.6 移植性(将来対応の考慮)

- パス・Keychain・プロセス管理は trait で抽象化し、`platform::macos` モジュールに実装を隔離。
- XDG は macOS でも尊重(`dirs` クレートの macOS デフォルトではなく XDG 準拠を明示採用)し、Linux 移行コストを下げる。

## 6. 技術要件

| 項目 | 内容 |
|---|---|
| 言語 | Rust(edition 2024、stable toolchain) |
| 主要クレート(案) | tokio, clap, serde, toml, toml_edit, tracing, rusqlite, reqwest, keyring |
| 依存方針 | 最小主義は取らないが肥大化は避ける。差し替えが想定される箇所(JSON-RPC 層・永続化・シークレットストア)は必ず ports の trait 背後に置き、クレート選定を後から変更可能にする。JSON-RPC 層は serde_json + tokio による薄い自作から始め、要件次第でライブラリへ移行 |
| アーキテクチャ | ヘキサゴナル。`core`(ドメイン・ステートマシン)/ `ports`(TaskSource, AgentIde, LlmRouter, SecretStore の trait)/ `adapters`(JSON-RPC プラグインブリッジ、SQLite、Keychain) |
| ワークスペース構成 | `orchestrator-core` / `orchestrator-cli` / `plugin-protocol`(プラグイン開発者向けに公開する型定義クレート)/ 各公式プラグイン crate |
| プラグイン管理 | `$XDG_DATA_HOME/totsuka/plugins/{name}/` にバイナリ + manifest を配置。enable/disable は設定側のフラグ |
| AI Gateway | OpenAI 互換 `/chat/completions` を前提とし `base_url` / `model` / `api_key_ref` を設定可能 |

## 7. UI/UX 要件

- GUI なし。CLI の出力品質を UX と定義する。
- エラーメッセージは「原因 + 次のアクション」を必ず含む(例: `config not found → run 'app init'`)。
- `--debug` オプションで開発中に必要な情報(RPC ペイロード、状態遷移、LLM 判定根拠)を出力。機密情報は 5.2 のマスキング方針に従い出力しない。
- 出力は NO_COLOR 環境変数と非 TTY を尊重。

## 8. コンテンツ要件

> 「何を書けばよいか」への回答: CLI ツールにおけるコンテンツとは、**ユーザーが読むテキスト全般**です。以下を成果物として定義します。

| コンテンツ | 内容 |
|---|---|
| CLI ヘルプ / エラーメッセージ | 文言規約(トーン・用語統一・英語で統一するか日英併記か)を定める。v1 は英語 UI + 日本語 README を推奨 |
| README | 概要、インストール、クイックスタート(5分で1タスク流せる手順) |
| 設定リファレンス | config.toml 全キーの説明とデフォルト値 |
| プラグイン開発ガイド | プロトコル仕様(JSON-RPC メソッド一覧)、manifest 仕様、サンプルプラグイン |
| 運用ガイド | doctor の読み方、worktree 掃除、トラブルシューティング FAQ |
| CHANGELOG | Keep a Changelog 形式、semver 連動 |

用語集(Task / Source / Agent / worktree / dispatch 等)を定義し、ログ・ドキュメント・コードで統一する。

## 9. テストと品質保証

| レイヤ | 内容 |
|---|---|
| ユニットテスト | ステートマシン遷移、リポジトリ選択ロジック(ルール部)、設定パース、マスキング。cargo test |
| 自動結合テスト | **モックプラグイン**(テスト用の fake task_source / fake agent_ide バイナリ)を用意し、JSON-RPC 境界を実プロセスで検証。tempdir 上の実 git リポジトリで worktree ライフサイクルをテスト |
| E2E | GitHub のテスト用リポジトリ + fake agent で「タスク取得→worktree→ディスパッチ→完了→掃除」の全経路を CI 上で実行。LLM 呼び出しはレコーディング(VCR 方式)でスタブ化 |
| 手動結合テスト | herdr / orca 実機との疎通、設計プレビュー表示、waiting_input 検知。リリース前チェックリスト化 |
| 品質ゲート | clippy(deny warnings)、rustfmt、cargo-audit / cargo-deny(依存脆弱性・ライセンス)、カバレッジ計測(llvm-cov) |

## 10. 展開と保守

> 「考えられること」への回答:

### 10.1 配布

- **GitHub Releases のユニバーサルバイナリ(arm64 / x86_64)(推奨)** + `cargo install`。パッケージマネージャ(Homebrew 等)は v1 では対象外。
- macOS Gatekeeper 対策: 社内配布なら ad-hoc 署名 + 手順書で可。社外公開するなら Developer ID 署名 + notarization を計画(v1 で判断が必要な項目)。

### 10.2 バージョニング・互換性

- アプリ本体は semver。**プラグインプロトコルは独立したバージョン**を持ち、manifest で互換範囲を宣言。破壊的変更時はメジャーを上げ、旧プロトコルを1世代サポート。
- 設定スキーマにバージョンキーを持たせ、起動時マイグレーション(または `config migrate`)を提供。

### 10.3 更新・運用

- `--version` とリリースノートへの導線。self-update は v1 では対象外(バイナリ再取得 or `cargo install` 再実行)。
- 状態DB のスキーママイグレーション(埋め込みマイグレーション、起動時自動適用 + バックアップ)。
- 依存 API(Notion / GitHub / herdr Socket API)の変更監視を保守タスクとして定義。プラグイン分離により本体リリースなしで追随可能。
- Issue テンプレート + `doctor --json` の出力添付を報告フローとする。

### 10.4 チーム展開

- 設定ファイルのテンプレートを社内リポジトリで配布(シークレットは各自 Keychain / env)。
- オンボーディング手順: インストール(ダウンロード / `cargo install`) → `init` → キー設定 → `doctor` → `run` の5ステップに収める。

---

## 11. 付録A: プラグインプロトコル最小メソッドセット(v0)

> 本メソッドセットは初期版。AI IDE 側の仕様変化やハンドリング要件に応じて**継続的なチューニングを前提**とし、変更は F-54 のプロトコルバージョニングで管理する。

| メソッド | 方向 | 対象種別 | 用途 |
|---|---|---|---|
| `initialize` | O→P | 共通 | 固有設定(解決済みシークレット含む)と capability の交換。task_source には `triggers`/`poll_interval_secs` も渡す(protocol 0.1.6) |
| `shutdown` | O→P | 共通 | 終了要求 |
| `config/validate` | O→P | 共通 | 固有設定の検証(F-59) |
| `task/submit` | **P→O request** | task_source | プラグインが見つけたタスクを push(persist-before-ack、protocol 0.1.6)。protocol 0.2.0 で削除された `tasks/fetch` の後継 — task_source は全て push 専用 |
| `task/update_status` | O→P | task_source | ソース側ステータス遷移(F-84) |
| `result/publish` | O→P | task_source | 設計結果等の書き戻し(F-07) |
| `task/dispatch` | O→P | agent_ide | worktree・タスク・mode を渡し実行開始。セッション ID を返す |
| `task/cancel` | O→P | agent_ide | 実行キャンセル |
| `session/attach` | O→P | agent_ide | 既存セッションへの再接続(F-37) |
| `state/subscribe` → notification | P→O | agent_ide | 状態遷移・ログ断片のストリーム(F-38) |
| `notify` (notification) | O→P | notifier | イベント配送(F-90) |

O = Orchestrator, P = Plugin

## 12. 未決事項(Open Questions)

| # | 論点 | 決定者 |
|---|---|---|
| 1 | 設計レビュー・実装レビューの承認操作を本アプリの CLI で行うか、GitHub / Notion 側の操作のみとするか(状態遷移 `waiting_review` の解除トリガー) | プロダクトオーナー |
| 2 | ~~`run` を都度実行(ワンショット)にするか `--watch` 常駐にするか~~ → **解決済み(2026-07-12)**: ワンショットをデフォルトとし、`--watch` で常駐ポーリング | 解決済み |
| 3 | herdr の pane 制御(設計プレビュー)を capability として v1 必須にするか、herdr プラグインのみの拡張とするか | アーキテクト |
| 4 | ~~1タスクで design → implement を自動連続実行するか~~ → **解決**: ワークフローモデル(§4.9)を採用。plan と implement は別ワークフローとし、ソース側のステータス遷移(人間の操作)がハンドオフとなる | 解決済み |
| 5 | 社外公開の予定有無(署名 / notarization / ライセンス選定に影響) | 経営判断 |
| 6 | Nanatsusaya(サーバ版)との将来的な統合・役割分担(プロトコル共通化の要否) | アーキテクト |
