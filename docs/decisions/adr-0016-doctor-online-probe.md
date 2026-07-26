---
type: Decision
title: ADR-0016 鍵の「解決できる」と「使える」を分け、後者は doctor --online のオプトインで検査する
description: doctor が api_key_ref の解決可否しか見ず無効な LLM キーを実行時まで露見させなかった問題（#267）に対し、既定のオフライン・非対話（ADR-0006）は一切変えずオプトインの --online で 1 回の最小ライブリクエストを投げる決定。プローブは json_schema なし・リトライなし・max_tokens 1・本文破棄で、401/403 のみ fail・その他の失敗は warning に留める。実行時側は HTTP ステータスを型として持ち回り 401/403 だけ warn! へ上げる。GET /models 方式・doctor の既定オンライン化・連続失敗カウンタは不採用。
resource: https://github.com/tomoya-k31/totsuka/issues/267
tags: [doctor, llm, secret, probe, observability, cli]
timestamp: 2026-07-26T00:00:00Z
status: accepted
owner: tomoya-k31
---

# Status

Accepted — 2026-07-26（[#267](https://github.com/tomoya-k31/totsuka/issues/267)）

[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) の「doctor は非対話を保つ」方針を**否定せず補完する**。既定の `doctor` の挙動は本 ADR で一切変わらない。

# Context

`doctor` の `check_llm_key` は **シークレット参照が解決できるか**しか見ていなかった。`op://` 参照に至っては解決すら行わない（`op read` が生体認証プロンプトを出しうるため。ADR-0006 の非対話原則として正しい）。

結果、実機で OpenRouter が全リクエストに HTTP 401（`User not found.`）を返し続けている状態で、`doctor` は次のように報告していた。

```
ok:   llm — api_key_ref is an op:// reference (checked by the 1password probes, not resolved here)
```

一方 run のログでは毎メンション:

```
INFO task_source_slack::repo_resolver: LLM classification inconclusive; asking the operator
  error=LLM request failed: HTTP 401: {"error":{"message":"User not found.","code":401}}
```

**縮退そのものは設計どおり**で安全である（[task-source-slack](/components/task-source-slack.md) の 3 段階解決は、分類できなければ必ず operator の picker へ落ちる）。問題は縮退が正しいことではなく、**設定不備が「少し不便な正常動作」と見分けがつかない**ことにある。候補が 2 件以上ある構成では新規会話のたびに picker が出て、そのたびに 401 が返るまでの往復コストを払う。実際、実機検証中に偶然ログを読むまで誰も気づかなかった。

構造として欠けているのは 2 つ:

1. **実行時**: 分類失敗が一律 `info!` の「inconclusive」だった。しかし 401/403 は「判断がつかなかった」ではなく「設定が壊れている」であり、同じ棚に置くべきではない。エラーが `Err(String)` に平坦化されていたためステータスによる分岐そのものが不可能だった。
2. **事前**: 「参照が解決できる」と「その鍵が API に受理される」を確かめる手段がどこにも無い。前者は後者を一切含意しない。

# Decision

## 1. HTTP ステータスを型として持ち回り、認証拒否だけログレベルを上げる

`task-source-slack` の `ChatTransport` のエラーを `String` から `ChatError { status: Option<u16>, message: String }` に変える。`status` が `None` なのはトランスポート層の失敗（DNS・接続拒否・タイムアウト・本文が JSON でない）で、これらは資格情報について何も語らない。`ChatError::is_auth_failure()` は `Some(401 | 403)` のときだけ true。

`repo_resolver` の縮退経路を分岐し、認証拒否のときだけ `warn!` へ上げて actionable な文言にする:

```
WARN the LLM provider rejected the API key; repository selection falls back to
     the operator picker for every new conversation until it is fixed — check
     [llm].api_key_ref and run `totsuka doctor --online`
```

**振る舞いは変えない**。認証拒否も従来どおり `Resolution::NeedsSelection` へ縮退する（回帰テストで固定）。変えるのは「運用中に気づけるか」だけである。

429 / 5xx を認証拒否に含めないのは、これらが「プロバイダが混んでいる・壊れている」であって鍵の問題ではないため。ここを広げると、プロバイダの一時的な不調のたびに「鍵を確認せよ」と言うオオカミ少年になる。

## 2. `doctor --online` — オプトインのライブプローブ

既定の `doctor` は**完全にこれまでどおり**（オフライン・非対話・`op://` を解決しない）。`--online` を明示したときだけ、`[llm]` に対して 1 回の最小リクエストを投げる。

プローブは [`LlmRouter`](/components/orchestrator-core.md) の本経路を**通さない**専用メソッド `OpenAiRouter::probe_auth` として実装する。理由はすべて「鍵が受理されるかだけを訊く」ための切り分け:

| 選択 | 理由 |
|---|---|
| `response_format` の json_schema を送らない | 構造化出力の受理形はプロバイダごとに違う。拒否された schema（400）が「鍵が悪い」に化ける |
| リトライしない（`max_retries = 0`） | プローブは今答えるか答えないかのどちらか。5xx をリトライすると不調なプロバイダ相手に `doctor` が固まる |
| `max_tokens: 1` | 健全なプロバイダでの課金を丸め誤差にする |
| レスポンス本文を破棄 | 2xx が返った時点で「鍵は受理された」という問い自体には答えが出ている |

severity の対応は **401/403 だけが fail**、それ以外の失敗（タイムアウト・トランスポート・429・5xx）は `Check::warn`。ネットワークの不調で `doctor` が exit 3 になると、赤信号が「設定が壊れている」の意味を失う。

チェック名はオフラインの `llm` とは別の **`llm-online`** にする（`--json` の消費側が両者を区別できる）。オフラインの解決に失敗している場合はプローブを行わない — 投げる鍵が無く、同じ失敗を言い直すだけになる。

`--online` は `op://` 参照を**実際に解決する**。ADR-0006 が既定で避けている生体認証プロンプトは、このフラグが明示的に買うコストである（`--help` にも明記）。

フラグ名を `--check-llm` のような対象特化にしなかったのは、「ネットワークに出る／プロンプトが出うるプローブ群」という区分の方が旗として長持ちするため。現時点で `--online` が実行するのは `[llm]` のプローブのみ。

## 3. エラー本文は「プロバイダの一文」に絞る

上の 2 つはどちらも**プロバイダのレスポンス本文を人間に見せる**。見せなければ 401 の原因（`User not found.` なのか `insufficient_quota` なのか）が分からないので見せること自体は必要だが、本文をそのまま運ぶと**ゲートウェイが弾いた資格情報を本文にエコーバックしてきた場合にそれごと露出する**。しかも露出先はどちらも保護が効かない:

- プラグイン側のログは `tracing_subscriber::fmt()` そのままで、`orchestrator_core::logging` の redaction 層は**ワークスペース境界（[ADR-0011](/decisions/adr-0011-arch-fitness-function.md)）によりプラグインから参照できない**
- `doctor` のチェック結果は tracing ではなく **stdout へ `println!`** されるため、そもそも redaction 層を通らない（しかも `--json` の出力は不具合報告に添付される運用）

そこで両方とも、本文が OpenAI 互換のエラーエンベロープ（`{"error":{"message": …}}`）として解釈できるときは **`error.message` だけを残す**。解釈できない形（HTML のエラーページ、プロキシ独自の形式、素のテキスト）は従来どおり切り詰めた生本文にフォールバックする — **見慣れない形が返ってきたときこそ運用者が中身を見る必要がある**ため、ここで黙らせるのは逆効果。

完全な防御ではない（非 JSON 本文に鍵が載っていれば通る）が、最も起きやすい経路を塞ぎつつ診断能力を落とさない妥協点として採る。redaction 層をプラグインから使えるようにする案は、アーキテクチャ境界を崩すコストが釣り合わないため採らない。

# Alternatives considered

- **`doctor` を既定でオンライン化する** — 不採用。`doctor` は CI や unattended でも走る。ネットワーク到達性と 1Password セッションを前提にした瞬間、環境診断のためのコマンドが環境を選ぶようになる。ADR-0006 の非対話原則はここで守る価値がある。
- **`GET {base_url}/models` で認証だけ確かめる** — 不採用。トークンを消費しない点は魅力だが、OpenAI 互換を名乗るゲートウェイが `/models` を実装している保証がなく（未実装なら 404 で判定不能）、何より **run が実際に叩く経路と違う**。プローブが通ったのに本番が落ちる形は、プローブが無いより悪い。
- **連続失敗をカウントして notifier に流す（issue の案 3）** — 今回は不採用。プラグイン側に失効検知の状態機械を持ち込む実装コストに対し、案 1（運用中に気づける）+ 案 2（事前に気づける）で今回の穴は塞がる。鍵が**運用の途中で**失効するケースの検知として価値は残るので、必要になった時点で別途起票する。
- **エラー文字列を正規表現で見て 401 を拾う** — 不採用。`Err(String)` のまま `contains("HTTP 401")` する案は最小差分だが、プロバイダの文言に依存する検査を増やすことになる。ステータスは元々型のある情報であり、平坦化した場所を戻すのが筋。

# Consequences

- `totsuka doctor --online` は **ネットワークに出て、わずかに課金され、`op://` 構成では生体認証プロンプトを出しうる**。CI や cron からは使わない。
- `doctor`（フラグなし）の出力・exit code・非対話性は不変。既存のスクリプトへの影響は無い。
- `ChatTransport::complete` のエラー型が変わるため、`task-source-slack` のテストダブルは `ChatError` を返すよう追随が必要（プラグイン内に閉じた seam なので外部影響なし）。
- 「参照が解決できる」と「鍵が使える」が別チェックとして分かれたことで、以後 `[llm]` 以外の外部依存にも同じ 2 段構え（オフライン検査 + `--online` プローブ）を追加できる。

# Citations

1. [ADR-0006 シークレット参照に 1Password (op://) を第 2 バックエンドとして追加する](/decisions/adr-0006-onepassword-secret-backend.md) — doctor の非対話原則
2. [ADR-0012 CLI の exit code 体系と --json エラーエンベロープ](/decisions/adr-0012-cli-exit-codes-json-errors.md) — exit 3（問題検出）の意味
3. [運用ガイド（doctor / worktree 掃除 / FAQ）](/operations/operations-guide.md) — doctor の読み方
