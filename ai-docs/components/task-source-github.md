---
type: Component
title: task-source-github プラグイン
description: GitHub Issues / ProjectsV2 をタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。GraphQL で fetch→正規化、ProjectsV2 ステータス書き戻しを行い、Issue へは何も書かない。呼び出す 4 つの GraphQL 操作と、トークン権限（十分条件は実測済み・最小値は未実測。fine-grained PAT が user 所有ボードに使えない理由を含む）を扱う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-github
tags: [rust, crate, plugin, task-source, github, graphql, projectsv2]
generated: { by: claude-code/opus-5, at: 2026-08-25T21:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

GitHub Issues / ProjectsV2 を totsuka のタスクソースとして接続する公式プラグイン（F-02）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。ワークスペース初の `plugins/` 配下クレート。#188（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md) Phase B）で protocol 0.1.6 の **push 型**へ移行 — [plugin-sdk](/components/plugin-sdk.md) の `poll_loop` が `initialize` 供給の workflows を内部 cadence（`[github].poll_interval_secs`、既定 60s — 0.6.0 / #554 で `[plugins.github]` から移動）で fetch し、各タスクを `task/submit` で push する。orchestrator 側のポーリングは行われない。

トークンは `initialize` の config で解決済みのものを受領し（F-65）、プラグイン自身は Keychain に触れない。JSON-RPC は stdout、診断ログは stderr（ホストがログへ転送）。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `[github]`（= `InitializeParams.config`）を型付け。`token` / `status_field` / `github_login`（F-08 の自己判定）/ `in_progress_statuses` / `status_map`（orchestrator status→Project option）/ `source_name` / `api_url` / `max_retries`。`deny_unknown_fields`。**ボードはここに無い**（#554）: Orchestrator の `[[projects]]`（`source = "github"` の要素）から `initialize` で届き、`ProjectConfig::resolve` が `RepoInfo.project` の紐付けと突き合わせて組み立てる。要素のキーは `owner` / `owner_type` / `project_number` / `triage_status`（`ProjectOptions`、こちらも `deny_unknown_fields`）。`claimed_repos()` はそこから `initialize` 応答の claim を組み立てる。`triage_status`（任意、#548 派生）を書くと destination に「起票後にこの Status を付けよ」と具体的なコマンド列（`item-list` / `field-list` で id を引いて `item-edit`）が入る — 未設定なら Status なしで追加される |
| `transport` | `GithubTransport` trait（`post_graphql`）＋ reqwest 実装 `ReqwestTransport`（bearer 認証・User-Agent 必須・タイムアウト・指数バックオフ §5.3）。ロジックを録画レスポンスでテストするための seam |
| `client` | `GithubClient<T: GithubTransport>`。`fetch`（ProjectsV2 items を GraphQL 取得→`Task` 正規化→トリガー絞り込み→取り込み制御 F-08）/ `update_status`（SingleSelect option を解決して mutation、未知 option はエラー F-84）/ `publish`（Issue コメント、長文は `<details>` 折りたたみ F-07）/ `validate`（viewer 疎通 F-59）。GraphQL は plain JSON で構築（GraphQL クレート不使用） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。`Server::new(factory, SubmitClient)`（#188: SDK の stdio ランタイム[単一 writer タスク]で駆動され、`LineHandler` 実装経由で serve される）。initialize（config 型付け → client 構築 → triggers があれば SDK `poll_loop` を常駐 spawn — 各 tick で全 trigger を fetch し `task/submit` push。triggers 空なら poll なし）/ config·validate / task·update_status / result·publish / shutdown。`tasks/fetch` は **0.2.0（#190）で削除済み** — 未初期化メソッドは拒否。Session drop（re-initialize 含む）で poll タスクを abort。`TransportFactory` で録画トランスポートを注入しテスト |
| `main` | SDK stdio ランタイム（`plugin_sdk::runtime::stdio` + `serve`）。`ReqwestFactory` を配線。ログは stderr |

# 取り込み制御（F-08）

fetch（`poll_loop` の各 tick が呼ぶ `GithubClient::fetch`。0.2.0 で `tasks/fetch` RPC 自体は削除されたが、`poll_loop` 内部からは引き続き使う）は **`[[projects]]` の全ボードを設定順に走査し**（#542）、ボードごとに: まずワークフローの trigger（`project_status` / `label`）で候補を絞り、次に多人数運用ゲーティングを適用する: assignee が他者のタスクを除外（自分は `github_login` で判定・大小無視）、`in_progress_statuses` のステータスを除外、**そのボードに紐づかないリポジトリ**を除外（紐付けは `[[repositories]].project`、#554）。厳密な排他制御はしない。重複 push は orchestrator が `duplicate` ack で安価に破棄するため、プラグイン側に seen-set は持たない。

**1 ボードの失敗は poll 全体の失敗にする。** そのボードを飛ばして残りを返すと、トークンの失効やボードの削除が「いま取り込むものが無い」と区別できなくなり、静かなボードと同じ見た目になって表に出てこない。

**`task/update_status` はボードを逆引きする。** `TaskUpdateStatusParams` は `{task_id, status}` だけで、どのボードの item かを request が語らない。ingest 時に `task_id → ボードの index` を**プロセス内メモリ**に覚えておき、それを先頭にして**全ボードを順に試す**。メモが外れるのは異常ではなく通常で（再起動でメモは消えるがタスクは残る、item は後からボード間を移動しうる）、メモは最適化であって前提ではない — 見つからなければ試したボードを全部名指しするエラーになる。

**探索中のボードに対象の Status 列が無くても、そこで打ち切らない。** 探索は item が載っていないボードも訪れるので、そういうボードが対象の列を持っている必要はない。ここでエラーにすると**呼び出し側が `?` で探索ループごと抜け**、次のボードなら成功したはずの遷移が失敗する。メモが空になる再起動直後は必ず先頭のボードから当たるので、現実に踏む経路である。列が無いことをエラーにするのは **item がそのボードで見つかった後**で、そのときは意味どおり「このボードの設定が足りない」を指す。

# capabilities（F-83）

manifest（`plugins/task-source-github/plugin.toml`、`protocol_version = ">=0.6.0, <0.7"`）と `initialize` 応答で `kind = task_source` を宣言する。**`outputs` は空**（#398）—— 成果物はエージェントが `gh` で自分で書くので、このプラグインは何も publish しない。`output = "source"` を書いた workflow は `config validate` が弾く（F-83）。

# テスト

`GithubTransport` を録画レスポンスの fake に差し替え、initialize→poll_loop→`task/submit` push（SubmitHarness で観測・ack 注入）、正規化→update_status の全経路を JSON-RPC 境界越しに結合テスト（`tests/integration.rs`）。取り込み制御（他者 assignee / 実行中）、triggers 空での no-poll、トークン無効時の `config/validate`（原因＋次アクション）も検証。実バイナリを stdio で駆動して疎通確認済み。

# 依存

- `plugin-protocol`（プラグイン境界）、[plugin-sdk](/components/plugin-sdk.md)（stdio ランタイム / `poll_loop` / `SubmitClient`）、`reqwest`（GraphQL）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。

# 成果物の書き込み（#398 で非推奨）

`design` / `implement` profile の workflow は `output = "none"` になり、成果物はエージェントが `gh issue comment` などで自分で書く。**`result/publish` の実体は削除済み**（#398。ADR-0033 は「削除は 0.3」と書いたが、実際に消えたのは 0.5 系である）。`answer` / `triage` profile は `output` を `source` に解決するので、このソースで使うには **`output = "none"` を明示して上書きする**（`output` は profile を上書きできる唯一のキー）。代わりに `instructions_kind`（コアが `WorkflowInfo` の専用フィールドで送る。0.6.0 までは trigger に焼き込んでいた）から `[prompts]` の指示文を選び、`Task.instructions` に載せる — これが書き込み先をエージェントへ伝える唯一の経路で、**旧プラグインでは無言で欠落する**（capability 宣言が無いので probe できない。コアと同時にリリースすること）。

# トークンに必要な権限

**十分条件は実測済み、最小値は未実測**（#514、2026-08-23）。下の「実際に呼んでいるもの」は確定した事実で、「実測できたこと」はサンドボックス（user 所有の Project、private リポジトリ 2 本）に対して`.claude/skills/live-e2e/scripts/github-permissions.sh` が 4 操作すべてを実際に投げて確かめた。「導いた権限」の側は**依然として導出**である — 権限を削ったトークンをまだ試していないので、そこに書かれた値が**最小**であることは示されていない。**この但し書きは実測が済むまで消さないこと。** 断定に固まると、間違っていたときに誰も疑わなくなる。

## 実際に呼んでいるもの

全て `https://api.github.com/graphql` への単一 POST に bearer トークンを載せる形で、操作は **4 つだけ**である。REST も Contents API も使わない。**書き込みは Project のカード移動だけ**で、Issue へは何も書かない（#398 で `addComment` ごと消えた）。

| 操作 | 触るもの |
|---|---|
| Project アイテム取得 | `user`\|`organization` → `projectV2(number:)` → `items`。アイテムごとに Issue の `id number title body url`、`repository { name }`、`assignees`、`labels` |
| Project / フィールド / アイテムの id 解決 | `projectV2 { id, field(name:) { options }, items { id } }` |
| カード移動 | `updateProjectV2ItemFieldValue` |
| 疎通確認 | `viewer { login }` |

## 実測できたこと（2026-08-23）

`bash .claude/skills/live-e2e/scripts/github-permissions.sh probe --write` を、実 GitHub の
サンドボックス（`tomoya-k31` 所有の Project #7 / private リポジトリ 2 本）に対して実行した。
このスクリプトはプラグインと**同じエンドポイント・同じヘッダ・同じクエリ本文**で 4 操作だけを投げる。

| トークン | 結果 |
|---|---|
| OAuth（`gh auth token`）scope = `gist, project, read:org, repo, workflow` | **4 操作すべて成功**。`title` / `body` / `url` / `number` / `repository.name` は 62/62 件で非 null、`assignees` / `labels` も `nodes` が null にならず非空を確認。カード移動（write）も成功 |

これが言うのは「**この scope 集合で足りる**」までで、**どれが要らないかは言っていない**。

**「エラーが出なかった」を pass と読まないこと。** GraphQL の権限不足は HTTP 200・`data` あり・
**フィールドが `null`** という形で出うる。スクリプトが `errors` の有無と独立にフィールド単位で
present/null を判定しているのはこのためで、`assignees` / `labels` は
**`nodes: null`（権限不足の疑い）と `nodes: []`（本当に付いていない）を別物として数える**。

## fine-grained PAT は user 所有ボードには使えない

**Account permissions に Projects は存在しない。** GitHub の
[fine-grained PAT の権限一覧](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens)
が挙げる User permissions に Projects は無く、Projects は **Organization permissions にしか無い**
（[community discussion #156512](https://github.com/orgs/community/discussions/156512) が同じ制約を追っている）。

したがって:

- **org 所有**のボード → fine-grained PAT が使える（Organization permissions の Projects）
- **user 所有**のボード → fine-grained PAT では ProjectsV2 に到達できない。**scope を持つトークン**を使う —
  classic PAT の `project` scope、または `gh auth token` が返す OAuth トークン（同じく `project` scope を含むもの）。
  下の実測はまさに後者で通しているので、**「classic PAT でなければ駄目」ではない**。効いているのは
  トークンの呼び名ではなく **scope を持つ方式かどうか**である

サンドボックスは user 所有なので、**fine-grained PAT の最小値はこの環境では測れない**。
測るには org 所有の Project が要る。

## 導いた権限

**fine-grained PAT**（org 所有ボードのみ。未実測）:

| 種別 | 権限 | なぜ |
|---|---|---|
| Repository | **Metadata: Read** | 必須（他の Repository 権限の前提） |
| Repository | **Issues: Read** | Project アイテム経由で読む Issue の本文・ラベル・アサイニー。**write は不要**（#398 で `addComment` が消えた） |
| Organization | **Projects: Read and write** | ProjectsV2 の読み取りと `updateProjectV2ItemFieldValue`。**Organization permissions にしか無い** — user 所有ボード向けの Account permissions は存在しないので、その場合は classic PAT を使う（上節） |

**Contents は不要**である。このトークンでリポジトリの中身を読み書きすることはない。

**scope ベースのトークン**（classic PAT、または `gh auth token` の OAuth トークン。user 所有ボードではこちら。最小値は未実測）: `project`（ProjectsV2 の読み書き）と、`repo`（private リポジトリを含む場合）または `public_repo`。private org のボードでは `organization(login:)` の解決に `read:org` も要りうる。

**未解決の問い**: Issue の本文・ラベル・アサイニーは `projectV2` のアイテム経由でしか読んでおらず、Issues エンドポイントを直接は叩かない。**`project` scope だけでこれらが返るなら `repo` は要らない**。どちらなのかは `project` だけの classic PAT を切って上のスクリプトを回せば 1 回で分かる（#514 手順 2）。

## PR 作成はこのトークンの仕事ではない

`implement` profile のワークフローが PR を開くとき、`gh pr create` を実行するのは**エージェント自身**であって、このプラグインではない。エージェントはペインの環境にある**あなた自身の `gh` 認証**を使う。

したがって `gh auth login` は**別個の前提条件**であり、ここで設定する PAT とは無関係である。`totsuka doctor` はこれをエージェントツールの検査として別に見る。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [plugin-sdk](/components/plugin-sdk.md)
- [ADR-0008 task/submit push 取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [Spec §4.2 タスクソース / F-02・F-04・F-07・F-08・F-84](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
