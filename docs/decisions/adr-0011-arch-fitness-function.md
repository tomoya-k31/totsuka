---
type: Decision
title: ADR-0011 アーキテクチャ Fitness Function は cargo metadata + 自前スクリプトで CI 検証する
description: ワークスペースの依存境界不変条件（plugins → plugin-protocol / plugin-sdk のみ、plugin-protocol は leaf、依存循環なし）を cargo metadata --no-deps ベースの自前スクリプト scripts/arch-lint.sh で機械検証し、ci.yml の clippy ジョブ内 step として毎 PR 実行する決定。cargo-deny bans / cargo-modules は不採用。許可リストには issue 起票後に追加された plugin-sdk を含める。
tags: [architecture, fitness-function, ci, workspace, dependency]
timestamp: 2026-07-23T12:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-23（[#172](https://github.com/tomoya-k31/totsuka/issues/172)）

# Context

totsuka のヘキサゴナル構成では「プラグインはプロトコル層のみに依存し orchestrator-core の内部に触れない」「plugin-protocol は実装クレートに依存しない leaf」という依存境界が設計の要だが、これは規約と PR レビューの目視でしか守られておらず、CI に自動ガードがなかった。誤って `plugins/*` に `orchestrator-core` 依存を足しても CI は green のまま通過する。依存循環の検出手段も未導入だった（[#172](https://github.com/tomoya-k31/totsuka/issues/172)）。

なお issue 起票時の依存グラフには存在しなかった `plugin-sdk` がその後追加され、`task-source-*` 3 プラグインが依存している。issue の字義（「plugins は plugin-protocol のみ」）をそのまま強制すると現状のコードベースで即 CI が赤になるため、許可リストの範囲を実態に合わせて確定する必要があった。

実装手段の候補は次の 3 つ（issue 記載）:

1. `cargo metadata` を jq / 小スクリプトで検査 — 追加ツール不要、ルールを自由に書ける
2. `cargo-deny` の bans 機能 — ただし bans のモデルは「特定クレートの全面禁止 / 重複検出」であり、「このクレート群に限りワークスペース内依存はこの集合のみ」という**クレート群ごとの許可リスト**は素直に表現できない
3. `cargo-modules` / `cargo-depgraph` ベース — モジュール粒度まで見えるが追加ツールのインストールが必要で、必要なのはクレート粒度のみ

# Decision

**`cargo metadata --no-deps` + 自前スクリプト（`scripts/arch-lint.sh`、bash + jq + awk）を採用する**（候補 1）。検証ルールと更新手順は [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md) に集約する。

- **許可リスト**: plugins → `plugin-protocol` / `plugin-sdk`（dev は + `test-support`）。issue の字義から拡張し、SDK 経由の抜け穴を塞ぐため **`plugin-sdk` 自体にも protocol-only 制約を課す**。`plugin-protocol` は全種別でワークスペース内依存ゼロ（leaf）。
- **循環検査**: normal + build + dev の全エッジを Kahn 法で検査。dev だけの循環は cargo 的に合法だが意図しない結合とみなしエラーにする。循環メンバーの特定は順方向・逆方向の 2 回 peel の交差で行う。
- **CI 配置**: 新規ジョブではなく **`ci.yml` の `clippy / rustfmt` ジョブ内の step** として実行する。[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) の「1 分切り上げ課金のためジョブ追加より既存ジョブへのステップ追加を優先」に従う。`cargo metadata --no-deps` は依存解決もネットワークも不要で数秒のため、重い clippy より前に置き fail-fast させる。
- **plugins/* の判定はクレート名列挙ではなく manifest パス**（`plugins/` 配下）で行い、新プラグイン追加時のスクリプト更新を不要にする。
- **受け入れ検証はローカル違反注入で実施**: 検証用の使い捨て PR は作らず、3 種の違反（plugin → core / protocol → test-support / core ⇄ cli 循環）を一時注入してスクリプトが exit 1 することを確認した。CI は同一スクリプトを呼ぶだけなので実質同等とみなす。あわせてローカル pre-PR チェック（`.claude/rules/dev-flow.md` の Rust set）にも組み込む。

# Consequences

- 依存境界違反・循環は PR の CI（`clippy / rustfmt` ジョブ）で自動検出される。main への push では実行されないが、main へのコード流入は PR 経由のみのため検知網としては十分。
- 正当な依存追加時はスクリプト冒頭の許可リストと [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md) を同一 PR で更新する必要がある（手順は同ドキュメント）。
- cargo-deny bans を使わないため、依存境界ルールが `deny.toml` ではなくスクリプト内の変数として宣言される。ルールの表現力と引き換えに、検証ロジック自体の保守は自前になる。
- jq が前提ツールに加わる（GitHub ホストランナーにはプリインストール済み。ローカルに無い場合は明確なメッセージで exit 2）。

# Citations

[1] [Issue #172](https://github.com/tomoya-k31/totsuka/issues/172)
[2] [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md)
[3] [ADR-0007 CI 実行タイミングの再設計（Actions コスト最適化）](/decisions/adr-0007-ci-cost-optimization.md)
[4] [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
