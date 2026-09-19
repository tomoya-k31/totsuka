---
type: Decision
title: ADR-0083 リポジトリ分類を「分類」で切った共有クレート repo-classifier にまとめる
description: core の LlmRouter（chat_json）は通信手段で切った抽象で、プロンプト・スキーマ・回答の解釈が repo_select に埋まり、Slack プラグインは同じ仕事を別実装で持っていた。trait を「タスクと候補を渡すと検証済みの判定が返る」RepoClassifier に切り直し、HTTP・再試行・エラー整形・chat 実装を leaf クレート repo-classifier に集めて core と plugins の両方から使う決定。chat の回答契約を出力モード（JsonSchema / Prose）で表す理由、閾値と訊き直しの方針を呼び出し側に残す理由、plugin-sdk に置かない理由を記録する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/repo-classifier
tags: [decision, adr, llm, repo-select, architecture, workspace, slack, refactor]
generated: { by: claude-code/opus-5, at: 2026-09-19T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-723
    resource: https://github.com/tomoya-k31/totsuka/issues/723
    title: "decisions モデル（TypeSafe Jev）を [llm].api で選べるようにする — #723"
  - id: adr-0070
    resource: /decisions/adr-0070-llm-liveness.md
    title: "LLM ゲートウェイの死活監視 — ADR-0070"
  - id: adr-0011
    resource: /decisions/adr-0011-arch-fitness-function.md
    title: "ワークスペース依存境界の Fitness Function — ADR-0011"
---

# Status

**採択（stable）。** #723 の PR 1 本目（挙動不変のリファクタ）で実装した。decisions 方式（TypeSafe Jev）の追加は同 issue の後続 PR で、本 ADR が作った切れ目の上に載る。

# Context

リポジトリ分類（F-11〜F-14）の実装が 2 つあった。

| | core（`repo_select` + `adapters::llm`） | Slack プラグイン（`llm.rs`） |
|---|---|---|
| 抽象 | `LlmRouter::chat_json(ChatRequest{system, user, json_schema})` | `ChatTransport::complete(config, body)` |
| 出力の強制 | `response_format: json_schema`（strict） | プロンプトで JSON を頼み、前後の文から抽出 |
| 読めない回答 | `select_repo` で 1 回訊き直す | 誤答を assistant ターンで返し、訂正文を添えて 1 回 |
| HTTP・再試行・エラー整形 | 自前（指数バックオフ、`scrub_urls`） | 自前（再試行なし、`provider_message` で本文を絞る） |

`LlmRouter` は名前こそ汎用だが、実際の呼び出し元は `select_repo` だけで、**プロンプト・JSON Schema・回答の解釈はすべて `select_repo` の中**にあった。chat ではない方式（#723 の decisions モデル。`/chat/completions` を持たず、`{model, state, questions}` を送ると型付きの判定が返る）を差し込もうとすると、差し込む場所が無い。「chat で JSON を返す」という trait の形そのものが、方式を固定していた。

プラグインは arch-lint（[ADR-0011](/decisions/adr-0011-arch-fitness-function.md)）により `plugin-protocol` / `plugin-sdk` にしか依存できず、core のコードは使えない。重複はこの境界の帰結でもある。

# Decision

## 1. trait は「分類」で切る

```rust
pub trait RepoClassifier: Send + Sync {
    fn classify(&self, request: &ClassifyRequest) -> impl Future<Output = Result<Classification, ClassifyError>> + Send;
    fn probe(&self) -> impl Future<Output = Result<(), ClassifyError>> + Send { async { Ok(()) } }
    fn reset_connections(&self) {}
}
```

- `ClassifyRequest` は **何を**（`subject`: ラベル付きの本文、`candidates`: name / summary / README 先頭）だけを持つ。**どう訊くか**（プロンプト、スキーマ、ワイヤ形式）は実装の仕事になる。例外は `chat_prompt: Option<ChatPrompt>` で、Slack の設定可能テンプレートを chat 実装へ渡す口として残した（chat 以外の実装は無視する）
- `Classification` は**検証済み**: `repo` は候補のどれか、`confidence` は `[0, 1]`。全実装が同じ関数を通るので、「候補外を弾く」を呼び出し側ごとに書き忘れる余地が無い。候補外は `UnknownRepo`、読めない回答は `InvalidResponse` で、どちらも `is_bad_answer()`
- F-111 の `probe()` / `reset_connections()` は既定実装ごと引き継いだ（[ADR-0070](/decisions/adr-0070-llm-liveness.md)）

## 2. 方針は呼び出し側に残す

閾値、低確信度のときに何をするか、悪い答えを訊き直すかどうかは、**分類器ではなく呼び出し側**が決める。

- core（`select_repo`）: `[llm].confidence_threshold`（新設、既定 0.6）未満は pending。悪い答えは 1 回訊き直してから pending。通信の恒久失敗は failed
- Slack: 自分の `[llm].confidence_threshold` 未満はピッカー。訊き直しはしない（下の correction は分類器の中の話）

同じ判定に対する反応が 2 者で違うのは正しい。core のタスクは pending に置けば人が拾うが、Slack のメンションはその場でピッカーを出す。反応を分類器に押し込むと、どちらかの都合でもう一方が歪む。

## 3. chat の回答契約は出力モードで表す

2 つの実装の違いは実装の癖ではなく**回答契約の違い**なので、`ChatOutput` として型にした。

| `ChatOutput` | 送るもの | 読み方 | 使う側 |
|---|---|---|---|
| `JsonSchema` | `response_format: json_schema`（strict） | 本文 = 判定オブジェクト。`reason` 必須 | core |
| `Prose` | `response_format` なし | 前後の文・コードフェンスから抽出。`reason` 任意 | Slack（`json_schema` を受け付けないモデルがある） |

`ChatPrompt.correction` があれば、読めない回答を**分類器の中で 1 回だけ**訂正付きで再試行する（Slack の従来挙動）。無ければ `InvalidResponse` を返して再試行は呼び出し側に任せる（core の従来挙動）。**correction が効くのは読めない回答だけ**で、読めたが候補外の回答は再試行しない —— これも Slack の従来挙動どおり。

## 4. 置き場所は新しい leaf クレート

`crates/repo-classifier` を新設し、core と `plugins/*` の両方の許可リストに入れた。ワークスペース内のどのクレートにも依存しない（arch-lint の `classifier-leaf`）。

| 案 | 評価 |
|---|---|
| **新しい leaf クレート（採用）** | 責務がそのまま名前になる。core と plugins の双方から使え、どちらにも寄らない |
| `plugin-sdk` に置く | 許可リストは変えずに済むが、core → plugin-sdk という新しいエッジができ、「task_source プラグインを書くための SDK」の責務がぼやける。reqwest を使わないプラグインにも HTTP クライアントが入る |
| `plugin-protocol` に置く | 論外。protocol はプラグイン境界の公開型であって、外部 API の叩き方ではない |
| 共通化しない | decisions 方式を足すと実装が 4 つになる |

## 5. トランスポートの seam は HTTP の 1 往復

テスト用の差し替え口は `HttpTransport::post_json(HttpRequest{url, api_key, body, timeout})` にした。

- **API キーはリクエストに載せる。** Slack は `initialize` で初めてキーを知る（orchestrator の `[llm]` を引き継ぐことがある）ので、トランスポート（= 接続プール）の生成時にはキーが無い
- Slack の既存の結合テストは「chat-completions の JSON を返す偽物」を差し込んでいた。seam を HTTP の高さに置いたので、それらは型名の置き換えだけで無改造のまま通る。seam を `RepoClassifier` の高さに置くと、プロンプトや抽出の検査がテストから消えていた

# Consequences

- エラー本文は、標準の error envelope なら常に `error.message` だけに絞る（従来は Slack だけ。core は生の本文 500 文字）。401 の本文に資格情報を echo するゲートウェイがあったとき、core のログにも載らなくなった。envelope でない本文は従来どおり 500 文字まで残す（Slack は 300 → 500）
- Slack の分類は信頼度の範囲検査（`[0, 1]`）が加わった。従来は 1.5 のような値も閾値を通過していた
- `[llm].max_tokens` を省略したときは送らない、という実際の挙動に合わせて設定リファレンスの既定値表記を直した（従来「256」と書いていたが、`SelectConfig::default()` の 256 はテストでしか使われていなかった）
- trait と型は `repo-classifier` にあり、core の `ports::llm` はそれを再輸出するだけになった。ports が外部クレートの trait を指すのは初めてだが、ヘキサゴナルの意味（domain は trait 越しにしか外界を見ない）は変わらない
