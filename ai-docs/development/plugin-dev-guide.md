---
type: Guide
title: プラグイン開発ガイド
description: totsuka プラグインの作り方。plugin-protocol クレートの型、JSON-RPC(NDJSON/stdio) メソッド、plugin.toml マニフェスト、capability 宣言、開発ループ（plugin install --from-source）とビルド手順（bin 名 = plugin.toml の name という不変条件）、install/enable の流れ、参照実装。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol
tags: [plugin, protocol, json-rpc, manifest, guide]
generated: { by: claude-code/opus-5, at: 2026-08-01T06:30:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/plugin-dev-guide.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/plugin-dev-guide.md docs/plugin-dev-guide.ja.md -->

# 概要

プラグインは **stdio 上で JSON-RPC 2.0（1 行 1 メッセージ = NDJSON）を話す単一実行バイナリ**。3 種の kind がある: `task_source`（タスク供給）、`agent_ide`（エージェント駆動）、`notifier`（通知）。プロトコルの単一の正は [plugin-protocol クレート](/components/plugin-protocol.md)（型定義を公開）。

# 依存

```toml
[dependencies]
plugin-protocol = { git = "https://github.com/tomoya-k31/totsuka" }
```

`plugin_protocol` が提供する型（`Task`、`InitializeParams/Result`、各メソッドの params/result、`Manifest`、`Capabilities`、`jsonrpc` ヘルパ）を使う。プロトコル版は **アプリ本体と独立**（#50）。

# マニフェスト（plugin.toml）

各プラグインは `plugin.toml` を同梱する。

```toml
name = "github"                 # インスタンスバイナリ名と一致
kind = "task_source"            # task_source | agent_ide | notifier
version = "0.1.0"               # プラグイン自身の版
protocol_version = ">=0.1.6, <0.5"  # 対応する Orchestrator プロトコル範囲(F-54)

[capabilities]                  # 実際に対応する機能だけ宣言(F-33)
plan_mode = true                # agent: plan モード対応
state_stream = true             # agent: state/subscribe ストリーム(F-38)
outputs = ["source"]            # task_source: result/publish 対応(F-83)
task_submit = true              # task_source: push 型ソース宣言（必須。[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）
```

Orchestrator は起動前に `protocol_version` の互換性を検査し（F-54）、宣言された capability のみ要求する。**プロトコル 0.2.0 以降、task_source は push（`task_submit = true`）専用**（`tasks/fetch` は削除済み）。`^0.1` を宣言する manifest は 0.2.0 の Orchestrator に、`<0.3` を上限とする manifest は **0.3.0**（#264 の `Task.thread_key` 削除）に、`<0.4` を上限とする manifest は **0.4.0**（#411 の `TaskDispatchParams.hook` / `Capabilities.design_preview` 削除）に、それぞれ起動拒否される — 上限は超えたい破壊的バンプの**次**のメジャー/マイナーに置く（現行なら `<0.5`）。

上の例は task_source なので下限は `>=0.1.6`（`task_submit` capability が入ったバージョン。それより前を含める範囲は宣言できない）。

**下限も上限と同じくらい意味を持つ。** 例えば herdr は `>=0.2.3` を宣言する。0.2.3 が `TaskDispatchParams.tool_launch` の入ったバージョンで、herdr には argv を自前で組み立てるフォールバックがもう無いためである。下限で弾いておくことが、そのフォールバックを「非推奨」ではなく**到達不能**にしている（[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。

**これは kind で決まる規則ではない。** 同じ agent_ide でも orca は `>=0.1.0` のままである — `orca` CLI 自体を駆動していて `tool_launch` を一度も読まないので、下限を上げると**問題なく動く Orchestrator を弾く**ことになる。task_source / notifier も同じ理由で据え置き。**下限は「何に依存しているか」に従う**のであって、プラグインの kind やその時点の最新プロトコルに合わせるものではない。

# メソッド（§11 付録 A）

**O→P** = Orchestrator→Plugin 呼び出し、**P→O** = Plugin→Orchestrator 通知。

## 共通

| メソッド | 方向 | 内容 |
|---|---|---|
| `initialize` | O→P | 解決済み config + プロトコル版を渡す。plugin_version + capabilities を返す（F-65）。**task_source には orchestrator の `[[repositories]]` も `repositories: [{name, summary?, path?}]` として供給される**（0.1.1、#109。任意フィールド — 使わなければ無視してよい。ソース側でリポジトリ解決するプラグインは自前設定の重複を省ける）。**同じく orchestrator の `[llm]` も `llm: {base_url, model, api_key?}` として供給される**（0.1.2、#119。api_key は解決済み。プラグイン自身の LLM 設定があればそちらを優先する default + override を推奨）。**task_source には `triggers: [{workflow, trigger}]`（`[[workflows]]` 定義順）と `poll_interval_secs: Option<u64>` も供給される**（0.1.6、[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)。監視条件と自プラグイン内部の fetch 周期。イベント駆動ソースは `poll_interval_secs` を無視してよい） |
| `config/validate` | O→P | プラグイン設定を検証（F-59） |
| `shutdown` | O→P | 猶予付き終了要求 |

## task_source

`task_source` は **push 専用**（プロトコル 0.2.0、[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。タスクを見つけたら `task/submit` を Orchestrator へ**自分から**送る — Orchestrator がタスクを取りに来る RPC（旧 `tasks/fetch`）は存在しない。イベント駆動ソース（Webhook/Socket 等）は受信のたびに、ポーリングが自然なソース（GitHub/Notion 等）は `initialize` で受け取った `triggers`/`poll_interval_secs` で自前タイマーを回して、それぞれ `task/submit` を呼ぶ（[plugin-sdk](/components/plugin-sdk.md) の `poll_loop` がこのタイマー実装を提供する）。

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/submit` | **P→O request** | プラグインが見つけたタスクを Orchestrator へ push（persist-before-ack）。応答は `accepted`（永続化）/ `duplicate`（冪等キー衝突、破棄してよい）/ `rejected`（恒久的に処理不能、reason 付き）のいずれかで**すべて最終**（同じタスクを reason で再送しない）。`NOT_ACCEPTING`/`SUBMIT_OVERLOADED`/`INTERNAL_ERROR` は再送可能（submit は冪等なのでバックオフ再送してよい） |
| `task/update_status` | O→P | ソース側ステータス遷移（F-84） |
| `result/publish` | O→P | 成果物をソースへ書き戻し（F-07） |

## agent_ide

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/dispatch` | O→P | worktree 上で作業開始 → セッション ID を返す（F-31）。worktree は **detached HEAD** で渡る — ブランチ作成・コミット・push・PR 作成はすべてエージェント側の責務（F-86、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)） |
| `task/cancel` | O→P | 実行中タスクのキャンセル |
| `session/attach` | O→P | 既存セッションへ再接続（F-37）。attached + 現在状態を返す |
| `state/subscribe` | O→P | 状態/ログのストリーム購読（応答後に通知を流す） |
| `state/notification` | P→O | 状態変化 + ログ断片の通知（F-38）。`state` は `idle`/`running`/`waiting_input`/`done`/`failed` |

## notifier

| メソッド | 方向 | 内容 |
|---|---|---|
| `notify` | O→P（通知・応答不要） | イベント（`waiting_input`/`done`/`failed`/`pending`）配送（F-90）。**配送失敗はタスク実行に影響させない（F-93）** |

# 状態の対応（F-32）

エージェントの状態 `AgentState` は Orchestrator のステートマシンへ写像される（dispatched→running は `Start`、blocked は `waiting_input` でスロット解放、done は publishing へ）。プラグインは自分のツールの状態を 5 値へ正直に写像する。

# ビルドと install（開発ループ）

チェックアウトからの導入は 1 コマンドで済む。ビルド → install → enable までまとめて行う（#346）。

```sh
totsuka plugin install --from-source github --enable      # 1 つだけ
totsuka plugin install --from-source --all --enable       # 全部
totsuka plugin install --from-source --all --profile dev  # デバッグビルドで
```

チェックアウトは cwd から上へ辿って自動検出する（`--repo <dir>` で明示も可）。判定は「Cargo ワークスペースのルート**かつ** `plugins/` を持つ」で、`git rev-parse --show-toplevel` は使わない — 無関係なクローンでも答えてしまうため。`cargo build ... --bins` は全対象パッケージをまとめて **1 回**だけ起動する。何が起きるか先に見たいときは `--print-plan`（cargo を起動せず計画だけ印字）。

以下は手作業で同じことをする場合の内訳。

各プラグインはリポジトリルートの Cargo ワークスペースの通常メンバー（`plugins/{crate}/`）。ワークスペースルートから対象クレートを指定してビルドする。

```sh
cargo build --release -p task-source-github
```

生成物はクレート単体の `target/` ではなく、ワークスペース共有の `target/release/` に置かれる。

**バイナリ名は Cargo パッケージ名ではなく `plugin.toml` の `name` である。** 各プラグインの `Cargo.toml` は `[[bin]] name` を `plugin.toml` の `name` に一致させており（`task-source-github` パッケージの bin は `github`）、`totsuka plugin install <dir>` が要求するのもこの名前なので、**リネームは不要**。上のコマンドの出力は `target/release/github` になる。

この一致は `scripts/arch-lint.sh` の `plugin-bin-name` チェックが機械検証する（[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md)）。新しいプラグインを追加するときは `[[bin]] name` を `plugin.toml` の `name` に合わせること。合っていないと install は `plugin binary <name> not found in <dir> → expected a file named after the plugin` で失敗する。

install に `<dir>` を渡す形では、`plugin.toml` とバイナリを同じディレクトリに置く必要がある。

```sh
mkdir -p dist/github
cp target/release/github plugins/task-source-github/plugin.toml dist/github/
totsuka plugin install ./dist/github
```

`--from-source` はこの staging ディレクトリを作らない。core の `prepare_install_from(manifest_path, binary_dir)` が manifest とバイナリを別々の場所から取れるので、manifest は `plugins/<pkg>/` のまま、バイナリは `target/<profile>/` から直接読む。

# install / enable の流れ

- `totsuka plugin install <dir>`: `plugin.toml` + バイナリを含むディレクトリを検証（SHA-256 表示・確認）し `$XDG_DATA_HOME/totsuka/plugins/{name}/` へ配置（§5.4）
  - **再インストール（新しいビルドで入れ替える）でも、インストール先のバイナリを上書きすることはない。** 同じディレクトリに一時ファイルを作って `rename` で差し替えるため、インストール先は毎回新しい inode になる。macOS がコード署名の検証結果を vnode 単位でキャッシュするため、中身だけを書き換えると次回起動が無言で `SIGKILL` される（#292）
- `totsuka plugin enable {name}`: `config.toml` の `[plugins.{name}] enabled = true` を書き換え
- **install（バイナリの存在）と enable（設定の宣言）は分離**（F-56）

# 参照実装

- task_source: [task-source-github](/components/task-source-github.md)（GraphQL）、[task-source-notion](/components/task-source-notion.md)（REST + プロパティマッピング）
- agent_ide: [agent-ide-herdr](/components/agent-ide-herdr.md)（Socket API アダプタ）、[agent-ide-orca](/components/agent-ide-orca.md)（CLI ラップ）
- notifier: [notifier-macos](/components/notifier-macos.md)（osascript）
- 最小骨格: `crates/orchestrator-core/src/bin/mock_plugin.rs`（config 駆動で全 kind を演じるテスト用モック）

# 動作確認

`totsuka config validate`（online で `config/validate` を委譲）と `totsuka doctor`（ライブ疎通 probe）で自作プラグインの疎通を確認できる。
