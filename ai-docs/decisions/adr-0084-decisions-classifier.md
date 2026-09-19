---
type: Decision
title: ADR-0084 リポジトリ分類に decisions モデル（TypeSafe Jev）を [llm].api で選べるようにする
description: 文章を生成せず、候補から 1 つを選んで全候補の確率を返す判定専用モデル（TypeSafe Jev、OpenRouter 経由）を、[llm].api = "decisions" で chat と並ぶ分類方式として加えた決定。設定は raw → 型付き enum へ try_from で変換して API に合わないキーを読み込みエラーにする。閾値と比べるのは API の confidence ではなく選ばれた候補の確率であること、「どれも当てはまらない」の選択肢を必ず足すこと、質問文を設定可能にしないこと、alpha の endpoint を上書き可能にするだけで仕様変更には備えないこと、プラグインへは decisions のとき空の base_url で渡して古いプラグインに chat として誤用させないこと、[llm].api を env で切り替えられないことを記録する。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/repo-classifier/src/decisions.rs
tags: [decision, adr, llm, repo-select, classifier, jev, openrouter, protocol, config]
generated: { by: claude-code/opus-5, at: 2026-09-19T22:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-723
    resource: https://github.com/tomoya-k31/totsuka/issues/723
    title: "decisions モデル（TypeSafe Jev）を [llm].api で選べるようにする — #723"
  - id: live-check
    resource: https://github.com/tomoya-k31/totsuka/issues/723#issuecomment-5738409820
    title: "実キーでの検証結果（2026-09-19、OpenRouter /api/alpha/decisions）"
  - id: openrouter-decisions
    resource: https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-questions-and-answers-request.md
    title: "OpenRouter — Submit a Decisions (questions and answers) request"
  - id: typesafe-confidence
    resource: https://docs.typesafe.ai/confidence
    title: "TypeSafe — Confidence"
  - id: adr-0083
    resource: /decisions/adr-0083-repo-classifier-crate.md
    title: "リポジトリ分類を共有クレート repo-classifier にまとめる — ADR-0083"
---

# Status

**採択（stable）。** #723 の PR 2 本目で実装した。[ADR-0083](/decisions/adr-0083-repo-classifier-crate.md) が作った `RepoClassifier` の切れ目に、2 つ目の実装として載せている。

# Context

TypeSafe の Jev は「System One」と呼ばれる判定専用モデルで、文章を生成しない。判定対象（`state`）と、答えの集合が決まった質問（`choice` / `noul` / `score`）を送ると、集合のうち 1 つと、全選択肢の確率分布が返る。候補外の答えは構造上返らない。入力は $0.042 / 100 万トークン、出力は無料で、応答は約 100ms。リポジトリ分類（「この仕事は候補のどれか」）は、そのまま `choice` 1 問になる。

OpenRouter から既存のキーで使えるが、**`/chat/completions` では呼べない**（カタログ上 `modality: text->decisions`、`supported_parameters: []`）。呼び先は `POST https://openrouter.ai/api/alpha/decisions` で、TypeSafe 本家の `POST /v1/systemone` と同じ本文を取る。[^openrouter-decisions]

実キーで確かめたこと[^live-check]:

- `probabilities` と `confidence` はどちらも返る（OpenRouter のスキーマ上はどちらも省略可）
- `criteria` のキーに `/` `.` `_` を含むリポジトリ名（`tomoya-k31/totsuka`、`dotfiles.nvim`）がそのまま通り、`choice` にも元の名前で返る
- どれにも当てはまらない仕事では、足しておいた `none` が p=0.98 で選ばれる
- 確率は小数第 2 位に丸められる。応答の `model` は `typesafe/jev-1.13-20260917` のような OpenRouter 形式の固定版 ID

# Decision

## 1. `[llm].api` で方式を選ぶ。既存の設定はそのまま動く

```toml
[llm]
api = "decisions"                 # 省略時 "chat"
model = "~typesafe/jev-latest"
api_key_ref = "keychain:totsuka/openrouter"
# endpoint = "https://openrouter.ai/api/alpha/decisions"
```

- 共通: `api` / `model` / `api_key_ref` / `timeout_secs` / `confidence_threshold`
- chat だけ: `base_url`（必須）/ `max_tokens`
- decisions だけ: `endpoint`（任意。**完全な URL**。ゲートウェイごとにパスが違うので `base_url` からは導かない）

読み込みは平たい `RawLlmConfig` を `try_from` で型付きの `LlmConfig { api: LlmApi::Chat{..} | LlmApi::Decisions{..}, .. }` に変換し、**`api` に合わないキーは読み込みエラー**にする。`deny_unknown_fields` と同じ考え方で、書いたのに効かない行を黙って許さない。

**`[llm].api` を切り替える環境変数は作らない。** 切り替えると必須キーが変わり、env だけでは正しいテーブルを作れない —— `[llm]` を env から合成しないのと同じ理由である。`TOTSUKA_LLM_ENDPOINT` は足し、`api` に合わないキーの上書きはエラーにした。

## 2. 閾値と比べるのは、選ばれた候補の確率

API の `confidence` は「分布がどれだけ一点に集中しているか」で、選ばれた候補の確率とは別の統計量である（確率 0.84 に対して confidence 0.6 の例がある）。[^typesafe-confidence] chat 側の confidence は「どれくらい自信があるか」を LLM が自己申告した値で、`[llm].confidence_threshold` はそれを前提に 0.6 を既定にしてきた。**同じ閾値の意味を保つには、選ばれた候補の確率を使う**のが近い。`confidence` は確率が返らなかったときの代わりにだけ使い、両方無ければ悪い答え（`InvalidResponse`）として 1 回訊き直してから pending にする。

## 3. 「どれも当てはまらない」を必ず選択肢に足す

TypeSafe の推奨どおり、`criteria` に `none` を足す。選ばれたら新しい判定 `Classification::NoneFits` を返し、core は訊き直さずに pending、Slack はピッカーを出す。**`none` が無いと、当てはまらない仕事でも確率の高い候補が 1 つ選ばれ、閾値を越えれば誤ったリポジトリで作業が始まる。** リポジトリ名が `none` だったときは `_` を前置して衝突を避ける。

## 4. 質問文は設定可能にしない

Slack の chat 経路は `prompts.classifier_*` を設定で変えられるが、decisions の質問文（`instructions`）は固定にする。答えの意味は選択肢（候補名と説明）が決めるので、文言を変えられても良くなる余地が小さく、質問と選択肢が食い違う余地だけが増える。

## 5. alpha の endpoint は上書き可能にするだけで、仕様変更には備えない

OpenRouter の Decisions API はパスに `alpha` が付く。**リクエストとレスポンスの形を抽象化する層は作らない。** やるのは、呼び先を `[llm].endpoint` で差し替えられるようにすること（TypeSafe 直の `https://api.typesafe.ai/v1/systemone` も同じ本文で動く）と、alpha であることを設定リファレンスに書くことだけ。形が変わったら、そのとき実装を直す。

## 6. プラグインへは、decisions のとき空の `base_url` で渡す（protocol 0.7.4）

task_source プラグインは orchestrator の `[llm]` を `InitializeParams.llm`（`LlmInfo`）で受け取り、自前の設定が無ければ分類の既定として使う（#119）。`LlmInfo` に `api` と `endpoint` を足したが、**プラグインがどのフィールドを理解するかを orchestrator は知る手段が無い**（マニフェストの版範囲は下限を上げない限り新旧を区別しない）。

そこで decisions の `LlmInfo` は **`base_url` を空文字**で送る。0.1.2 以来、同梱の Slack プラグインは空の `base_url` を「供給なし」として扱うので、`api` を知らない古いプラグインは decisions の既定を採用せず、自前の `[slack.llm]` に落ちる。**chat の URL をここに入れると、古いプラグインが判定専用モデルに chat リクエストを送る**ことになる。`api` は chat のとき wire に出さないので、chat の `LlmInfo` は 0.7.3 以前とバイト単位で同じである。未知の `api` 値は `LlmApiKind::Other` として受け、`initialize` を失敗させない。

# Consequences

- 実キーで 1 回だけ叩いたときの費用は、分類 1 回で約 400 入力トークン（約 $0.000017）、疎通確認 1 回で約 270 入力トークン（約 $0.000011）
- decisions の確率は 0.01 刻みなので、`confidence_threshold` も 0.01 刻みでしか意味を持たない
- `LlmInfo` を構造体リテラルで組むコードは source break（0.7.3 の `ConfigValidateResult.warnings` と同じ形）
- 自前の分類器を持つプラグインを他に書くときも、`repo-classifier` の `ConfiguredClassifier` を使えば両方式に対応できる

[^openrouter-decisions]: OpenRouter — Submit a Decisions (questions and answers) request
[^live-check]: 実キーでの検証結果（2026-09-19、OpenRouter /api/alpha/decisions）
[^typesafe-confidence]: TypeSafe — Confidence
