---
type: Decision
title: ADR-0056 複数トラッカーは単一プラグイン内のリストで持ち、repo→トラッカーの順方向マッピングはプラグイン設定を正本にする
description: "GitHub Project / Notion Database を複数同時に polling し、Slack 発の依頼を解決済みリポジトリに対応するトラッカーへ起票するための設計。複数プラグインインスタンスを不採用にし、単一プラグイン内の [[projects]] / [[databases]] リストを採る。repo→トラッカーの順方向マッピングはプラグイン設定を唯一の正本とし、initialize 応答の claimed_repos で Orchestrator へ伝える。Slack 発の起票は既存 triage profile の Agent 経路を完成させる形にし、新規 RPC は足さない。"
resource: https://github.com/tomoya-k31/totsuka/issues/542
tags: [decision, task-source, github, notion, slack, routing, config, protocol, adr]
generated: { by: claude-code/opus-5, at: 2026-08-27T03:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable、ただし **§4 は [ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) が置き換えた**（#554）。

- **§4（repo→トラッカーの順方向マッピングはプラグイン設定を正本にする）は無効。** マッピングは
  `config.toml` の `[[projects]]` と `[[repositories]].project` へ移り、プラグイン側の
  `repos = [...]` は消えた。却下理由に挙げた「二重管理」と「core が固有概念を知る」は、
  情報を*複製*せず*移動*し、core が読むのを参照だけに留める形には当たらなかった
- **§1〜§3 は生きている。** 複数対象を単一プラグイン内のリストで持つこと（複数インスタンスを
  採らない）、旧トップキーを互換なしで削除したこと、要素を「対象を特定するキー + repos」に
  絞ったこと —— いずれも変わらない。変わったのは `repos` の出どころだけである
- `initialize` 応答の `claimed_repos` も生きている。宛先の散文は core が組み立てられないため

以下の Decision / Consequences は **2026-08-23 時点の決定として読むこと**。§4 と、`ClaimConflict`
（2 プラグインが同じリポジトリを主張したときの報告）に言及している箇所は ADR-0058 が上書きする
—— その状態は現在**書けない**ので、機構ごと削除された。

stable。[#542](https://github.com/tomoya-k31/totsuka/issues/542) の実装とともに確定した。

[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md)（プラグイン名の不変条件）を**維持したまま**複数対象を扱う決定である。[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)（push ingestion）と [ADR-0024](/decisions/adr-0024-agent-instruction-layers.md)（指示の層構造）の上に乗る。

# Context

`plugins/github.toml` は `project_number` を 1 つ、`plugins/notion.toml` は `database_id` を 1 つしか持てない。実運用では複数リポジトリを横断し、リポジトリごとに別の board / database を使っている。

前提: **1 リポジトリが使うトラッカーは 1 つ**。1 リポジトリに複数ツールを紐付ける要件は無い。

解くべき問題は 3 つある。

1. GitHub Project を複数 polling する
2. Notion Database を複数 polling する
3. Slack 発のタスクを、解決済みリポジトリに対応するトラッカーへ起票する

3 が設計上いちばん重い。**リポジトリ → トラッカーの順方向マッピングがどこにも無い**からである。プロトコルの `RepoInfo` と `config.toml` の `[[repositories]]` はリポジトリを*説明*するだけで、タスクの `repo_hint` は item → リポジトリという*逆*方向を指す。「`totsuka` についての新しい依頼 → どの board に載せるか」に答えられるデータが存在しない。

# Decision

## 1. 複数対象は単一プラグイン内のリストで持つ。複数インスタンスは不採用

`plugins/github.toml` に `[[projects]]`、`plugins/notion.toml` に `[[databases]]` を置く。

```toml
# plugins/github.toml
[[projects]]
owner = "tomoya-k31"
owner_type = "user"
project_number = 7
repos = ["totsuka", "dotfiles"]

[[projects]]
owner = "my-org"
owner_type = "organization"
project_number = 3
repos = ["web-app"]
```

`source_name` は据え置き。したがって `[[workflows]].source = "github"` の結合（`domain/workflow.rs` の `w.source == task.source`）は無傷で、ワークフロー定義は 1 行も変わらない。

**複数インスタンス（`[plugins.github-a]` / `[plugins.github-b]`）を採らない理由**は ADR-0027 そのものである。`plugin.toml` の `name` は `[plugins.<name>]` の設定キー・`plugins/<name>.toml` のファイル名・ストアのディレクトリ名・bin 名・ワークフローの `source` すべての識別子であり、複数インスタンスは `name ≠ bin 名` を要求する。これは ADR-0027 が「2 つの命名規則が併存する状態を作らない」として却下した緩和の復活にあたる。加えて運用者に `plugins/github-a.toml` と `plugins/github-b.toml` の重複と、board ごとのワークフロー複製を強いる。

**タスク id が衝突しない**ことがこの選択を安全にしている。GitHub の issue node id と Notion の page id はどちらもグローバル一意なので、1 プラグインが複数 project/database を扱っても冪等キー `UNIQUE(source, source_task_id)` は衝突しない。

## 2. 旧トップキーは削除する（破壊的）。互換で残さない

`owner` / `owner_type` / `project_number` / `repos`（github）と `database_id`（notion）はトップから消す。両 config は `#[serde(deny_unknown_fields)]` なので、旧設定は `initialize` で硬く落ちる。

単数キーを残して配列と排他にする案は不採用。**「両方書いたらどちらが効くか」を定義する羽目になる**うえ、コードパスが 2 本になる。

**移行案内も置かない。** 旧キーを検出して書き換え方を示すコードは書けるが、totsuka は #542 の時点でまだ非公開で、既存の設定ファイルは**実運用 1 本と live-e2e 1 本の合計 2 本**しかなく、どちらも #542 の作業で書き換える。案内を受け取る相手が存在しないうちに案内を書くと、以後ずっと保守する対象が 1 つ増えるだけになる。serde の `unknown field` で落ちるので、壊れたことは黙らない。

## 3. 配列要素は「対象を特定するキー + `repos`」だけに絞る

ステータス列（`status_field` / `status_map` / `in_progress_statuses`）・`property_map`・`prompts`・`token` はトップ共有のまま（`status_map` はその後 [ADR-0062](/decisions/adr-0062-status-vocabulary.md) で廃止された）。board ごとに列名が違う運用はありうるが、今の要件に無い。必要になったら additive で要素側に上書きキーを足せる — 先に入れると「効いていない設定」を作る。

Notion の `[[databases]].repos` は**新設の挙動**である。これまで Notion 側に repo フィルタは無かったので、`repo_hint` プロパティの値がその要素の `repos` に無いページは skip されるようになる。

## 4. repo → トラッカーの順方向マッピングはプラグイン設定を唯一の正本にする

`[[projects]].repos` / `[[databases]].repos` が正本。`config.toml` の `[[repositories]]` には**何も足さない**。

同じ情報を core 側にも書くと二重管理になり、ズレたときにどちらが正しいかを定義する罠が増える。core に `tracker = "github"` を持つ案も、`tracker = { source, project_number }` まで持つ案も、この理由で不採用とした（後者は core がプラグイン固有の概念を知ることにもなる）。

Orchestrator へは `initialize` 応答の新フィールドで伝える（protocol 0.5.1、additive）:

```rust
pub struct ClaimedRepo {
    pub repo: String,        // [[repositories]].name
    pub destination: String, // どこへ・どうやって起票するか（Agent 向けの散文）
}
```

**`destination` を構造体ではなく散文にした**のは、消費者がコードではなく Agent のプロンプトだからである。`{project_number, owner}` のような構造にすると、Orchestrator が各トラッカーの形を知り、それを文へ組み立て直す責務を負う — この設計が避けたい結合そのものである。加えて将来 task_source が増えるたびにバリアントの追加、つまりプロトコル変更が要る。

代償は明示しておく: **`destination` の内容は機械検査されない**。守るのは triage の検収 rubric だけで、これは [ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md) の read-only と同じクラスの保証（散文でしか言っていない）である。

`claimed_repos` が空であることは「このプラグインは claim しない」であって、「このリポジトリにトラッカーが無い」ではない。0.5.1 より前のプラグインは省略で空を返すので、この 2 つを混同すると古いプラグインが「トラッカー無し」に見える。

**重複の検査は 2 層**に分ける。同一プラグイン内の repo 重複は `config/validate` がエラーにする（設定の誤りとして、その場で直せる）。github × notion 跨ぎの重複は Orchestrator が claim を突き合わせて起動時 warn + `doctor` で検出する（片方のプラグインからは見えないため）。

## 5. Slack 発の起票は既存 triage 経路を完成させる。新規 RPC は足さない

Orchestrator が `profile = "triage"` のタスクを dispatch するとき、解決済みリポジトリを claim している source の `destination` をプロンプトへ追記する。ADR-0024 の「指示の上乗せ」層で、`initial_prompt` と同格に置く。

この形を採ったのは、**起票する経路が既にあり、足りないのは宛先だけ**だったからである（#324 の `:books:` → `triage`：Agent がスレッドを読み `gh issue create` して URL を報告する。`task-source-slack` の `defaults.toml` が「the orchestrator does not file issues」と書いているとおり）。

副作用として、**GitHub 発の triage タスク（ある repo の issue を読んで別 repo のボードへ起票する）にも同じ仕組みが効く**。Slack 専用の配線にしなかったのはこのためでもある。

dispatch 時に注入するので、プラグインの起動順序に依存しない。`launch_plugins` は `cfg.plugins` のイテレーション順に逐次起動するので slack が github より先に `initialize` されうる — initialize 時に配る設計はこの順序と #495 の個別再起動の両方で壊れる。

### 検討して採らなかった案

**core 仲介の `task/register`（P→O）→ `task/create`（O→P）**。Slack が起票内容を送り、Orchestrator が claim している source へ転送してプラグインが API で起票する。不採用の理由は、**起票内容を書く主体として Agent のほうが質が高い**こと。Agent はリポジトリを読んでから書ける。加えて triage 経路と二重になり、各プラグインに書き込み API とメソッド 2 つが要る。

**Slack の `[llm]` / core の `[llm]` で title/body を草案する**。同じ理由で不採用。リポジトリを読まない要約は誤要約のリスクを足すだけである。

**Slack プラグインが GitHub/Notion API を直接叩く**。決定 4 の「正本は 1 つ」を崩し、token を `plugins/slack.toml` に複製する。

**Notion プラグインにページ作成 API を持たせる**。GitHub 側は Agent の `gh`、Notion 側はプラグインの API という非対称になる。Notion への書き込みは **Agent 環境の Notion MCP を前提**とし、totsuka は `destination` に `database_id` と `property_map` の列名を渡すだけにする。GitHub の `gh` と対称で、`doctor` はこの前提を検査しない（Agent 環境の道具立ては totsuka の管轄外）。

## 6. `task/update_status` の逆引きはプラグイン内で解く

`TaskUpdateStatusParams` は `{task_id, status}` だけで、GitHub の実装は設定済みの単一 project の中から item を探していた。複数 project では「この task はどの project の item か」を引く必要がある。

poll で取り込んだ `task_id → 要素 index` をプラグイン内メモリに持ち、未知（再起動直後など）なら全要素を順に探すフォールバックを置く。プロトコルは変えない。

`task_id` を project ごとに前置修飾する案は不採用 — `Task.id` は冪等キーの構成要素であり、これを変えると既存タスクの同一性が壊れる。

# Consequences

- 既存の `plugins/github.toml` / `plugins/notion.toml` は**そのままでは起動しない**。変換が要る（実運用・live-e2e とも #542 の作業に含める）。**この判断は「利用者が作者 1 人」という現時点の事実に依存している** — public 化（#506）より後に同種の破壊的変更をするなら、移行の扱いを決め直すこと。
- `[[projects]].repos` / `[[databases]].repos` が**必須・非空**になる。従来 `repos` を省略して「project 内の全 repo」を取り込んでいた構成は、リポジトリ名を明示する必要がある。これは claim を生成するための情報が他に無いためで、緩めると順方向マッピングが穴あきになる。
- `destination` の文面は機械検査されない。誤りは Agent が誤ったボードへ起票する形で現れる。triage の検収 rubric が「投稿先に載った証拠の URL」を要求することで、少なくとも*何も起票しなかった*ケースは落ちる。
- Notion 起票は Agent 環境に Notion MCP がある構成でしか動かない。無い場合、Agent は `destination` を読んでも実行手段が無く、検収で落ちる（黙って成功はしない）。
- protocol 0.5.1 は additive だが、`InitializeResult` はプラグインが構築する型なので、**リポジトリ外のプラグインは再コンパイルが要る**（`claimed_repos` の初期化子が増える）。ワイヤは互換で、0.4.2 の `not_released` 追加と同じ形である。
