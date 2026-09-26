---
type: Decision
title: ADR-0102 orchestrator-core の domain を config から切り離し、層の向きを arch-lint で検査する
description: "domain/workflow.rs が config のスキーマ型を import していた逆向きの依存を解消し、値型を domain に、config.toml からの変換を config::interpret に置いた決定。serde の derive は domain に残し、CleanupPolicyConfig は config に残す。domain / ports が config・adapters を参照しないことを arch-lint の core-layer で検査し、3 層の外のモジュールは移さず doc で位置づける。"
resource: https://github.com/tomoya-k31/totsuka/issues/762
tags: [decision, core, architecture, hexagonal, fitness-function, config, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T21:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

Accepted（2026-09-26、#762）

# Context

`orchestrator-core` は「domain = 純粋なドメイン型とステートマシン」と宣言していたが、`domain/workflow.rs` が `config` の型（`WorkflowConfig`・`ProjectConfig`・`Profile`・`WorkflowMode`・`OutputPolicy`・`VerificationMode`・`CleanupPolicyConfig`）を import していた。設定ファイルのスキーマを変えると domain へ波及する向きである。domain の他の 2 モジュール（`state`・`signal`）は std しか使っておらず、違反はこの 1 モジュールに閉じていた。

`scripts/arch-lint.sh` はクレート**間**の依存しか検査しない（[workspace-dependency-rules](/architecture/workspace-dependency-rules.md)）。クレート内の向きには後戻りを止めるものが無かった。

# Decision

1. **値型は domain に置く。** `Profile`・`WorkflowMode`・`OutputPolicy`・`VerificationMode` と、解釈済みの `CleanupPolicy`（旧 `worktree`）を `domain::workflow` に移した。一時的な再公開（`pub use`）は同じ PR の中で外し、呼び出し元はすべて `domain` から import する。
2. **serde の `Deserialize` は付けたまま移す。** serde は設定ファイルではなく汎用のシリアライズ層で、外から見える形は変種名だけである。config 側に写しを持つと変種を 2 か所に書くことになり、変種を足したときにずれる。この 4 つは中身の無い変種だけの enum なので、`config-template-lint` の入力（`ident:` の行）にも影響しない。
3. **`CleanupPolicyConfig` は config に残す。** `untagged` と `keep_7d` / `keep_28d` の糖衣は TOML の書き方そのものである。また struct 変種 `{ retention_days }` が雛形のキーとして数えられているので、動かすと template-lint の入力が変わる。変換は `From<CleanupPolicyConfig> for CleanupPolicy` にし、`keep_*` の展開を解釈時の 1 回にした（#210 の「`keep_*` を知っているのは config の解釈だけ」を 1 か所に保つ）。
4. **変換は `config::interpret` に置く。** `Workflow::from_config(s)` を `RootConfig::domain_workflows` に置き換え、`OutcomeAction::from_table` と `OUTCOME_ACTION_KEYS` も同じモジュールへ移した。「profile を解決する唯一の場所」（#394、[ADR-0033](/decisions/adr-0033-workflow-profile.md)）・「source を導出する唯一の場所」（#626、[ADR-0069](/decisions/adr-0069-workflow-projects.md)）・「`initial_prompt` を正規化する唯一の場所」（#415、[ADR-0038](/decisions/adr-0038-workflow-initial-prompt.md)）・「`status` キーを読む唯一の場所」（#574/#626）は、置き場所が変わっただけで 1 か所のままである。
5. **`arch-lint` に `core-layer` を足す。** `domain/**` と `ports/**` が `config` / `adapters` を参照しないことをテキストで検査する。各行のパスと、`;` までまとめた `use` 文の両方を読む。コメント行と `#[cfg(test)]` 以降は数えない。
6. **3 層の外のモジュールは動かさない。** doc のほうを実際の構成（3 層・アプリケーション層・基盤）に合わせる。

# Alternatives considered

- **組み立て側（`run`）に変換を置く**: 呼び出し元の 1 つが `config::validate` なので、config → run という新しい逆向きの依存ができる。却下。
- **変換を `(&[WorkflowConfig], &[ProjectConfig])` の自由関数にする**: 呼び出し元 19 か所（本番 3 + 統合テスト 16）は全員同じ `RootConfig` から両方を渡していた。引数を 1 つにすれば、別の config の projects と取り違える余地がなくなる。`RootConfig` のメソッドにした。
- **型 1 つごとに PR を分ける（expand → migrate → contract を PR 単位で）**: `pub use` を残した中間状態が main に並ぶだけになる。1 PR の中でコミットを分ければ同じ安全性が得られる。
- **3 層の外のモジュールをディレクトリごと層へ移す**: `orchestrator-cli` まで含めてパスを大量に書き換えることになるが、振る舞いも検査も良くならない。
- **`domain::Trigger` の `toml::Table` を `serde_json::Map` 等に持ち替える**: trigger はプラグインへそのまま渡す値で、持ち替えると `initialize` に渡す JSON の形が変わるおそれがある。「振る舞いを変えない」という前提に反するので、今回は扱わない。
- **Rust のテストや独自 lint で層を検査する**: Rust は自分の import を列挙できず、既存の fitness function はすべて bash + awk である。

# Consequences

- config のキーを足したり直したりしても、domain のコードとテストは変わらない。変更は `config::schema` と `config::interpret` で止まる。
- `core-layer` は移行前の `main` に対して走らせると `domain/workflow.rs` の `use crate::config::{…}` で exit 1 になり、移行後は 0 error になることを確認した。
- `use` 文は複数行にわたっても `;` まで 1 つにまとめて読むので、`use crate::{config};` や、グループに `as` で別名を付けて `config::` と一度も書かない形も捕まる（#802 のレビューで補強）。代わりに、外部クレートの `…::config` を use すると誤検知になる。今の domain / ports にその形は無い。
- アプリケーション層（`run` など）どうしの向きや、アプリケーション層 → adapters の依存は検査しない。後者は正当な向きである（`recovery` → state DB、`plugins::spec` → `PluginSpec`）。
