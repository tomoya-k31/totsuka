---
type: Decision
title: ADR-0009 TOTSUKA_* 環境変数オーバーライドはホワイトリスト方式で配線する
description: F-66 第 2 層（TOTSUKA_* 環境変数）の配線にあたり、汎用 TOML オーバーレイではなく明示的なキー対応表（ホワイトリスト）を採用し、不正値は起動エラー・未知キーは警告とする fail-loud 方針を採る決定。フラット文字列 map の ConfigResolver は削除し、RootConfig へ直接適用する型付き関数に置き換える。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/config/env_overrides.rs
tags: [config, environment, precedence, f-66, fail-loud]
timestamp: 2026-07-22T12:00:00Z
status: accepted
owner: tomoya-k31
---

# Status

Accepted — 2026-07-22（[#208](https://github.com/tomoya-k31/totsuka/issues/208)）

# Context

[Spec](/product/orchestrator-spec.md) の F-66 は設定の優先順位を「CLI フラグ > `TOTSUKA_*` 環境変数 > `plugins/{name}.toml` > `config.toml`」と規定している。しかし実装では第 2 層が**どこからも呼ばれていなかった**。

`crates/orchestrator-core/src/config/layered.rs` に `ConfigResolver`（4 層の `HashMap<String, String>`）と `env_layer_from`（`TOTSUKA_` を剥がして小文字化）が単体テスト付きで存在したが、リポジトリ全体を検索しても構築箇所は 1 つもなく、`TOTSUKA_MAX_CONCURRENCY=5 totsuka run` は**エラーにもならず黙って無視**されていた。CI・コンテナ実行という、この層が最も効いてほしい場面で気づけない。

これは単なる呼び忘れではなく、`ConfigResolver` の形が設計として接続不能だったことが原因である。

1. **型強制がない** — 値は文字列のまま。`"5"` → `u32` の変換は呼び出し側任せで、その実装が存在しない
2. **ネストキーの規約がない** — `max_concurrency` の `_` が語区切りなのか階層区切りなのか判別できず、`log.level` を表現できない
3. **適用先がない** — `RootConfig` は `deny_unknown_fields` 付きの型付き構造体であり、フラット文字列 map から流し込む経路がない
4. **上位 2 層が実態と乖離** — cli 層の実態は `--debug` 1 フィールドの特例、plugin_file 層の実態は plugin へ渡す opaque な JSON パススルー（§4.6）であり、どちらも key/value map ではない

つまり「フラットな文字列 map を 4 層重ねる」というモデル自体が、この設定モデルに合っていなかった。

# Decision

## 1. フラット文字列 map を捨て、型付きオーバーレイに置き換える

`layered.rs` を削除し、`config/env_overrides.rs` を新設する。マージ器は作らず、**適用順で優先順位を実現する**:

```
config.toml パース (RootConfig::from_toml_str)
        ↓
apply_env_overrides(&mut cfg, env)      ← 第 2 層
        ↓
CLI フラグ適用（--debug）                ← env より後 = CLI が勝つ
```

第 3 層（`plugins/{name}.toml`）は §4.6 の二層モデルどおり plugin へ opaque にパススルーされる領域で、Orchestrator は解釈しない。したがってこの経路には現れない（env での上書きはプラグイン側の責務）。

## 2. 適用範囲はホワイトリスト（汎用 TOML オーバーレイは不採用）

環境変数名 → 適用先フィールドの静的な対応表を持ち、そこにある変数だけを解釈する。対象は「CI・コンテナ実行でファイルを書き換えずに差し替えたい、Orchestrator が解釈するスカラ」に限る（[設定例集](/development/config-examples.md) に一覧）。

代替案の**汎用 TOML オーバーレイ**（`TOTSUKA_LOG__LEVEL` のような区切り規約で任意のキーへ流し込む）は採らなかった:

- 区切り規約を導入しても `RootConfig` が `deny_unknown_fields` である以上、キー名を間違えれば結局エラーになる。「任意のキーを触れる」自由度は、型付きスキーマに対しては幻でしかない
- 配列・動的キー（`[[repositories]]`、`[[workflows]]`、`[plugins.{name}]`）はインデックスや名前を含む規約が必要になり、環境変数名として実用に耐えない
- 明示表なら `TOTSUKA_LOG_PROMPTS` → `log.log_prompts` のような命名の調整（`LOG_LOG_` の重複回避）が自由にできる

配列・動的キーへのオーバーライドは**スコープ外**とする。必要になったら別 issue で扱う。

## 3. Fail-loud（黙って無視しない）

本 issue の本質は「効かないこと」ではなく「**効かないのに何も言わないこと**」なので、扱いは沈黙を避ける方向に倒す。

| ケース | 挙動 |
|---|---|
| ホワイトリスト対象の値が型変換・検証に失敗（`TOTSUKA_MAX_CONCURRENCY=abc`） | **起動エラー**（変数名・値・期待型をメッセージに含める） |
| `TOTSUKA_LLM_*` を設定 + `config.toml` に `[llm]` 不在 | **起動エラー**（`base_url`/`model` が必須のため env だけからテーブルを合成しない） |
| 未知の `TOTSUKA_*`（typo した `TOTSUKA_MAX_CONCURENCY` 等） | **警告**（stderr）。起動は継続 |
| 予約名（注入系。下記） | 無視（警告も出さない） |
| 対象キーの値が空文字列 | **警告 + 未設定扱い**（env の「空 = unset」慣習に合わせる） |

未知キーを警告どまりにするのは、`TOTSUKA_` 接頭辞の変数が他用途で存在しうるためである（下記の予約名がまさにそれ）。typo は見えるが致命ではない、という中間を選んだ。

## 4. 注入系（outbound）env は予約名として除外する

Orchestrator がエージェント/フックプロセスへ**注入する** env（`TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` / `TOTSUKA_HOOK_SPOOL_DIR` / `TOTSUKA_PROMPT_CONTEXT`。[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)、`HookLaunchSpec`）は設定オーバーライドとは**逆向きの別系統**であり、未知キー警告から除外する。エージェントセッションがこれらを export した状態で `totsuka` CLI を再実行しうるため、除外しないと警告が常時出る。

注意: 予約名 `TOTSUKA_HOOK_SPOOL_DIR`（単数 `HOOK_`、注入）と新設 `TOTSUKA_HOOKS_SPOOL_DIR`（複数 `HOOKS_`、`[hooks]` テーブルの上書き）は 1 字違いである。

## 5. 適用は CLI の設定ロード経路で一元化する

`Cx::load_config` が env スナップショットを受け取り、そこで適用する（呼び出し元は `run` / `config` / `focus` / `doctor`）。`run` だけに適用しないのは、`totsuka focus` / `doctor` が `[hooks].socket_path` から `run` がバインドしたソケットを解決するためで、片方だけに効かせると**別のソケットを見る**不整合が生じる。

例外は `plugin_cmd.rs` のローカルローダ（`plugin enable`/`disable` の**ファイル編集用**）で、env を見せると編集結果が env 値で汚染されるため raw のまま維持する。

# Consequences

- これまで黙って無視されていた `TOTSUKA_*` を偶然設定していた環境では、**新たに値が効き始める / 不正値が起動エラーになる**。これは本 issue の意図した挙動変更であり、後方互換性の観点では破壊的になりうる
- 対応キーを増やすには表への追記が必要になる（汎用オーバーレイなら不要だった）。これは意図的なコスト — 何が上書きできるかがコードとドキュメントの両方で有限に列挙される
- `ConfigResolver` / `env_layer_from` は削除された。優先順位を固定する意図は `env_overrides.rs` のテストが引き継いでいる
- **既知の非対称**: `config.toml` 側の `log.level` 不正値は現状 silent fallback のまま（`run_cmd.rs` の `.and_then(parse_level)`）。env 経由は本 ADR でエラーになるため、同じ typo でも経路によって挙動が違う。ファイル経路の改善は別途
- `totsuka config show` は引き続き**ファイルの内容**を表示するが、有効な env オーバーライドがある場合は末尾に一覧を追記する。表示しないと「ファイルが全て」と誤読させ、本 issue と同種の沈黙を再生産するため

# Citations

[1] [Issue #208](https://github.com/tomoya-k31/totsuka/issues/208)
[2] [Spec](/product/orchestrator-spec.md) F-66（設定の優先順位）、§4.6（二層設定モデル）
[3] [設定例集](/development/config-examples.md) — 対応環境変数の一覧と使用例
[4] [設定リファレンス](/development/config-reference.md)
[5] [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) — 注入系 env（`HookLaunchSpec`）の出自
