#!/usr/bin/env bash
# dev-test.sh — ローカルのテスト実行（依存: cargo, cargo-nextest）
#
# ワークスペースをビルドしてから nextest でテストを回し、最後に doctest を回す。
# 3 コマンドをスクリプトにしてあるのは、順序と環境変数に**壊れ方の分かっている
# 前提**があるからで、絞り込みや設定のためではない:
#
#   1. TEST_SUPPORT_PREBUILT_BINS は「直前にワークスペース全体をビルドした」
#      ことが前提。nextest はテストごとにプロセスを分けるので、立て忘れると
#      ネストした cargo build が**テストの数だけ**走る（ADR-0018 §3）。
#      逆に、ビルドせずに立てると古いバイナリを検証してしまう。
#   2. nextest は doctest を実行できない（上流の制限）。libtest で別に回さないと
#      黙って検査対象から消える。
#
# 使い方:
#   scripts/dev-test.sh                       # フルラン（PR 直前はこれ）
#   scripts/dev-test.sh -E 'package(=agent-ide-herdr)'   # 追加引数は nextest へそのまま渡る
#
# 引数はすべて `cargo nextest run` に転送する。絞り込みが要るときは nextest の
# filterset をそのまま使う（このスクリプト独自のオプションは持たない）。
#
# 守備範囲は**テストだけ**。fmt / clippy / arch-lint / cargo doc は対象外で、
# 従来どおり手で回す（反復中は rust-analyzer の診断が代替になる）。
#
# 終了コード: 0 成功 / それ以外は失敗したコマンドのもの
set -euo pipefail

command -v cargo >/dev/null 2>&1 || {
  echo "dev-test: cargo が必要です" >&2
  exit 2
}
cargo nextest --version >/dev/null 2>&1 || {
  echo "dev-test: cargo-nextest が必要です (cargo install cargo-nextest --locked)" >&2
  exit 2
}

cd "$(dirname "$0")/.."

# --all-features は付けない（このワークスペースは [features] 宣言ゼロで no-op。
# ADR-0029）。RUSTFLAGS="-D warnings" も付けない — root Cargo.toml の
# [workspace.lints.rust] warnings = "deny" に全 11 メンバーが opt-in 済みで、
# tests/ ターゲットにも効くことを実測済み。付けるとフィンガープリントだけが
# 変わり、clippy / cargo doc / rust-analyzer と別のアーティファクト空間を
# 作ってビルドを二重に払う（ADR-0049）。
echo "==> cargo build --workspace --all-targets"
cargo build --workspace --all-targets

echo
echo "==> cargo nextest run --workspace $*"
TEST_SUPPORT_PREBUILT_BINS=1 cargo nextest run --workspace "$@"

echo
echo "==> cargo test --doc --workspace"
cargo test --doc --workspace

echo
echo "dev-test: 完了（fmt / clippy / arch-lint / cargo doc はこのスクリプトの守備範囲外）"
