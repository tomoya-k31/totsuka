# Decisions (ADR)

アーキテクチャ上の意思決定記録。1決定=1ファイル、`adr-NNNN-<slug>.md` 形式。

* [ADR-0001 OKFによるドキュメント管理の採用](adr-0001-adopt-okf.md) - リポジトリ内ドキュメントをOKF v0.1準拠のKnowledge Bundleとして管理する決定。
* [ADR-0002 Rust workspace 構成と CI 品質ゲート](adr-0002-rust-workspace-ci.md) - totsuka を Rust edition 2024 のヘキサゴナル workspace（core/cli/plugin-protocol）として構成し、clippy deny warnings・rustfmt・cargo-audit/deny・llvm-cov を CI 品質ゲートに据える決定。
