#!/usr/bin/env bash
# dev-test.sh — ローカルのテスト実行（依存: cargo, cargo-nextest。jq があれば使う）
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
#   - 毎回: `<target>/debug/deps` の `.o` を掃く（キャッシュ損失ゼロ。効くのは
#     ビルド時間で、ディスクではない）
#   - 定期: `cargo clean --profile dev`（既定 7 日ごと、または incremental の
#     クレート単位ディレクトリが 300 個超）
# 掃除ごと止めたいときは DEV_TEST_SKIP_TARGET_CLEANUP=1、
# clean の間隔だけ変えたいときは DEV_TEST_CLEAN_MAX_AGE_DAYS=<日数>（0 で毎回）。
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
# ワークスペースの .o が <target>/debug/deps に**残り続ける**。cargo はこれを GC
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
# `cargo build --workspace --all-targets` が 0.33 秒の no-op）。
#
# **効くのはビルド時間であってディスクではない。** deps の .o は incremental の
# オブジェクトへの**ハードリンク**である（実測: link count 3、inode が
# incremental/<crate>/s-<session>/*.o と一致）。したがって deps 側のリンクを消しても
# 実体は残り、解放されるのは incremental 側が既に GC 済みだったものだけ。実際、
# 84 万件を掃いた木で減ったのは 34G → 28G にとどまった。ディスクを戻すのは
# cargo clean のほうの仕事である。
#
# 同じ理由で「このランでビルドした .o だけ残す」はできない。ハードリンクなので
# mtime は**その CGU を最初にコンパイルした時刻**であり、今回のビルドで作り直した
# ものでも古い時刻を持つ（実測で確認）。世代を mtime で切り分ける方法は無い。
#
# 同じ target を使う別プロセスと**排他はしていない**。掃除中に別のビルドがリンク段階
# に入っていると、開けなくなった .o でリンクエラーになりうる。起きるのは即座に見える
# 失敗で、再実行すれば直る（壊れたものが黙って残る類ではない）。
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
# （消した直後のフル再ビルドは 5.57s → 18.14s。12.6 秒の実損）。しかしクレート単位の
# ディレクトリは溜まり続け、実測 19〜30 MB/dir でディスクだけが膨らむ（実測で
# 1,126 個 / 21G まで到達していた）。回収する手段は cargo clean しかない。
#
# そこで cargo clean は「溜まったら」ではなく**定期的に**、テストが通ったあとに
# 自動で実行する。走らせるタイミングをテストの後にしてあるのは、その回の結果は
# もう出ていて、待たされているものが無いからである。
#
# **`--profile dev` を付けて <target>/debug だけを消す。** 素の `cargo clean` は
# <target>/release まで消してしまい、手元でビルドしたプラグインのリリースバイナリを
# 巻き込む（実測で確認: `--profile dev` は 11,116 ファイルを消し、release に置いた
# ファイルは残る）。それでも <target>/debug は clippy・cargo doc・rust-analyzer と
# 共有しているので、**次に回すそれらも cold になる**。フルビルドの 27.8 秒
# （空の target からの実測。10 コア・user 182 秒）だけでは済まない。
# ---------------------------------------------------------------------------

# cargo clean を回す間隔（日。0 なら毎回）と、間隔を待たずに回す incremental の
# クレート単位ディレクトリ数。<target>/debug/incremental の直下は
# `<crate>-<hash>/` で、その中に `s-<session>/` が入る。数えているのは前者。
# 300 個は実測 19〜30 MB/dir でおおよそ 6〜9 GB にあたる。
#
# `:-` ではなく `-` を使う: `DEV_TEST_CLEAN_MAX_AGE_DAYS=` と明示的に空を渡された
# ときも既定値で握り潰さず、下の検証で弾くため。
readonly CLEAN_MAX_AGE_DAYS="${DEV_TEST_CLEAN_MAX_AGE_DAYS-7}"
readonly CLEAN_INCREMENTAL_CRATE_DIRS=300

# 数値でない値を黙って受けると日数の比較が成立せず、「掃除の時期ではない」と
# 見分けがつかないまま**永久に掃除されなくなる**。ここで落とす。
# ただし掃除を切っている人には関係がないので、そのときは検証もしない。
if [ -z "${DEV_TEST_SKIP_TARGET_CLEANUP:-}" ]; then
  case "${CLEAN_MAX_AGE_DAYS}" in
  '' | *[!0-9]*)
    echo "dev-test: DEV_TEST_CLEAN_MAX_AGE_DAYS は 0 以上の整数で指定してください（受け取った値: ${CLEAN_MAX_AGE_DAYS}）" >&2
    exit 2
    ;;
  esac
fi

# target ディレクトリは cargo に聞く。CARGO_TARGET_DIR や .cargo/config.toml の
# build.target-dir で移動していると、`target` 決め打ちでは掃除が空振りする一方で
# cargo clean だけが本物に効く、という食い違いが起きる。実測 0.02 秒。
# jq が無い環境では環境変数と既定値に落とす。
resolve_target_dir() {
  local d=""
  if command -v jq >/dev/null 2>&1; then
    d=$(cargo metadata --format-version 1 --no-deps --offline 2>/dev/null |
      jq -r '.target_directory // empty' 2>/dev/null) || d=""
  fi
  case "${d}" in
  '' | null) echo "${CARGO_TARGET_DIR:-target}" ;;
  *) echo "${d}" ;;
  esac
}

TARGET_DIR=$(resolve_target_dir)
readonly TARGET_DIR
readonly DEPS_DIR="${TARGET_DIR}/debug/deps"
readonly INCREMENTAL_DIR="${TARGET_DIR}/debug/incremental"

# 最後に cargo clean した時刻の記録。<target> 直下なので
# `cargo clean --profile dev`（= <target>/debug のみ）では消えない。
readonly CLEAN_STAMP="${TARGET_DIR}/.dev-test-last-clean"

# ファイルの mtime を epoch 秒で返す。GNU stat と BSD stat では綴りが違ううえ、
# GNU の `stat -f` は「ファイルシステム情報」で成功してしまうので、素性を先に見る。
stat_mtime() {
  if stat --version >/dev/null 2>&1; then
    stat -c '%Y' "$1" 2>/dev/null
  else
    stat -f '%m' "$1" 2>/dev/null
  fi
}

# incremental/ 直下のクレート単位ディレクトリ数。取れなければ 0。
incremental_crate_dirs() {
  [ -d "${INCREMENTAL_DIR}" ] || {
    echo 0
    return 0
  }
  find "${INCREMENTAL_DIR}" -maxdepth 1 -mindepth 1 -type d 2>/dev/null |
    wc -l | tr -d ' '
}

# 前回の掃除から CLEAN_MAX_AGE_DAYS 日以上経っていれば 0。
#
# `find -mtime +N` は使わない。BSD find の -mtime は 24 時間単位を切り捨てるので
# `+7` は「8 日以上」になり、`0` も「毎回」ではなく「24 時間以上」になってしまう。
# epoch 秒で比べれば言葉どおりになる。
clean_due_by_age() {
  # スタンプが無い状態は「掃除した直後」と区別できないので、掃除せず置くだけに
  # する（下の ensure_clean_stamp）。初回の実行が必ず cargo clean になるのを
  # 避けるため。この規則が効くのは**この日数による経路だけ**で、下の
  # クレート単位ディレクトリ数による経路はスタンプの有無を見ない
  # ——既に膨らんでいる木は、初回だろうと掃除するのが正しい。
  [ -e "${CLEAN_STAMP}" ] || return 1

  local stamp now
  stamp=$(stat_mtime "${CLEAN_STAMP}") || return 1
  case "${stamp}" in
  '' | *[!0-9]*) return 1 ;;
  esac
  now=$(date +%s)
  [ "$((now - stamp))" -ge "$((CLEAN_MAX_AGE_DAYS * 86400))" ]
}

# cargo clean を回すべき理由を 1 行で返す。回す必要がなければ非ゼロ終了。
clean_reason() {
  local dirs
  dirs=$(incremental_crate_dirs)
  if [ "${dirs}" -gt "${CLEAN_INCREMENTAL_CRATE_DIRS}" ]; then
    echo "incremental のクレート単位ディレクトリが ${dirs} 個（閾値 ${CLEAN_INCREMENTAL_CRATE_DIRS}、実測 19〜30 MB/dir）"
    return 0
  fi
  clean_due_by_age || return 1
  echo "前回の cargo clean から ${CLEAN_MAX_AGE_DAYS} 日以上経過"
}

# cargo clean を回したら 0、回さなかった・失敗したら 1 を返す。
# **失敗を 0 で返さない**のが要点で、0 を返すと呼び出し側が「掃除済み」と見なして
# .o 掃除まで飛ばしてしまい、掃除が続けて失敗する環境では .o が野放しになる。
run_cargo_clean_if_due() {
  local reason
  reason=$(clean_reason) || return 1

  echo "==> cargo clean --profile dev（${reason}）"
  echo "    <target>/debug を消すので、次のフルビルドに加えて clippy / cargo doc /"
  echo "    rust-analyzer も cold になります（空からのフルビルドは実測 27.8 秒）。"

  # 掃除の失敗でランごと失敗扱いにはしない。ここまで来た時点でテストは全部通って
  # いるので、非ゼロで抜けると「テストが落ちた」と読まれる。ただし黙って諦めると
  # 掃除されないまま溜まり続けるので、警告は出してスタンプも進めない
  # （＝次のランでもう一度試す）。
  if ! cargo clean --profile dev; then
    echo "dev-test: cargo clean に失敗しました。次のランで再試行します" >&2
    return 1
  fi
  # スタンプ書き込みの失敗でランを落とさない（次のランでもう一度掃除するだけ）。
  : >"${CLEAN_STAMP}" 2>/dev/null || true
  return 0
}

# スタンプが無ければ置く。ここで失敗してもランは落とさない。
ensure_clean_stamp() {
  [ -e "${CLEAN_STAMP}" ] && return 0
  mkdir -p "${TARGET_DIR}" 2>/dev/null && : >"${CLEAN_STAMP}" 2>/dev/null || true
  return 0
}

# deps の .o を掃く。.o は**ビルドの入力ではない**ので、キャッシュは 1 バイトも
# 失われない。
sweep_stale_objects() {
  [ -d "${DEPS_DIR}" ] || return 0

  local removed
  removed=$(find "${DEPS_DIR}" -maxdepth 1 -name '*.o' \
    -print -delete 2>/dev/null | wc -l | tr -d ' ') || removed=0

  if [ "${removed}" -eq 0 ]; then
    echo "dev-test: 掃除する .o はありませんでした"
    return 0
  fi
  echo "dev-test: .o を ${removed} 個掃除しました（${DEPS_DIR}）"
}

# テストが通ったあとの後始末。ビルドかテストが落ちた回は set -e でここまで来ない
# ので、デバッグ中の回は debuginfo も incremental もそのまま残る。
cleanup_target() {
  [ -z "${DEV_TEST_SKIP_TARGET_CLEANUP:-}" ] || return 0

  # cargo clean を回せたなら .o もろとも消えているので、掃く対象はもう無い。
  if run_cargo_clean_if_due; then
    return 0
  fi
  ensure_clean_stamp
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
