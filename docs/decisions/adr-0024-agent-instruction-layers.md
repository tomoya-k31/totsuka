---
type: Decision
title: ADR-0024 エージェントへの指示は task_source プラグインが所有し、実行エンベロープだけを設定と core が持つ
description: ペイン内エージェントに渡す散文（手順・テンプレート・書式）を task_source プラグインの prompts が Task.instructions 経由で所有し、tools 設定と core は argv のツール制限・モデル・タイムアウトだけを持つ決定。Slack の books 起票フローで確定した。allowedTools は制限ではなく付与であること、ペインがオペレーターの settings.json を継承することを前提として明記し、スキル注入・agents インライン JSON・initialPrompt は不採用とする。
resource: https://github.com/tomoya-k31/totsuka/issues/324
tags: [decision, prompt, tool, permission, slack, agent, adr]
generated: { by: claude-code/opus-5, at: 2026-07-31T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: cc-cli-reference
    resource: https://code.claude.com/docs/en/cli-reference
    title: Claude Code — CLI reference
  - id: cc-permissions
    resource: https://code.claude.com/docs/en/permissions
    title: Claude Code — Permissions
  - id: cc-settings
    resource: https://code.claude.com/docs/en/settings
    title: Claude Code — Settings（precedence / merge）
  - id: cc-sub-agents
    resource: https://code.claude.com/docs/en/sub-agents
    title: Claude Code — Subagents（--agent / --agents / initialPrompt）
  - id: cc-skills
    resource: https://code.claude.com/docs/en/skills
    title: Claude Code — Skills
---

# Status

stable。[#324](https://github.com/tomoya-k31/totsuka/issues/324)（Slack の `:books:` リアクションから GitHub Issue を起票する）の設計中に確定した。実装は #324 に従い [#319](https://github.com/tomoya-k31/totsuka/issues/319) のマージ後。

本 ADR は [ADR-0014](/decisions/adr-0014-tool-abstraction.md)（ツール知識は orchestrator 側）と [ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md)（プロンプトは設定可能・実行面は設定不可）を、**プラグインが持つ散文**という軸で補う。どちらも改訂しない。

# Context

`:books:` フローは「Slack スレッドを読み、`gh` CLI で Issue を起票し、ProjectsV2 に追加する」という定型作業をペイン内のエージェントにやらせる。Rust で GitHub を叩かない代わりに、**手順をエージェントへ確実に届け、危険な操作を防ぐ**必要がある。

当初の設計は、固定手順を `[tools.<name>].mode_args` の `--append-system-prompt` に、`gh` 実行の許可を `--allowedTools` に置いていた。実機仕様と本リポジトリの実装を突き合わせた結果、その土台が 3 点で成立していなかった。

## 1. `--allowedTools` は制限ではなく付与である

CLI リファレンスの定義は「プロンプトなしで実行されるツール」であり、続けて **restrict したいなら `--tools` を使え**と書かれている。[^cc-cli-reference] narrow に列挙しても、列挙外が拒否されるわけではない。

## 2. ペインはオペレーターの `~/.claude/settings.json` を丸ごと継承する

totsuka が `--settings` で渡すファイル（`crates/orchestrator-core/src/hooks/mod.rs` の `render_settings`）は **`hooks` キーしか持たない**。Claude Code の設定はレイヤ間でマージされ、書かなかったキーは下位レイヤの値が残る。[^cc-settings] したがってオペレーターの `permissions.allow` / `permissions.defaultMode` / `enabledPlugins` / MCP 構成はペイン内でそのまま効く。

`permissions.allow` に `Bash(gh:*)` があれば `gh api` も通り、`defaultMode = "auto"` なら確認プロンプトも出ない。**`--allowedTools` を narrow に書くことの意味は「他人の環境での最低限保証」に留まる。**

これは意図した設計である（ペインは「オペレーターの環境で代わりに作業させている」ものなので、本人の許可リストが効くのが自然）。ただし前提として明文化しないと、安全性の議論が環境依存であることを見落とす。

## 3. 散文の所有者が割れていた

返信の作法（`reply_instructions`）は `plugins/task-source-slack/src/defaults.toml` の `[prompts]` にあり、ADR-0023 の枠組みで `[[workflows]].prompts` から上書きできる。一方で起票の作法だけを `[tools]` の system prompt に置くと、**ワークフロー固有の散文がワークフロー横断のツールプロファイルに入る**。これは #324 自身が「ProjectsV2 の情報を `[tools]` に置かない理由」として挙げた罠と同じ構造である。

# Decision

## 1. 層で所有者を分ける

| 種類 | 所有者 | 経路 |
|---|---|---|
| **ドメインの散文**（手順・テンプレート・書式・行き先） | task_source プラグインの `[prompts]` | `Task.instructions` |
| **実行エンベロープ**（ツール制限・モデル・思考量・タイムアウト） | `[tools.<name>]` / `[[workflows]]` | argv（core が組み立てる） |

散文は 1 か所に集まり、CLI フラグの知識は orchestrator 側に留まる（ADR-0014 / H-01 を維持）。core は Slack や GitHub のドメイン知識を持たない。

副次効果として **散文がツール可搬になる**。`Task.instructions` は `TOTSUKA_PROMPT_CONTEXT` → `UserPromptSubmit` フックの `additionalContext` として不可視注入されるが、`invisible_injection` を持たないツール（opencode）では可視の `extra_context` に降りる経路が既に実装されている（`crates/orchestrator-core/src/run/mod.rs`）。argv に置いた散文はこの縮退に乗れない。

## 2. ツール制限は `--tools` と `--disallowedTools` だけで行う

- `--tools` — built-in ツールの集合そのものを絞る。**MCP ツールには効かない**[^cc-cli-reference]
- `--disallowedTools` — deny。「どのレイヤで deny されても他のレイヤは allow できない」[^cc-permissions] のでオペレーターの allow に勝つ。素のツール名を書くとそのツールが**文脈から消える**

`--allowedTools` は使わない。付与であって制限ではないため（Context 1）。

## 3. `permissionMode` は指定しない

ペインは無人なので、確認プロンプトが出ても誰も答えない。`permissionMode: default` にするとオペレーターの `auto` を上書きしてプロンプトが復活し、`timeout_secs` 経過後に F-103 でエスカレーションする。これは「安全側の失敗」ではなく **「時間切れまで待たされて失敗する」** という UX である。`bypassPermissions` も採らない（deny の塗り漏れがそのまま事故になる）。

## 4. 安全保証は 4 段で、`disallowedTools` はその一部でしかない

`Bash` が残っている限り `cat > file` / `sed -i` / `tee` は通るので、**`disallowedTools` は「構造的不可能性」を作らない**。実効的な保証はこう積む:

| 守るもの | 保証 | 種類 |
|---|---|---|
| 本体リポジトリが汚れない | worktree で作業する | 構造的 |
| リモートが汚れない | `output = "source"` + F-86 + `Bash(git push:*)` deny | 構造的 |
| 破壊的な GitHub 操作をしない | `Bash(gh api:*)` / `gh issue delete` 等の deny | 構造的（サブコマンド単位） |
| worktree 内のファイルを編集しない | 散文のみ。worktree ごと捨てるので実害なし | 散文 + 使い捨て |

保証の本体は **隔離**（worktree ≠ 本体リポジトリ、push は構造的に不可能）であって、廃棄ではない。廃棄はディスク衛生の問題として `[worktree].cleanup` で扱う。

## 5. F-86 の適用範囲は「成果物の公開」に限る

F-86（agent_ide プラグインの成果はコミットまで。push / PR 作成は Orchestrator の責務）は改訂しない。F-86 が守っているのは**ワークフローの成果物（コード変更）を外部に公開する経路の一元管理**であり、対象は push と PR である。`gh issue create` は成果物の公開ではなく**タスクの登録**で、worktree のコミットとは無関係（`output = "source"` により push は構造的に抑止されている）。

# 検討した選択肢

## 手順を Claude Code の「スキル」にして `--plugin-dir` で配る

**不採用。** 出発点はこの案だった。

- 手順が 1KB 程度のうちは遅延ロードの利得がゼロ。スキルの主な価値である「本文をプロンプトから追い出す」は、`Task.instructions` でも同じだけ得られる
- 資産の materialize（配置先・冪等な書き出し・バージョン管理）という新機構を背負う
- **プラグイン由来の subagent は `permissionMode` / `hooks` / `mcpServers` を無視する**[^cc-sub-agents] ため、権限制御をプラグインに持ち込めない
- スキルは Claude Code 固有で、codex / opencode に対応物が無い。散文を `Task.instructions` に置けばツール可搬になる（Decision 1）

手順が育って supporting files（テンプレート・スクリプト）を同梱したくなった時点で再検討する。その場合も**呼ばせる**のではなく subagent 定義の `skills` フィールドで**起動時にプリロードする**（全文が文脈に注入されるのでモデルの判断が要らない）のが正しい使い方になる。

## `--agents` インライン JSON + `--agent <name>`

**不採用。** `--agents` は subagent をその場で定義し、`--agent` はメインスレッド自身をその定義にするフラグで、`tools` / `disallowedTools` / `maxTurns` / `model` / `effort` / `skills` を 1 か所で書ける。argv には JSON 文字列として載るのでリテラル改行が入らず、[#124](https://github.com/tomoya-k31/totsuka/issues/124) の事故領域も避けられる。

それでも採らない理由:

- **`--agent` は Claude Code の既定 system prompt を完全に置換する**（`--system-prompt` と同じ挙動）[^cc-sub-agents]。books はリポジトリのコードを読ませるので、ツール利用のガイダンスを失うのは明確な劣化リスク
- ツール制限は `--tools` / `--disallowedTools` 単体で得られるので、置換のリスクを取ってまで `--agent` を選ぶ理由がない
- 失うのは `maxTurns` だけで、`timeout_secs` + F-103 が代替する

狭い定型ジョブで既定 system prompt を捨ててよいと判断できる場面が出たら、そこで改めて採ればよい。

## `--agents` の `initialPrompt` で起動の合図を自動投入する

**不採用。** これは好みの問題ではなく、採ると壊れる。

`initialPrompt` はメインセッションエージェントとして動くとき最初の user ターンとして自動投入され、コマンドとスキルが処理される[^cc-sub-agents]ので、一見すると「確定的にスキルを起動する」手段に見える。

しかし herdr プラグインの `submit_prompt`（`plugins/agent-ide-herdr/src/agent.rs`）は、`agent.send` でテキストを打ち込んだ後 **「エージェントが既に走っていたら Enter を押さずに早期リターン」** する。この分岐は `--resume` のために必要で消せない。

`initialPrompt` の自動投入でこの条件が必ず true になるため:

1. `Task.body`（起動の合図）と `extra_context`（core が前置するマーカー自己申告指示）が **永久に投入されない**
2. エージェントはマーカー規約を知らないまま終わり、Stop は UNKNOWN → エスカレーション
3. 打ち込まれた本文が入力欄に居座り、次ターンの追い回答を汚染する

競合ではなく決定論的な取りこぼしであり、症状は「エスカレーションだけ見える」ので原因に辿り着きにくい。

## `--append-system-prompt` / `--append-system-prompt-file`

**不採用。** 動作はする。`--append-system-prompt-file` を使えば手順が Markdown ファイルになり `rumdl` もかかる。しかしワークフロー固有の散文がワークフロー横断のツールプロファイルに入るという構造は変わらない（Context 3）。

## core 側の `verification = "pattern"`（`last_assistant_message` を正規表現で判定）

**見送り。** `last_assistant_message` は claude / codex / opencode の 3 ツールとも同じ UDS 契約で core に届いている（`crates/orchestrator-core/src/hooks/totsuka-opencode.js` ほか）。`verification = "llm"` が claude / codex 限定なのは「判定材料が無いから」ではなく「判定をセッション内のモデルにやらせているから」に過ぎない。

core 側で正規表現判定する mode を作れば 3 ツール共通・決定論的・LLM コストゼロで、`regex` は既に `orchestrator-core` の依存にある。ただし `VerificationMode` の variant 追加・`[[workflows]]` の設定と検証・engine の分岐と、**core 改修になる**。

今回は core 無改修を優先し、opencode で `verification = "llm"` が `human` に縮退することを受容する（起票自体は回るが、完了報告に `totsuka task verify` が要る）。opencode を主力ツールにするなら再検討する。

## command 型 Stop フックで URL を検証する

**不採用。** `on-stop.sh` / codex 用スクリプト / `totsuka-opencode.js` の **3 本すべてに実装が要る**。同じことを core 側 1 箇所でできる pattern 検証に明確に劣る。

## totsuka が render する settings に `permissions` / sandbox を持たせてペインを隔離する

**不採用。** ペインをオペレーターの設定から切り離せば `--allowedTools` の narrow 指定が意図どおり効き、環境をまたいだ再現性も出る。しかし代償として、オペレーターが自分の許可リストで滑らかに回していた操作が確認プロンプトで止まるようになり、無人ペインではそれが時間切れ失敗になる（Decision 3 と同じ構図）。

「ペインはオペレーターの環境で代わりに作業させている」という位置づけを採り、継承を正とする。

# Consequences

## 良くなること

- 散文の置き場が 1 つに決まる。Slack の返信の作法と起票の作法が同じ `[prompts]` に並び、同じ上書き機構（ADR-0023）で調整できる
- 散文が **ツール可搬** になる。opencode でも手順が届く（可視 `extra_context` に降りる）
- `orchestrator-core` にドメイン固有の散文が入らない。#324 が掲げた「変更は `plugins/task-source-slack` と設定ファイルに閉じる」が core 改修なしで維持される
- 禁止事項の一部（`gh api` / `git push` / Edit / Write）が散文から **deny という実効的な制限** に格上げされる
- TOML の `"""` に行末 `\` を置く必要がなくなる。argv を経由しないのでリテラル改行が入ってよい

## 受け入れるコスト・リスク

- **`Bash` 経由のファイル書き込みは防げない。** 保証は worktree の隔離と使い捨て性に依存する（Decision 4）
- **MCP ツールはペインに入ったままになる。** `--tools` は MCP に効かず、オペレーターが Slack MCP を有効にしていればエージェントが直接投稿でき、`result/publish` の承認フローを迂回しうる。これは `slack-reply` にも存在する既存の穴で、[#331](https://github.com/tomoya-k31/totsuka/issues/331) に切り出した
- **`--allowedTools` を narrow に書くことの安全上の意味は環境依存になる。** オペレーターのグローバル設定は `totsuka doctor` から見えないので、books の実効的な権限が環境ごとに変わる
- **opencode では `verification = "llm"` が `human` に縮退する。** 完了報告が自動化されず `totsuka task verify` が要る。`marker_block: false` なのでマーカー欠落の Stop は即エスカレーションになる
- `instructions` は `UserPromptSubmit` が毎ターン発火するので長文を毎回再注入する。一発完了なら実害は小さい

# 実装

`plugins/task-source-slack/src/defaults.toml` の `[prompts]` に `books_instructions` を追加し、`src/config.rs` の placeholder テーブルと `prompt_entries` に `reply_instructions` と同じ流儀で登録する（placeholder は持たない）。`build_issue_task` が組む構造化セクション（対象リポジトリ / permalink / ProjectsV2 / 対象メッセージ / スレッド全文）の**前**に置く。

`[tools.claude-gh-issue]` の `mode_args` は実行エンベロープだけを持つ:

```toml
[tools.claude-gh-issue]
kind = "claude"
command = "claude"
mode_args = [
  "--tools", "Bash,Read,Grep,Glob",
  "--disallowedTools",
  "Edit", "Write", "NotebookEdit",
  "Bash(gh api:*)", "Bash(gh issue close:*)", "Bash(gh issue edit:*)",
  "Bash(gh issue delete:*)", "Bash(gh repo delete:*)", "Bash(gh secret:*)",
  "Bash(git push:*)", "Bash(git commit:*)",
]
```

`mode_args` の各要素は argv の 1 トークンとして herdr の `agent.start` に渡り、**シェルを経由しない**。`--tools` はカンマ区切りの 1 要素だが、`--disallowedTools` が argv 配列で複数値をどう取るか（可変長 / フラグ反復 / カンマ区切り）は実機で確認する必要がある。**ここが外れると deny が黙って全部無効になる**ので、実装前に潰す最優先項目である。

同じく実機で確認する項目:

- deny がオペレーターの `permissions.allow` に本当に勝つか（`Bash(gh:*)` が allow されている状態で `Bash(gh api:*)` を deny する）
- `--tools "Bash,Read,Grep,Glob"` で調査が回るか。止まるようなら追加する

詳細な段階分割と残りの検証項目は #324 に記載する。

[^cc-cli-reference]: Claude Code — CLI reference
[^cc-settings]: Claude Code — Settings（precedence / merge）
[^cc-permissions]: Claude Code — Permissions
[^cc-sub-agents]: Claude Code — Subagents（`--agent` / `--agents` / `initialPrompt`）
