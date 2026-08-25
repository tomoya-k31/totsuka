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
# テストが通ったあとに target/ を掃除する（ADR-0060）。放っておくと `.o` が
# 際限なく溜まってビルドが桁で遅くなるため。詳細は下の「target/ の掃除」を参照。
#   - 毎回: target/debug/deps の .o を掃く（0.53 秒・キャッシュ損失ゼロ）
#   - 定期: cargo clean（既定 7 日ごと、または incremental が 300 セッション超）
# 掃除ごと止めたいときは DEV_TEST_SKIP_TARGET_CLEANUP=1、
# clean の間隔だけ変えたいときは DEV_TEST_CLEAN_MAX_AGE_DAYS=<日数>。
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

# ---------------------------------------------------------------------------
# target/ の掃除（ADR-0060）
#
# macOS の cargo は dev プロファイルで split-debuginfo = "unpacked" が既定なので、
# ワークスペースの .o が target/debug/deps に**残り続ける**。cargo はこれを GC
# しないので放っておくと際限なく増える（実測: 1 か月で 839,365 個 / 15G）。
#
# エントリ数はそのままビルド時間に効く。同一ワークスペース・同一の変更（葉クレート
# を touch した warm 再ビルド）で、deps のエントリ数**だけ**を変えた対照実験:
#
#     53,733 エントリ →   6.6s /  7.2s
#     89,029 エントリ →  11.5s / 12.4s
#    250,029 エントリ →  20.7s / 23.7s
#    852,733 エントリ →  39.9s / 126.3s
#
# そこで毎回掃く。**.o はビルドの入力ではない**ので、これでキャッシュは 1 バイトも
# 失われない（実測: 250,001 → 664 エントリまで消した直後の
# `cargo build --workspace --all-targets` が 0.33 秒の no-op）。フル再ビルド 1 回分
# の .o は約 8,900 個 / 1.2G で、掃除は 0.53 秒。
#
# 対象は target/debug/deps だけ。target/release/deps は実測で 610M 止まりで、
# 問題として観測されていないので触らない。
#
# 失うものは 1 つだけで、実測で特定してある: RUST_BACKTRACE のスタックフレームから
# 自前クレートの `at <file>:<line>` が落ちる（関数名は残る）。パニック位置の
# `panicked at crates/…/foo.rs:434:5` は file!() 由来なので**影響を受けない**。
# その crate を次にビルドし直した時点で戻る。
#
# 掃除はテストのあとに走る。ビルドかテストが落ちた回は set -e でここまで来ないので、
# デバッグ中の回は debuginfo が残る。
#
# incremental/ は .o と違って**ビルドの入力**なので、掃くとその場で損をする
# （消した直後のフル再ビルドは 5.57s → 18.14s。12.6 秒の実損）。しかしセッション
# ディレクトリは溜まり続け、実測 19〜30 MB/dir でディスクだけが膨らむ（実測で
# 1,126 個 / 21G まで到達していた）。回収する手段は cargo clean しかない。
#
# そこで cargo clean は「溜まったら」ではなく**定期的に**、テストが通ったあとに
# 自動で実行する。走らせるタイミングをテストの後にしてあるのは、その回の結果は
# もう出ていて、待たされているものが無いからである。代償は次回のフルビルドが
# incremental 無しの 27.8 秒になることだけで、それ以外は何も失わない。
# ---------------------------------------------------------------------------

# APFS のディレクトリ st_size は「エントリ数 × 32 バイト」ちょうどになる。
# 36〜852,720 エントリの 7 ディレクトリで誤差 0 を確認済み。実行 0.005 秒で、
# 実数を数える `find … -name '*.o' | wc -l` の 8.2 秒とは桁が違う。
# この定数は APFS 固有なので、Darwin 以外では掃除も検査もスキップする。
readonly APFS_DIR_BYTES_PER_ENTRY=32

# cargo clean を回す間隔（日）と、間隔を待たずに回す incremental セッション数。
# 300 セッションは実測 19〜30 MB/dir でおおよそ 6〜9 GB にあたる。
readonly CLEAN_MAX_AGE_DAYS="${DEV_TEST_CLEAN_MAX_AGE_DAYS:-7}"
readonly CLEAN_INCREMENTAL_SESSIONS=300

# 数値でない値を黙って受けると `find -mtime +abc` が失敗し、「掃除の時期ではない」と
# 見分けがつかないまま**永久に掃除されなくなる**。ここで落とす。
case "${CLEAN_MAX_AGE_DAYS}" in
'' | *[!0-9]*)
  echo "dev-test: DEV_TEST_CLEAN_MAX_AGE_DAYS は 0 以上の整数で指定してください（受け取った値: ${CLEAN_MAX_AGE_DAYS}）" >&2
  exit 2
  ;;
esac

# 最後に cargo clean した時刻の記録。target/ の中に置くので clean で必ず消え、
# 直後に置き直す。つまりこのファイルの mtime = 最後に掃除した時刻。
readonly CLEAN_STAMP=target/.dev-test-last-clean

# ディレクトリのエントリ数を返す（`.` と `..` を含む）。
# 読めない・数値でないときは非ゼロ終了。
dir_entries() {
  local z
  z=$(stat -f '%z' "$1" 2>/dev/null) || return 1
  case "${z}" in
  '' | *[!0-9]*) return 1 ;;
  esac
  echo $((z / APFS_DIR_BYTES_PER_ENTRY))
}

# incremental/ のセッションディレクトリ数。取れなければ 0。
incremental_sessions() {
  local entries
  [ -d target/debug/incremental ] || {
    echo 0
    return 0
  }
  entries=$(dir_entries target/debug/incremental) || {
    echo 0
    return 0
  }
  # dir_entries は `.` と `..` も数えるので引く。
  echo $((entries > 2 ? entries - 2 : 0))
}

# cargo clean を回すべき理由を 1 行で返す。回す必要がなければ非ゼロ終了。
clean_reason() {
  local sessions
  sessions=$(incremental_sessions)
  if [ "${sessions}" -gt "${CLEAN_INCREMENTAL_SESSIONS}" ]; then
    echo "incremental のセッションが ${sessions} 個（閾値 ${CLEAN_INCREMENTAL_SESSIONS}、実測 19〜30 MB/dir）"
    return 0
  fi
  # スタンプが無い状態は「掃除した直後」と区別できないので、掃除せず置くだけに
  # する。初回の実行が必ず cargo clean になるのを避けるため。
  [ -e "${CLEAN_STAMP}" ] || return 1
  if [ -z "$(find "${CLEAN_STAMP}" -maxdepth 0 -mtime "+${CLEAN_MAX_AGE_DAYS}" 2>/dev/null)" ]; then
    return 1
  fi
  echo "前回の cargo clean から ${CLEAN_MAX_AGE_DAYS} 日以上経過"
}

# 掃除本体。cargo clean を回したら 0、回さなかったら 1 を返す。
run_cargo_clean_if_due() {
  local reason
  reason=$(clean_reason) || return 1

  echo "==> cargo clean（${reason}）"
  echo "    次のフルビルドは incremental 無しの実測 27.8 秒になります。"

  # 掃除の失敗でランごと失敗扱いにはしない。ここまで来た時点でテストは全部通って
  # いるので、非ゼロで抜けると「テストが落ちた」と読まれる。ただし黙って諦めると
  # 掃除されないまま溜まり続けるので、警告は出してスタンプも進めない
  # （＝次のランでもう一度試す）。
  if ! cargo clean; then
    echo "dev-test: cargo clean に失敗しました。次のランで再試行します" >&2
    return 0
  fi
  mkdir -p target
  : >"${CLEAN_STAMP}"
}

# .o を掃く。.o は**ビルドの入力ではない**ので、キャッシュは 1 バイトも失われない。
sweep_stale_objects() {
  [ -d target/debug/deps ] || return 0

  local before after
  before=$(dir_entries target/debug/deps) || return 0
  find target/debug/deps -maxdepth 1 -name '*.o' -delete 2>/dev/null || true
  after=$(dir_entries target/debug/deps) || return 0

  if [ "${before}" -le "${after}" ]; then
    echo "dev-test: 掃除する .o はありませんでした（target/debug/deps: ${after} エントリ）"
    return 0
  fi
  echo "dev-test: .o を $((before - after)) 個掃除しました（target/debug/deps: ${before} → ${after} エントリ）"
}

# テストが通ったあとの後始末。ビルドかテストが落ちた回は set -e でここまで来ない
# ので、デバッグ中の回は debuginfo も incremental もそのまま残る。
cleanup_target() {
  [ -z "${DEV_TEST_SKIP_TARGET_CLEANUP:-}" ] || return 0
  [ "$(uname -s)" = "Darwin" ] || return 0

  # cargo clean を回したなら .o もろとも消えているので、掃く対象はもう無い。
  run_cargo_clean_if_due && return 0
  [ -e "${CLEAN_STAMP}" ] || { mkdir -p target && : >"${CLEAN_STAMP}"; }
  sweep_stale_objects
}

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
cleanup_target

echo
echo "dev-test: 完了（fmt / clippy / arch-lint / cargo doc はこのスクリプトの守備範囲外）"
