#!/usr/bin/env bash
# arch-lint.sh — ワークスペース依存境界の Fitness Function（依存: cargo, jq, POSIX awk）
#
# ヘキサゴナル構成の不変条件を `cargo metadata --no-deps` の出力から機械検証する。
# 対象は「ワークスペース内クレート間」の依存のみ（crates.io 等の外部依存は対象外）。
#
# チェック内容:
#   [E] plugin-deps   : plugins/* の [dependencies] は plugin-protocol / plugin-sdk のみ
#   [E] plugin-dev    : plugins/* の [dev-dependencies] は 上記 + test-support のみ
#   [E] plugin-build  : plugins/* の [build-dependencies] にワークスペース内依存なし
#   [E] sdk-deps      : plugin-sdk の依存は plugin-protocol（dev は + test-support）のみ
#   [E] protocol-leaf : plugin-protocol はワークスペース内クレートに一切依存しない
#   [E] cycle         : ワークスペース内に依存循環がない
#
# 使い方: scripts/arch-lint.sh
# 終了コード: 違反 1 件以上で 1、前提ツール欠如で 2
set -u

# ---------------------------------------------------------------------------
# 許可リスト（宣言的ルール）
#
# 正当なアーキテクチャ変更で依存を追加する場合は、ここを更新した上で、同一 PR で
# docs/architecture/workspace-dependency-rules.md（必要なら ADR も）を更新すること。
# plugins/* の判定はクレート名の列挙ではなく manifest パス（plugins/ 配下）で行う
# ため、新プラグイン追加時にこのファイルの更新は不要。
# ---------------------------------------------------------------------------
PLUGIN_ALLOWED_NORMAL="plugin-protocol plugin-sdk"
PLUGIN_ALLOWED_DEV="plugin-protocol plugin-sdk test-support"
SDK_ALLOWED_NORMAL="plugin-protocol"
SDK_ALLOWED_DEV="plugin-protocol test-support"
# plugin-protocol は leaf: いかなる種類のワークスペース内依存も持たない。
# orchestrator-core / orchestrator-cli / test-support に個別許可リストはない
# （循環検査のみ対象）。

command -v jq >/dev/null 2>&1 || {
  echo "arch-lint: jq が必要です (brew install jq)" >&2
  exit 2
}
command -v cargo >/dev/null 2>&1 || {
  echo "arch-lint: cargo が必要です" >&2
  exit 2
}

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# --no-deps: 依存解決を行わない（ネットワーク不要・高速）。ワークスペースメンバーの
# マニフェスト宣言だけが得られれば境界検査には十分。
META="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml")" ||
  {
    echo "arch-lint: cargo metadata の実行に失敗" >&2
    exit 2
  }

ERRORS=0
error() {
  echo "ERROR [$1] $2: $3"
  ERRORS=$((ERRORS + 1))
}

# ワークスペース内依存のみを TSV で抽出: <クレート名> <種別> <依存名> <manifest相対パス>
# 種別は normal / dev / build（cargo metadata では normal が null）。
DEPS="$(jq -r --arg root "$ROOT/" '
  [.packages[].name] as $members
  | .packages[] as $p
  | $p.dependencies[]
  | select(.name as $n | $members | index($n))
  | [$p.name, (.kind // "normal"), .name, ($p.manifest_path | ltrimstr($root))]
  | @tsv' <<<"$META")"

# 許可リスト（空白区切り）に依存名が含まれるか (0=含まれる)
allowed() { case " $1 " in *" $2 "*) return 0 ;; *) return 1 ;; esac }

# ---------- 1) 依存境界（許可リスト検査）----------
while IFS="$(printf '\t')" read -r pkg kind dep manifest; do
  [ -n "$pkg" ] || continue
  case "$manifest" in
  plugins/*)
    case "$kind" in
    normal) allowed "$PLUGIN_ALLOWED_NORMAL" "$dep" ||
      error plugin-deps "$pkg" "[dependencies] に許可外のワークスペース内依存 '$dep'（許可: ${PLUGIN_ALLOWED_NORMAL}）" ;;
    dev) allowed "$PLUGIN_ALLOWED_DEV" "$dep" ||
      error plugin-dev "$pkg" "[dev-dependencies] に許可外のワークスペース内依存 '$dep'（許可: ${PLUGIN_ALLOWED_DEV}）" ;;
    build) error plugin-build "$pkg" "[build-dependencies] にワークスペース内依存 '$dep'（許可なし）" ;;
    esac
    ;;
  *)
    case "$pkg" in
    plugin-sdk)
      case "$kind" in
      normal) allowed "$SDK_ALLOWED_NORMAL" "$dep" ||
        error sdk-deps "$pkg" "[dependencies] に許可外のワークスペース内依存 '$dep'（許可: ${SDK_ALLOWED_NORMAL}）" ;;
      dev) allowed "$SDK_ALLOWED_DEV" "$dep" ||
        error sdk-deps "$pkg" "[dev-dependencies] に許可外のワークスペース内依存 '$dep'（許可: ${SDK_ALLOWED_DEV}）" ;;
      build) error sdk-deps "$pkg" "[build-dependencies] にワークスペース内依存 '$dep'（許可なし）" ;;
      esac
      ;;
    plugin-protocol)
      error protocol-leaf "$pkg" "leaf クレートがワークスペース内クレート '$dep' に依存（種別: ${kind}）"
      ;;
    esac
    ;;
  esac
done <<<"$DEPS"

# ---------- 2) 依存循環（Kahn 法: 入次数 0 のノードを繰り返し除去）----------
# normal + build + dev の全エッジで検査する。dev-dependencies だけの循環は cargo
# 的には合法だが、本ワークスペースでは意図しない結合とみなす。正当な理由で導入
# する場合はこのルール（と docs）を見直すこと。
CYCLE_NODES="$(
  {
    jq -r '.packages[].name | "node\t" + .' <<<"$META"
    [ -z "$DEPS" ] || awk -F '\t' '{ print "edge\t" $1 "\t" $3 }' <<<"$DEPS"
  } | awk -F '\t' '
    $1 == "node" { nodes[$2] = 1; next }
    $1 == "edge" {
      fwd[$2] = fwd[$2] " " $3; indeg[$3]++
      bwd[$3] = bwd[$3] " " $2; outdeg[$2]++
      next
    }
    # 次数 0 のノードを繰り返し除去。残るのは「循環から到達できる集合」（順方向）
    # と「循環へ到達できる集合」（逆方向）で、両者の交差が循環の実メンバー。
    function peel(adj, deg, removed,   changed, n, cnt, tos, i) {
      changed = 1
      while (changed) {
        changed = 0
        for (n in nodes) {
          if (!(n in removed) && deg[n] + 0 == 0) {
            removed[n] = 1; changed = 1
            cnt = split(adj[n], tos, " ")
            for (i = 1; i <= cnt; i++) if (tos[i] != "") deg[tos[i]]--
          }
        }
      }
    }
    END {
      peel(fwd, indeg, removedF)
      peel(bwd, outdeg, removedB)
      rem = ""
      for (n in nodes) if (!(n in removedF) && !(n in removedB)) \
        rem = rem (rem == "" ? "" : " ") n
      print rem
    }'
)"
[ -z "$CYCLE_NODES" ] || error cycle workspace "依存循環に含まれるクレート: $CYCLE_NODES"

# ---------- サマリ ----------
N_PKGS="$(jq -r '.packages | length' <<<"$META")"
N_DEPS=0
[ -z "$DEPS" ] || N_DEPS="$(printf '%s\n' "$DEPS" | grep -c .)"
echo ""
echo "arch-lint: ${ERRORS} error(s)（${N_PKGS} crates / ワークスペース内依存 ${N_DEPS} 本を検査）"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
