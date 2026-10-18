---
type: Library
title: llm-choice
description: LLM に固定の選択肢から 1 つ選ばせる共有クレート（旧 repo-classifier、ADR-0118）。問いは呼び出し側が ChoiceRequest（subject と Question の id・instructions・選択肢・none の有無）で渡し、ChoiceClassifier が検証済みの Answer（選択肢 or どれも当てはまらない）を返す。HTTP の 1 往復を抽象した HttpTransport（本番は reqwest）、指数バックオフの RetryPolicy、OpenAI 互換 /chat/completions の ChatClassifier、判定専用モデル（TypeSafe Jev）の DecisionsClassifier、設定で選ぶ ConfiguredClassifier を持ち、リポジトリ分類は repo モジュールの問いの 1 つ。orchestrator-core と task_source プラグインの双方が使う leaf クレート。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/llm-choice
tags: [rust, crate, llm, repo-select, classifier, http, shared]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T13:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

「この対象について、選択肢のうちどれか」を外部の LLM API に訊き、**検証済みの答え**を返す。訊いた結果をどう扱うか（閾値、pending、ピッカー、訊き直し）は持たない —— それは呼び出し側の方針である（[ADR-0083](/decisions/adr-0083-repo-classifier-crate.md)）。何を訊くかも持たない —— 問いは呼び出し側が渡す（[ADR-0118](/decisions/adr-0118-llm-choice-crate.md)）。

問いは今のところ 1 つで、リポジトリ分類（`repo` モジュール）。呼び出し側は 2 つ。

- [orchestrator-core](/components/orchestrator-core.md) の `repo_select`（repo hint を持たないタスク、F-11〜F-14）。分類器は `adapters::llm::gateway_classifier()` が `[llm]` から組む
- [task-source-slack](/components/task-source-slack.md) の `llm`（メンションのリポジトリ解決の ② 段）

ワークスペース内のどのクレートにも依存しない leaf で、arch-lint が `classifier-leaf` として検査する（[依存境界ルール](/architecture/workspace-dependency-rules.md)）。

# 公開インターフェース

| 型 | 役割 |
|---|---|
| `ChoiceClassifier` | `choose(&ChoiceRequest) -> Result<Answer, ClassifyError>`、`probe()`（疎通確認。再試行しない、F-111）、`reset_connections()`（スリープ復帰時に接続プールを捨てる）。RPITIT なので dyn にはできない |
| `ChoiceRequest` | `subject: Vec<(label, text)>`（何を判定するか。表示順）・`question: Question`・`chat_prompt: ChatPrompt`（chat 実装にだけ効く、呼び出し側の言葉で書いた問い。decisions は `Question.instructions` を読む） |
| `Question` | `id`（decisions の `questions` のキー、chat の回答 JSON のキー）・`instructions`（decisions の質問文。`{none}` は「どれでもない」の実際のキーに置換）・`options: Vec<ChoiceOption{key, description}>`・`none: Option<String>`（decisions に「どれでもない」を足すときの説明。chat は無視する） |
| `Answer` | `Chosen { key, confidence, reason }`（`key` は必ず選択肢のどれか、`confidence` は必ず `[0, 1]`、`reason` は表示用で判断には使わない）か `NoneFits { reason }`（`none` を足した問いで decisions だけが返す） |
| `ClassifyError` | `Transport` / `Timeout` / `Status{status, message}` / `InvalidResponse` / `UnknownChoice`。判定メソッドは `is_auth_failure`（401/403）・`is_unreachable`（transport・timeout・5xx、F-111）・`is_retryable`（左記 + 429）・`is_bad_answer`（`InvalidResponse` / `UnknownChoice`、F-14 の訊き直し対象）。`ClassifyError::status(code, body)` は本文を標準 error envelope の `error.message` に絞る（プラグインには redact 層が無い） |
| `HttpTransport` | `post_json(HttpRequest{url, api_key, body, timeout})`。テストが差し替える seam。`&T` / `Arc<T>` にも実装済み。API キーはリクエストに載る（Slack は `initialize` でキーを知るため） |
| `ReqwestTransport` | 本番のトランスポート。`RwLock<reqwest::Client>` で `reset_connections` がプールごと差し替える。transport エラーは `scrub_urls` で URL の userinfo とクエリを削る |
| `RetryPolicy` | `NONE` / `STANDARD`（3 回・500ms から倍々・上限 60 秒）。`is_retryable` の失敗だけを再試行する |
| `ChatClassifier<T>` / `ChatSettings` | `{base_url}/chat/completions`。回答は `{"<question.id>": string, "confidence": number, "reason": string}`。`ChatOutput::JsonSchema`（strict な `response_format`、`enum` は選択肢のキー、`reason` 必須。core）と `ChatOutput::Prose`（`response_format` なし、JSON を前後の文から抽出、`reason` 任意。Slack）。`ChatPrompt.correction` があれば、読めない回答を訂正付きで 1 回だけ再試行する（選択肢外・範囲外の回答は再試行しない）。`probe` は schema なし・`max_tokens: 1`・再試行なしで、2xx なら本文は読まない |
| `ApiKey` | `Debug` で中身を出さない newtype |
| `scrub_urls` | URL の資格情報とクエリを落とす。core の `LlmHealth` も健全性の理由文で使う |
| `DecisionsClassifier<T>` / `DecisionsSettings` | 判定専用モデル（TypeSafe Jev）。`endpoint`（**完全な URL**。既定 `DecisionsSettings::OPENROUTER_ENDPOINT` = OpenRouter の `POST /api/alpha/decisions`、alpha）に `{model, state, questions}` を送る。`state` は subject をラベル付きオブジェクトにしたもの、質問は `question.id` をキーにした `choice` 1 つで、`criteria` のキーは選択肢のキーそのまま（`/` `.` `_` を含む名前が通ることを実キーで確認済み）、値は description（無ければ `null`）。`question.none` があれば「どれでもない」を足す（キーは `none`、選択肢と衝突したら `_` を前置して一意にする）。回答は `answers.<id>.choice`。`none` なら `NoneFits`、それ以外は **`probabilities[choice]` を confidence とし**、無ければ `confidence` で代用、両方無ければ `InvalidResponse`。理由文は分布から組み立てる（`<model>: totsuka p=0.84 (next: dotfiles 0.15)`）。`probe` は 1 語の state に `noul` 質問 1 つ（約 270 入力トークン） |
| `ConfiguredClassifier<T>` | `Chat` / `Decisions` のどちらか。trait が dyn にできないので、設定で方式を選ぶ呼び出し側はこれを持つ。`endpoint()` は `doctor` が表示する呼び出し先 |

## `repo` — リポジトリ分類の問い

| 項目 | 役割 |
|---|---|
| `Candidate` | `name` / `summary` / `readme_head` |
| `ClassifyRequest` | `subject`・`candidates: Vec<Candidate>`・`chat_prompt: Option<ChatPrompt>`（`None` なら `default_prompt`） |
| `Classification` | `Repo { repo, confidence, reason }` / `NoneFits { reason }`。`Answer` と相互に `From` |
| `classify(&classifier, &ClassifyRequest)` | `choice_request` で組んだ問いを `choose` して `Classification` で返す |
| `question(candidates)` | id `repo`、固定の質問文（"Which repository should this work be done in? Choose `{none}` only when no repository fits."）、選択肢は候補名（説明は summary + `README (head):`）、`none` あり |
| `default_prompt(subject, candidates)` | core が使う組み込みの chat プロンプト |

送る本文は repo-classifier 時代とバイト単位で同じで、chat / decisions の単体テストが期待値で固定している。回答 JSON のキーが `repo` のままなので、Slack の `classifier_*` の上書きは無改修で動く。

# 実機検証

`tests/live_openrouter.rs` は本物の OpenRouter Decisions API に当てる検査で、`#[ignore]` 付き（実キーと費用が要るので CI では回さない）。decisions 実装や OpenRouter の alpha endpoint が変わった可能性があるときに回す:

```bash
OPENROUTER_API_KEY="$(op read 'op://Dev/Openrouter/api_key')" \
  cargo test -p llm-choice --test live_openrouter -- --ignored --test-threads=1 --nocapture
```

2026-09-19 に repo-classifier として 3 件とも通過した（#723）。改名後はまだ回していない。
