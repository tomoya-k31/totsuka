#!/usr/bin/env bash
# arch-lint.sh — ワークスペース依存境界の Fitness Function（依存: cargo, jq, POSIX awk/grep）
#
# ヘキサゴナル構成の不変条件を `cargo metadata --no-deps` の出力から機械検証する。
# 対象は「ワークスペース内クレート間」の依存のみ（crates.io 等の外部依存は対象外）。
#
# チェック内容:
#   [E] plugin-deps   : plugins/* の [dependencies] は plugin-protocol / plugin-sdk のみ
#   [E] plugin-dev    : plugins/* の [dev-dependencies] は 上記 + test-support のみ
#   [E] plugin-build  : plugins/* の [build-dependencies] にワークスペース内依存なし
#   [E] sdk-deps      : plugin-sdk の依存は plugin-protocol（dev は + test-support）のみ、
#                       [build-dependencies] にワークスペース内依存なし
#   [E] protocol-leaf : plugin-protocol はワークスペース内クレートに一切依存しない
#   [E] cycle         : ワークスペース内に依存循環がない
#   [E] plugin-bin-name : plugins/* は bin ターゲットをちょうど 1 つ持ち、その名前が
#                       同ディレクトリの plugin.toml の `name` と一致する
#
# 使い方: scripts/arch-lint.sh
# 終了コード: 違反 1 件以上で 1、前提ツール欠如・検査自体の失敗で 2
#
# フェイルクローズ: 検査パイプライン（jq / awk）自体の失敗は「違反なし」ではなく
# エラー終了にする（-e / pipefail + 明示ハンドラ）。素通りしたら Fitness Function
# の意味がない。
set -euo pipefail

# ---------------------------------------------------------------------------
# 許可リスト（宣言的ルール）
#
# 正当なアーキテクチャ変更で依存を追加する場合は、ここを更新した上で、同一 PR で
# ai-docs/architecture/workspace-dependency-rules.md（必要なら ADR も）を更新すること。
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
command -v awk >/dev/null 2>&1 || {
  echo "arch-lint: awk が必要です" >&2
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
  | @tsv' <<<"$META")" || {
  echo "arch-lint: cargo metadata の解析（jq）に失敗" >&2
  exit 2
}

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
    # と「循環へ到達できる集合」（逆方向）で、両者の交差が循環メンバー
    # （循環が複数ある場合は循環間の経路上のノードも含む。pass/fail 判定は不変）。
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
)" || {
  echo "arch-lint: 循環検査（awk）に失敗" >&2
  exit 2
}
[ -z "$CYCLE_NODES" ] || error cycle workspace "依存循環に含まれるクレート: $CYCLE_NODES"

# ---------- 3) プラグイン成果物の命名 ----------
# bin 名 = plugin.toml の `name` を強制する。この 2 つは長らく食い違っており
# （`task-source-slack` vs `slack`）、`plugin install` は後者と同名のバイナリを
# 要求するため、導入のたびに手作業のリネームと dist ディレクトリ組み立てが必要
# だった。揃えておくと `target/{profile}/<name>` がそのまま install 可能・配布可能
# になる（ADR-0027）。新しいプラグインで再発させないための Fitness Function。
#
# <クレート名> <plugins/ からの相対ディレクトリ> <bin ターゲット名をカンマ区切り>
PLUGIN_BINS="$(jq -r --arg root "$ROOT/" '
  .packages[]
  | select(.manifest_path | ltrimstr($root) | startswith("plugins/"))
  | [ .name,
      (.manifest_path | ltrimstr($root) | sub("/Cargo\\.toml$"; "")),
      ([.targets[] | select(.kind | index("bin")) | .name] | join(",")) ]
  | @tsv' <<<"$META")" || {
  echo "arch-lint: プラグイン bin ターゲットの解析（jq）に失敗" >&2
  exit 2
}

N_PLUGINS=0
while IFS="$(printf '\t')" read -r pkg dir bins; do
  [ -n "$pkg" ] || continue
  N_PLUGINS=$((N_PLUGINS + 1))
  manifest="$ROOT/$dir/plugin.toml"
  if [ ! -f "$manifest" ]; then
    error plugin-bin-name "$pkg" "$dir/plugin.toml が存在しない（プラグインは manifest を同梱すること）"
    continue
  fi
  # トップレベルの `name = "..."` だけを読む（最初のテーブル見出しで打ち切る）。
  want="$(awk -F '"' '
    /^[[:space:]]*\[/ { exit }
    /^[[:space:]]*name[[:space:]]*=/ { print $2; exit }' "$manifest")" || {
    echo "arch-lint: $dir/plugin.toml の解析（awk）に失敗" >&2
    exit 2
  }
  if [ -z "$want" ]; then
    error plugin-bin-name "$pkg" "$dir/plugin.toml にトップレベルの name が無い"
    continue
  fi
  case "$bins" in
  "") error plugin-bin-name "$pkg" "bin ターゲットが無い（'$want' という名前で 1 つ必要）" ;;
  *,*) error plugin-bin-name "$pkg" "bin ターゲットが複数ある（$bins）: プラグインは '$want' 1 つだけを持つこと" ;;
  "$want") ;;
  *) error plugin-bin-name "$pkg" "[[bin]] name = '$bins' が $dir/plugin.toml の name = '$want' と不一致（install は '$want' という名前のバイナリを要求する）" ;;
  esac
done <<<"$PLUGIN_BINS"

# フェイルクローズ: 抽出が 0 件になったら「違反なし」ではなく検査の失敗として扱う。
# 上の select は manifest_path から $ROOT/ を剥がして plugins/ 判定しており、両者は
# 同じ根（$ROOT/Cargo.toml を cargo に渡している）から導かれるので通常ずれないが、
# ずれれば全プラグインが黙って対象外になる。ワークスペースには必ずプラグインがある。
[ "$N_PLUGINS" -gt 0 ] || {
  echo "arch-lint: plugins/* を 1 つも抽出できなかった（cargo metadata のパス表現と ROOT=$ROOT が食い違っている可能性）" >&2
  exit 2
}

# ---------- サマリ ----------
N_PKGS="$(jq -r '.packages | length' <<<"$META")"
N_DEPS=0
[ -z "$DEPS" ] || N_DEPS="$(printf '%s\n' "$DEPS" | grep -c .)"
echo ""
echo "arch-lint: ${ERRORS} error(s)（${N_PKGS} crates / ワークスペース内依存 ${N_DEPS} 本 / プラグイン ${N_PLUGINS} 個を検査）"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
