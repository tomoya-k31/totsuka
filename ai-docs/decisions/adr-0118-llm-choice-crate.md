---
type: Decision
title: ADR-0118 repo-classifier を問いを外から渡す llm-choice クレートへ汎用化する
description: 共有クレート repo-classifier を llm-choice へ改名し、「どのリポジトリか」に固定されていた問いを ChoiceRequest（question の id・instructions・選択肢・none の有無）として呼び出し側が渡す形に作り替えた決定。リポジトリ分類は llm_choice::repo のヘルパーが組む question の 1 つになり、挙動は変えない。chat の回答 JSON のキーは question の id なので、既存の classifier_* プロンプトの上書きはそのまま動く。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/llm-choice
tags: [decision, llm, classifier, crate, refactor, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T20:16:02+09:00 }
status: stable
owner: tomoya-k31
---

# Status

**採択（stable）。** #927 の stacked PR 1/3（挙動不変のリファクタ）で実装した。[ADR-0083](/decisions/adr-0083-repo-classifier-crate.md) の「trait は分類で切る」を、「分類の**問い**は呼び出し側が持つ」へ一般化する。[ADR-0084](/decisions/adr-0084-decisions-classifier.md) の decisions backend はそのまま載り替える。

# Context

Slack プラグインに「このメンションは返信が要るか」を LLM で判定する処理を足す（ADR-0120、#927 の 3/3 で追加）。chat と decisions（TypeSafe Jev）の両方で、候補から 1 つ選ばせて確信度を返す、という形は repo 分類とまったく同じである。

ところが repo-classifier は問いを 4 か所で固定している。

| 固定箇所 | 中身 |
|---|---|
| chat の回答 JSON | フィールド名 `repo`（JsonSchema の `properties` と `RawVerdict`） |
| decisions の質問 | id `repo`、文 "Which repository should this work be done in?"、`none` を必ず足す |
| 候補の型 | `Candidate { name, summary, readme_head }` |
| 組み込みプロンプト | "You route a software task to the correct repository…" |

このまま使うには「reply / no_reply という名前のリポジトリ」を渡すことになり、decisions には固定の質問文と `none` がそのまま乗る。並べて別 API を生やすと、chat / decisions の本文組み立てと解析が 2 系統になる。

# Decision

## 1. クレートを llm-choice に改名し、問いを外から渡す

```rust
pub struct ChoiceOption { pub key: String, pub description: Option<String> }

pub struct Question {
    pub id: String,                 // decisions の questions のキー、chat の回答 JSON のキー
    pub instructions: String,       // decisions の instructions
    pub options: Vec<ChoiceOption>,
    pub none: Option<String>,       // Some(説明) なら decisions に「どれでもない」を足す
}

pub struct ChoiceRequest {
    pub subject: Vec<(String, String)>,
    pub question: Question,
    pub chat_prompt: ChatPrompt,   // chat backend だけが読む。decisions は instructions を読む
}

pub enum Answer {
    Chosen { key: String, confidence: f64, reason: String },
    NoneFits { reason: String },
}

pub trait ChoiceClassifier { fn choose(&self, &ChoiceRequest) -> … ; fn probe(…); fn reset_connections(…) }
```

`ChatClassifier` / `DecisionsClassifier` / `ConfiguredClassifier` / `HttpTransport` / `RetryPolicy` / `ApiKey` はそのまま残し、`RepoClassifier` → `ChoiceClassifier`、`ClassifyError::UnknownRepo` → `UnknownChoice` に改める。

## 2. chat の回答 JSON のキーは question の id にする

`{"<id>": string, "confidence": number, "reason": string}`。repo 分類の id は `repo` なので、Slack の `classifier_system` を上書きしている運用者のプロンプトは無改修で動く。`none` は chat では使わない（JsonSchema の `enum` は選択肢だけ。従来どおり chat は「どれでもない」を言えない）。

## 3. リポジトリ分類は llm_choice::repo のヘルパーになる

`repo::question(candidates)` が選択肢（summary + README 先頭）、固定の質問文、`none` を組み、`repo::default_prompt(subject, candidates)` が従来の組み込みプロンプトをそのまま出す。core の `repo_select` と Slack の `llm` はこれを呼ぶだけになる。**送る本文はバイト単位で従来と同じ**で、既存テストの期待値はそのまま通る。

## 4. leaf であることは変えない

arch-lint の `classifier-leaf`（ワークスペース内クレートに依存しない）と、プラグインが依存してよいクレートの一覧は、名前を llm-choice に置き換えるだけ。

# Consequences

- 返信要否判定は、question を 1 つ足すだけで chat / decisions の両方に乗る
- `ai-docs/components/repo-classifier.md` は deprecated にし、後継 `llm-choice.md` へリンクする
- 外部に公開しているクレートではないので、改名の互換層は置かない

# Alternatives considered

| 案 | 不採用の理由 |
|---|---|
| reply / no_reply を「リポジトリ」として渡す | decisions に固定文 "Which repository…" と `none` が乗り、判定精度とコードの読みやすさが落ちる |
| repo-classifier を残し汎用 API を並設 | 本文組み立て・解析・エラー整形が 2 系統になり、ADR-0083 が 1 つにまとめた理由を壊す |
| 名前は repo-classifier のまま | 返信要否の判定が repo-classifier にある、という名前と中身のずれが残る |
