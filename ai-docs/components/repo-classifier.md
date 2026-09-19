---
type: Library
title: repo-classifier
description: リポジトリ分類（タスク・メンションが設定済みリポジトリのどれに属するか）の共有クレート。RepoClassifier trait と検証済みの Classification、HTTP の 1 往復を抽象した HttpTransport（本番は reqwest）、指数バックオフの RetryPolicy、OpenAI 互換 /chat/completions の ChatClassifier を持つ。orchestrator-core と task_source プラグインの双方が使う leaf クレート。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/repo-classifier
tags: [rust, crate, llm, repo-select, classifier, http, shared]
generated: { by: claude-code/opus-5, at: 2026-09-19T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

「この仕事は、候補のうちどのリポジトリのものか」を外部の分類 API に訊き、**検証済みの判定**を返す。訊いた結果をどう扱うか（閾値、pending、ピッカー、訊き直し）は持たない —— それは呼び出し側の方針である（[ADR-0083](/decisions/adr-0083-repo-classifier-crate.md)）。

呼び出し側は 2 つ。

- [orchestrator-core](/components/orchestrator-core.md) の `repo_select`（repo hint を持たないタスク、F-11〜F-14）。分類器は `adapters::llm::gateway_classifier()` が `[llm]` から組む
- [task-source-slack](/components/task-source-slack.md) の `llm`（メンションのリポジトリ解決の ② 段）

ワークスペース内のどのクレートにも依存しない leaf で、arch-lint が `classifier-leaf` として検査する（[依存境界ルール](/architecture/workspace-dependency-rules.md)）。

# 公開インターフェース

| 型 | 役割 |
|---|---|
| `RepoClassifier` | `classify(&ClassifyRequest) -> Result<Classification, ClassifyError>`、`probe()`（疎通確認。再試行しない、F-111）、`reset_connections()`（スリープ復帰時に接続プールを捨てる）。RPITIT なので dyn にはできない |
| `ClassifyRequest` | `subject: Vec<(label, text)>`（何を分類するか。表示順）・`candidates: Vec<Candidate>`・`chat_prompt: Option<ChatPrompt>`（chat 実装にだけ効く、呼び出し側が描画したプロンプト） |
| `Candidate` | `name` / `summary` / `readme_head` |
| `Classification` | `repo`（必ず候補のどれか）・`confidence`（必ず `[0, 1]`）・`reason`（表示用。判断には使わない） |
| `ClassifyError` | `Transport` / `Timeout` / `Status{status, message}` / `InvalidResponse` / `UnknownRepo`。判定メソッドは `is_auth_failure`（401/403）・`is_unreachable`（transport・timeout・5xx、F-111）・`is_retryable`（左記 + 429）・`is_bad_answer`（`InvalidResponse` / `UnknownRepo`、F-14 の訊き直し対象）。`ClassifyError::status(code, body)` は本文を標準 error envelope の `error.message` に絞る（プラグインには redact 層が無い） |
| `HttpTransport` | `post_json(HttpRequest{url, api_key, body, timeout})`。テストが差し替える seam。`&T` / `Arc<T>` にも実装済み。API キーはリクエストに載る（Slack は `initialize` でキーを知るため） |
| `ReqwestTransport` | 本番のトランスポート。`RwLock<reqwest::Client>` で `reset_connections` がプールごと差し替える。transport エラーは `scrub_urls` で URL の userinfo とクエリを削る |
| `RetryPolicy` | `NONE` / `STANDARD`（3 回・500ms から倍々・上限 60 秒）。`is_retryable` の失敗だけを再試行する |
| `ChatClassifier<T>` / `ChatSettings` | `{base_url}/chat/completions`。`ChatOutput::JsonSchema`（strict な `response_format`、`reason` 必須。core）と `ChatOutput::Prose`（`response_format` なし、JSON を前後の文から抽出、`reason` 任意。Slack）。`ChatPrompt.correction` があれば、読めない回答を訂正付きで 1 回だけ再試行する。`probe` は schema なし・`max_tokens: 1`・再試行なしで、2xx なら本文は読まない |
| `ApiKey` | `Debug` で中身を出さない newtype |
| `scrub_urls` | URL の資格情報とクエリを落とす。core の `LlmHealth` も健全性の理由文で使う |
