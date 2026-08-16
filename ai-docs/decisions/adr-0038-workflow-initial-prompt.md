---
type: Decision
title: ADR-0038 ワークフローごとの追加プロンプトは可視チャネルの先頭へ、新規会話のときだけ入れる
description: "`[[workflows]].initial_prompt` を追加するにあたり、不可視の TOTSUKA_PROMPT_CONTEXT ではなく可視の extra_context を選んだ理由、注入を resume_session_id で判定する理由、既存の prompts テーブルに統合しなかった理由、無人ハングを運用者責任にした理由。"
tags: [config, prompts, workflow, dispatch, 415]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T00:00:00Z }
---

# Context

エージェントへの追加指示を書く手段が、**Slack ソースの `reply_instructions` / `implement_instructions` しか無かった**（`plugins/task-source-slack/src/config.rs`）。GitHub / Notion ソースには存在せず、しかもソース単位でワークフロー単位でもない。

「`design` ワークフローのときだけ `/grill-me` で設計を詰めさせたい」といった、**ワークフローの性格に紐づく指示**を書く場所が無い。#192 の対応方針 1 はこれを「`task-source-github` プラグインに instructions を書き込む」で解こうとしていたが、それはプラグインごとに同じものを実装して回ることになる。

`[[workflows]].initial_prompt`（`Option<String>`）を足す。

# Decision

## D1 — 可視 `extra_context` の先頭で配送する

不可視の `TOTSUKA_PROMPT_CONTEXT`（`UserPromptSubmit` の `additionalContext`）ではなく、pane に見える `extra_context` に入れる。

1. **pane が唯一のデバッグ面。** `initial_prompt` はタスクの進め方を丸ごと変えうる指示なので、pane に映らないと後から「なぜこの動きをしたのか」を追えない
2. **不可視チャネルは「requester に届く成果物に混ぜたくないもの」専用。** ソース所有の返信スタイル（`Task.instructions`）とマーカー規約がそこに載っているのはそのため。`initial_prompt` は運用者が書いたタスク整形で、配信上の懸念がない
3. `invisible_injection` ケイパビリティに依存しない**単一経路**で済む

**不可視 `TOTSUKA_PROMPT_CONTEXT` の中身は一切変更していない。**

### 「スラッシュコマンドを展開させるため」ではない

これは**先に書きかけて誤りと分かった根拠**なので、打ち消しとして残す。`/grill-me` のような**スキル**は CLI のスラッシュコマンド展開を必要としない — モデルがテキストを読んで `Skill` ツールを呼ぶので、可視・不可視のどちらでも起動する。可視を選ぶ根拠は上の 3 点だけである。

### 位置に選択肢はない

herdr の `compose_prompt` が `{extra_context}\n\n---\n{task_body}` を組み立てて pane にタイプするので、ここに入れたものは自動的に先頭になる。前置き → タスク本文の順は #196 以前から変わっていない。名前が `prompt`（単数）なのは、**body を置き換えるものではなく body の前に付く前置き**だから。

## D2 — 注入は「エージェントがこの指示を記憶していないなら」＝ `resume_session_id.is_none()`

判定は `build_params` クロージャの**内側**に置く。

- resume する（= 会話継続）→ エージェントは初回に読んでいる → 入れない
- resume しない（= 新規セッション）→ 入れる
- **resume 非対応ツール**は毎回が新規セッションなので毎回入る。これは正しい挙動

クロージャの内側であることが効くのは `SESSION_UNRESUMABLE` の経路である。resume を落として再ディスパッチするとき、その 2 回目は**実際に新規セッションになる** — エージェントは何も覚えていない。内側で判定していれば `initial_prompt` が自動的に復活する。

### なぜ「毎回」が有害か

`/grill-me` のようなスキル起動コマンドは「今からこの手順で始めろ」という**開始宣言**である。会話の 3 ターン目に再入力されるとスキルが再起動し、それまでに積んだ文脈を壊す。

### なぜ `latest_session(record.id).is_none()` ではないのか

それは文字どおり「タスク行で初めてのディスパッチ」を意味するので、**resume 非対応ツールや retry では「エージェントは何も覚えていないのに指示も入らない」穴**ができる。判定すべきは「タスクにとって初めてか」ではなく「このエージェントがそれを読んでいるか」である。

## D3 — 既存の `prompts` テーブルに統合しない

`[prompts]` / `[[workflows]].prompts`（#314）のキー群とは別のトップレベルフィールドにする（その 2 面は [#465](https://github.com/tomoya-k31/totsuka/issues/465) で削除されたが、`initial_prompt` は別レイヤなので影響を受けない）。

| | `prompts.*` | `initial_prompt` |
|---|---|---|
| 中身 | 落とすと壊れる wire 規約の散文 | タスク指示の上乗せ |
| 操作 | 置換のみ | 連結 |
| 検証 | `missing_markers` / `ALLOWED_PLACEHOLDERS` で厳格 | なし |
| 送り先 | 不可視チャネル or Stop フック | 可視 pane の先頭 |

`defaults.toml` に 8 キー目として置くと「空文字列の既定値」という異質な行が入り、`embedded_defaults_toml_parses` の「空でないこと」テストも例外扱いになる。

グローバル既定（`[prompts]` 相当の層）も作らない。作ると「置換か連結か」という優先順位の問いが新たに生まれる。ワークフローは数個なのでコピペで足りる。（#465 がその `[prompts]` 自体を削除したので、この判断は結果的に先回りになった。）

## D4 — テンプレート変数を持たない

`{task_title}` / `{task_url}` のような展開は入れない。

- title / body / url は**同じ pane にタスク本文として既に入る**ので重複する
- `{` を含む文（JSON 例、コード断片、Rust のフォーマット式）をリテラルで書けなくなる
- **後から足すのは非破壊、外すのは破壊的**。まだ要求が無いうちに入れる理由がない

## D5 — 空白のみは「未設定」に倒す。validate はしない

`""` / `"   "` は `Workflow::from_config` で `None` に正規化する。書いて空にしたものを起動時エラーにするほどの事故ではなく、読み方が明白だから。正規化を解釈の 1 箇所でやることで、下流が trim を思い出す必要がなくなる。

`TOTSUKA_STATUS:` を含む場合に警告する案も採らない。実際に踏む人がほぼいないのに warning が壁紙化する。

## D6 — 無人ハングは設定した運用者の責任

追加プロンプトが「人間に問いかける」内容だった場合に何が起きるかは把握したうえでの判断である:

- `profile = "design"` → 無人 pane
- `/grill-me` の中身は「`AskUserQuestion` で 1 問ずつ聞き、回答を待ってから次へ進め」
- 無人 pane で `AskUserQuestion` を出すと **Stop すら発火しない**（ツール応答待ちで停止）
- → 無シグナル → `timeout_secs`（既定 1800s）で Escalated（F-103）

core が但し書きを自動で足すと、`initial_prompt` に書いた内容と**矛盾する指示が混ざりうる**。対話系スラッシュコマンドをブラックリストで検出する案も、リストが持続しないうえ、スキル以外の対話要求（「確認してから進めて」等）は原理的に検出できないので採らない。

`marker_self_report`（`prompts/defaults.toml`）は `NEEDS_INPUT (human input required)` を教えているが、「無人実行なので対話ツールは使えない」とは言っていない。これを常時教えるかどうかは**本件とは独立した別問題**で、今も Slack の `implement_instructions` 経由で同じ事故が起きうる。必要になったら別途起票する。

# Consequences

## 良くなること

- ワークフロー単位の追加指示が、**設定駆動・プラグイン非依存**で書けるようになった。#192 の対応方針 1（プラグインに書き込む）はこれで置き換わる
- 未設定なら `extra_context` は**バイト同一**なので、既存構成に対して挙動変化がゼロ

## 引き受けたコスト

- **指示の置き場が増えた。** `[prompts]` / `[[workflows]].prompts` / `rubric` / `initial_prompt` / ソースプラグインの `*_instructions` が並存する。層が違う説明は [config.toml リファレンス](/development/config-reference.md) に置いたが、「どこに書くか」を最初に迷う面は増えた（この並存は [#465](https://github.com/tomoya-k31/totsuka/issues/465) が前 2 者を削除して `rubric` / `initial_prompt` / プラグインの `*_instructions` の 3 つに減った）
- **無人ハングを踏める口を増やした。** D6 のとおり実装でもドキュメントでも面倒を見ない
- **実機検収は未了。** mock プラグインでは `extra_context` が組み立てられたことまでしか確かめられず、pane に実際に打ち込まれてエージェントが指示どおり動くかは実機でしか見えない

# 関連

- [config.toml リファレンス](/development/config-reference.md) — キーの仕様と運用上の注意
- [ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) — プロンプト外出しの原則（権限に関わる決定を設定文字列から到達可能にしない）
- [ADR-0024](/decisions/adr-0024-agent-instruction-layers.md) — 指示の所有層。**紛らわしいので注意**: あちらの不採用案に Claude Code ネイティブの `initialPrompt`（`agents` インライン JSON のキー）があるが、本 ADR の `[[workflows]].initial_prompt` は**別物**である。名前が似ているだけで、こちらは totsuka の設定キーで、届け方は pane への可視入力であり、Claude Code の機構には一切依存しない
- [ADR-0033](/decisions/adr-0033-workflow-profile.md) — workflow profile
