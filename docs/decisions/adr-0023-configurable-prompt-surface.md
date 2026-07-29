---
type: Decision
title: ADR-0023 AI ツールへ差し込むプロンプトは設定可能にし、実行を決める面は設定不可のまま残す
description: claude/codex/opencode へ注入するプロンプト文をコードから外出しし config.toml から上書き可能にする一方、スクリプト・argv・permission ブロック・ステータスマーカーは設定不可のまま残す決定。上書きはインライン文字列のみで、ファイルパス指定と TOTSUKA_PROMPTS_* env は不採用。マーカー規約を失う上書きは検証エラーで止める。
resource: https://github.com/tomoya-k31/totsuka/issues/311
tags: [decision, prompt, config, security, marker, adr]
generated: { by: human:tomoya-k31, at: 2026-07-30T22:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

Accepted（2026-07-30、エピック [#311](https://github.com/tomoya-k31/totsuka/issues/311)）。

[ADR-0020](/decisions/adr-0020-status-marker-stays.md)（マーカー存置）を **supersede しない** — 本 ADR はその決定を前提に、マーカーを「教える散文」だけを設定可能にする。
[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)（llm 検収はセッション内 prompt 型 Stop フック）、
[ADR-0014](/decisions/adr-0014-tool-abstraction.md)（`[tools]` レジストリ = ツール知識は core の設定に置く先例）、
[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)（`TOTSUKA_*` は明示ホワイトリスト）と関連する。

# Context

claude / codex / opencode に差し込むプロンプト文が Rust の文字列リテラルとしてソースに散在しており、**文言を調整するたびにコード変更とリビルドが必要**だった。対象は 6 箇所:

| 場所 | 内容 |
|---|---|
| `run/hooks.rs` `MARKER_SELF_REPORT_INSTRUCTION` | 全ディスパッチに注入される完了自己申告指示 |
| `hooks/mod.rs` `DEFAULT_RUBRIC` / `BACKGROUND_EXEMPTION` / `marker_convention()` | `verification = "llm"` の prompt 型 Stop フック本文 |
| `hooks/totsuka-plan.md` | opencode plan モードのエージェント markdown |
| `agent-ide-orca` `default_plan_prefix()` | plan モードのプロンプト前置き |
| `task-source-slack` `pipeline.rs` | 返信案指示 + body テンプレート |
| `task-source-slack` `llm.rs` | リポジトリ分類 LLM の system / user / retry プロンプト |

これらは「モデルに何をどう伝えるか」という**運用上のチューニング対象**であり、動作を変えるコードとは性質が違う。運用者が試行錯誤する対象がリリースサイクルに縛られていた。

一方で、同じファイル群には**動作そのものを決める面**が同居している。opencode の plan エージェント markdown は YAML frontmatter に `permission: {edit: deny, bash: deny, task: deny}` を持ち、この deny マップが plan モードの読み取り専用保証そのものである。フックスクリプト（`.sh`）と opencode の JS プラグインは実行されるコードである。

# Decision

## 1. プロンプト文はデータとして外出しし、設定で上書き可能にする

組み込みデフォルトを埋め込み `defaults.toml`（`include_str!` + `LazyLock`）へ移し、`config.toml` の `[prompts]`（グローバル）と `[[workflows]].prompts`（ワークフロー単位）で上書きできるようにする。プラグイン側は自分の `plugins/{name}.toml` を使う（プラグインは core の設定を見られないため）。

## 2. 守る一線: プロンプトは「何を伝えるか」だけを変え、「何が動くか」は変えない

**プロンプトキーはスクリプト・argv・permission ブロックを追加も改変もできない。** 具体的には:

| 面 | 設定可否 | 理由 |
|---|---|---|
| プロンプトの散文 | **可** | 本 ADR の目的 |
| ステータスマーカー（`<<STATUS:COMPLETED>>` 等） | 不可 | `on-stop.sh`（bash）と `totsuka-opencode.js` がリテラルをパースする。ADR-0020 が 3 ツール共通の唯一の完了信号と定めている |
| フックスクリプト 6 本（`.sh`） | 不可 | 実行されるコード |
| opencode JS プラグイン | 不可 | 実行されるコード |
| plan エージェントの YAML frontmatter（`permission` を含む） | 不可 | 散文に見えるキーから `bash: allow` を注入できてしまうと**権限昇格**になる |
| `[tools]` の argv（`command` / `mode_args` / `plan_args`） | 可（ADR-0014 の範囲） | 本 ADR とは別軸の既存決定 |

## 3. 上書きはインライン文字列のみ

`{ file = "~/.config/totsuka/prompts/marker.md" }` のようなパス指定形式は採らない。

## 4. マーカー規約を失う上書きは検証エラーにする

`config validate` / `run` / `doctor` が、**組み立て後**の `marker_self_report` を検査し、マーカーへの言及が 1 つも無ければ**エラー**として起動を止める。プレースホルダのタイポ（`{marker_completd}` 等）も同じくエラーにする。

## 5. `TOTSUKA_PROMPTS_*` env override は追加しない

# 検討した選択肢

## 上書き値の形式

| 案 | 判断 |
|---|---|
| **インライン文字列のみ** | **採用。** 既存の `rubric` / `pr_body_template` と同じ形で、`config show` / redaction / validate がそのまま効く |
| `{ file = "..." }` のパス指定 | **不採用。** repo 相対パスを持ち込めてしまい、決定 2 の一線を破る。リポジトリに置かれたファイルがプロンプトになるということは、リポジトリへの書き込み権限がプロンプト注入権限になるということである。加えて読み込み失敗・相対パス解決・doctor 検査が増える |

## デフォルトの置き場

| 案 | 判断 |
|---|---|
| **埋め込み `defaults.toml` 1 枚（crate ごと）** | **採用。** ユーザー config と同じ形なので差分が読みやすく、キー一覧がファイル内で完結する |
| `.md` 個別ファイルに分割して `include_str!` | **不採用。** キー一覧が結局 Rust 側の配列に残り、「コードで管理しない」が半端になる |
| `$XDG_DATA_HOME` へ書き出して読む | **不採用。** doctor の改竄検知・自己修復は「ディスク上の内容が期待値と違えば drift」という設計で、ユーザーが編集する前提のファイルとは意味論が衝突する |

## マーカー規約を失う上書きの扱い

| 案 | 判断 |
|---|---|
| **`marker_self_report` はエラー、`verification_prompt` は警告** | **採用。** 完了自己申告の指示なのに完了マーカーに一切触れない、という上書きに正当なユースケースが無い。検収文の再構成には正当なユースケースがある |
| 両方エラー | 不採用。rubric だけにしたいユースケースを潰す |
| 両方警告 | 不採用。警告は起動を止めないので、タイポで完了検知が壊れてもエスカレーション待ちまで気づけない |

## `TOTSUKA_PROMPTS_*` env override

**不採用。** 複数行の散文を env に通すのは footgun で、ADR-0009 の選定基準は「CI が `config.toml` を書き換えずに差し替えたいスカラー」である。プロンプトはこれに該当しない。

## `on-stop.sh` の block reason（マーカー再送指示）

**設定化しない（スコープ外）。** 理由は 4 つ。

1. 中身の実体はマーカー構文そのもので、設定化しても変わるのは周りの文言だけ
2. **セーフティネットは素のままにする。** 前倒し注入はこの block をほぼ発火させないための仕組みであり、主経路と fallback の両方を上書き可能にすると設定ミス 1 つでセッションからマーカーの言及が完全に消える
3. 手書き JSON なので、ユーザー文字列中の `"` や改行で不正 JSON になる。安全にやるには `jq -n --arg` が要るが、`on-stop.sh` には jq 不在時の fail-open 分岐があり、その経路で reason が丸ごと消える
4. `env_overrides::RESERVED` が 1 つ増える（ADR-0009 は意図的に狭く保っている）

代わりに、`on-stop.sh` が 3 つの `MARKER_*` 定数を含むことを assert するドリフト検知テストを置く。

# Consequences

## 良くなること

- プロンプトの文言調整がリビルドなしになり、ワークフロー単位で試せる
- 実機で得た知見（前倒し提示・配送契約・バックグラウンドタスク中の非マーカー）が、**上書きするユーザーが読む場所**である `defaults.toml` のキー直上に置かれる
- `[tools]`（ADR-0014）に続き、ツール固有の知識が core の設定として一元化される

## 受け入れるコスト・リスク

- **上書きミスで完了検知が壊れうる。** 緩和は決定 4 の検証エラーと、決定「`on-stop.sh` は固定」による第 2 のチャンス
- **アセットの意味論が変わる。** `orchestrator-<workflow>.json` と `agents/totsuka-plan.md` は config 由来のレンダリング結果になる。ドリフト検知は嘘にならない（`verify_one` は毎回 config から再レンダリングした期待値と比較する）が、[フックのセキュリティ](/security/hook-security.md) §3 の「静的埋め込み」の主張は**プロンプト文には当てはまらなくなる**
- **稼働中セッションには届かない。** `[prompts]` を編集すると次の `run` / `doctor` が settings ファイルを書き換えるが、既に起動しているエージェントには反映されない。プロンプト変更は**次のディスパッチから有効**
- 信頼境界は変わらない。`config.toml` はユーザー自身の XDG config 配下にあり、ペインに直接打ち込むのと同じ信頼領域である。決定 2 の一線を守る限り、攻撃面は増えない

# 実装

エピック [#311](https://github.com/tomoya-k31/totsuka/issues/311) の子 issue として段階的に実装した。

| PR | 内容 |
|---|---|
| #312 | `template` モジュール抽出（シングルパスレンダラの共通化） |
| #313 | `prompts` レジストリ + 埋め込み `defaults.toml`（挙動保存） |
| #314 | `[prompts]` / `[[workflows]].prompts` の設定面 |
| #315 | 検証（決定 4）+ doctor の上書き数表示 + 本 ADR |
| #316 | opencode plan エージェント（frontmatter は固定） |
| #317 | agent-ide-orca |
| #318 | task-source-slack |
