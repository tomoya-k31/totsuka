#!/usr/bin/env bash
# docs-freshness.sh — 人間向け docs/ が ai-docs/ の現在の内容から作られているかを検査する。
#
# なぜ必要か（#458 / ADR-0048）:
#   docs/ は ai-docs/ からの**生成物**だが、変換は機械的ではなく編集的である
#   （frontmatter を剥がすだけでなく、内部 issue 番号や判断過程を落とし、簡潔に
#   書き直す）。したがって「生成し直せば必ず同じ出力になる」という決定的な検査は
#   書けない。一方、検査を一切置かないと生成スキルの実行を忘れた PR から黙って
#   ズレる — このリポジトリでは**検査の無い手動同期が既に 2 度壊れている**
#   （ADR-0031 の元帳、ADR-0045 の撤回漏れ）。
#
#   そこで保証を 2 つに割る:
#     - 内容の正しさ  → 生成スキル + PR レビュー（人間）
#     - **古さの検出** → 本スクリプト（CI）
#   **「内容が正しい」ことは検査しない。**「ソースが変わったのに生成物が
#   追随していない」ことと「あるべき生成物が無い」ことだけを検出する。
#
# 2 方向を検査する:
#   1. **生成物 → ソース**: docs/ の各ページが持つマーカーの hash が、
#      名指したソースの現在の hash と一致するか。
#   2. **ソース → 生成物**: ai-docs/ の各ソースが宣言した生成物が実在し、
#      こちらを名指すマーカーを持っているか。
#   1 だけだと**生成物を消した／片方の言語を作り忘れた**変更が素通りする
#   （検査は「在るもの」しか見ないため）。それはこの機構が塞ぐはずの
#   「忘れる」そのものなので、宣言側からも突き合わせる。
#
# マーカー（生成物側、1 ファイルに 1 つ・先頭 HEAD_LINES 行以内）:
#   <!-- generated-from: ai-docs/development/config-reference.md sha256:<64hex> -->
#
# 宣言（ソース側）:
#   <!-- generates: docs/config-reference.md docs/config-reference.ja.md -->
#   対応表の正はここ（ソースファイル自身）であって、スキルの表はその写しである。
#   2 箇所に書くと必ず片方が腐るため。
#
# 手で書くページ:
#   EXEMPT に列挙したものだけがマーカー無しで許される。**判定はマーカーを
#   探す前に行う**ので、マーカー形式を本文で説明する手書きページを置いても
#   誤検出しない。列挙外でマーカーが無ければエラーにする — 「マーカーを
#   付け忘れたページ」が検査をすり抜けて永久に古いままになるのを防ぐため。
#
# 使い方:
#   scripts/docs-freshness.sh [humanDir=docs] [bundleDir=ai-docs]  # 検査
#   scripts/docs-freshness.sh --marker <sourcePath>                # マーカー1行を出力
#
#   `--marker` は docs/ を一切書き換えない。これは意図的で、
#   「hash だけ更新して内容は古いまま」という**検査を黙って無効化する近道**を
#   作らないため。内容を書き直した後に、その場でマーカー行を差し替えて使う。
set -u

HUMAN="docs"
BUNDLE="ai-docs"
MODE="check"
MARKER_SRC=""
POSITIONAL=0

# マーカーを探す範囲（ファイル先頭からの行数）。言語スイッチャの直後に置く規約。
HEAD_LINES=20

while [ $# -gt 0 ]; do
  case "$1" in
  --marker)
    MODE="marker"
    shift
    [ $# -gt 0 ] || {
      echo "docs-freshness: --marker にソースパスが要る" >&2
      exit 2
    }
    MARKER_SRC="$1"
    ;;
  --*)
    echo "unknown option: $1" >&2
    exit 2
    ;;
  *)
    if [ "${POSITIONAL}" -eq 0 ]; then
      HUMAN="$1"
    else
      BUNDLE="$1"
    fi
    POSITIONAL=$((POSITIONAL + 1))
    ;;
  esac
  shift
done
HUMAN="${HUMAN%/}"
BUNDLE="${BUNDLE%/}"

# マーカーを持たなくてよいページ（humanDir からの相対パス）。
EXEMPT="index.md index.ja.md"

sha256() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    echo "docs-freshness: shasum も sha256sum も見つからない" >&2
    exit 2
  fi
}

if [ "${MODE}" = "marker" ]; then
  [ -f "${MARKER_SRC}" ] || {
    echo "docs-freshness: ソースが無い: ${MARKER_SRC}" >&2
    exit 1
  }
  echo "<!-- generated-from: ${MARKER_SRC} sha256:$(sha256 "${MARKER_SRC}") -->"
  exit 0
fi

[ -d "${HUMAN}" ] || {
  echo "docs-freshness: ディレクトリが無い: ${HUMAN}" >&2
  exit 1
}
[ -d "${BUNDLE}" ] || {
  echo "docs-freshness: ディレクトリが無い: ${BUNDLE}" >&2
  exit 1
}

ERRORS=0
err() {
  echo "ERROR [$1] $2" >&2
  ERRORS=$((ERRORS + 1))
}

is_exempt() {
  for e in ${EXEMPT}; do
    [ "$1" = "$e" ] && return 0
  done
  return 1
}

marker_of() { # $1 = page → そのページのマーカー行（先頭 HEAD_LINES 行以内）
  head -n "${HEAD_LINES}" "$1" 2>/dev/null |
    grep -m 1 -o '<!-- generated-from:[^>]*-->' || true
}

# `find | while read` はサブシェルで回ると ERRORS が親に返らない。ここでは
# heredoc でリダイレクトするのでサブシェルにならず、かつ 1 行 1 パスなので
# 空白を含むファイル名でも壊れない（`for page in $(find ...)` は IFS で
# 分割されるため使わない）。bash 3.2 なので mapfile は使わない。

PAGE_COUNT=0
while IFS= read -r page; do
  [ -n "${page}" ] || continue
  PAGE_COUNT=$((PAGE_COUNT + 1))
  rel="${page#"${HUMAN}"/}"

  # EXEMPT はマーカーを探す前に判定する。マーカー形式を本文で説明する
  # 手書きページが「壊れたマーカーを持つ生成物」に見えるのを防ぐため。
  if is_exempt "${rel}"; then
    continue
  fi

  line=$(marker_of "${page}")

  if [ -z "${line}" ]; then
    err "marker" "${page}: 先頭 ${HEAD_LINES} 行に generated-from マーカーが無い（手書きページなら docs-freshness.sh の EXEMPT に足す）"
    continue
  fi

  src=$(echo "${line}" | sed -n 's/.*generated-from:[[:space:]]*\([^[:space:]]*\).*/\1/p')
  want=$(echo "${line}" | sed -n 's/.*sha256:\([0-9a-f]\{64\}\).*/\1/p')

  if [ -z "${src}" ] || [ -z "${want}" ]; then
    err "marker" "${page}: マーカーの形式が壊れている: ${line}"
    continue
  fi

  if [ ! -f "${src}" ]; then
    err "source" "${page}: ソースが存在しない: ${src}"
    continue
  fi

  got=$(sha256 "${src}")
  if [ "${got}" != "${want}" ]; then
    err "stale" "${page}: ソース ${src} が変わっているのに生成物が追随していない
      期待 (ページに記録): ${want}
      実際 (現在のソース): ${got}
   → 直し方: human-docs スキルで ${page} を作り直し、
     bash scripts/docs-freshness.sh --marker ${src}
     の出力でマーカー行を差し替える（hash だけ書き換えないこと）"
  fi
done <<EOF
$(find "${HUMAN}" -type f -name '*.md' | sort)
EOF

if [ "${PAGE_COUNT}" -eq 0 ]; then
  err "empty" "${HUMAN}/ に .md が 1 つも無い（検査対象ゼロで緑になるのを防ぐ）"
fi

# --- ソース → 生成物 -------------------------------------------------------
# 宣言した生成物が実在し、こちらを名指すマーカーを持っているかを見る。
DECL_COUNT=0
while IFS= read -r src; do
  [ -n "${src}" ] || continue
  decl=$(head -n "${HEAD_LINES}" "${src}" 2>/dev/null | grep -m 1 -o '<!-- generates:[^>]*-->' || true)
  [ -n "${decl}" ] || continue
  DECL_COUNT=$((DECL_COUNT + 1))

  outs=$(echo "${decl}" | sed -n 's/.*generates:[[:space:]]*\(.*\)[[:space:]]*-->.*/\1/p')
  if [ -z "${outs}" ]; then
    err "declares" "${src}: generates 宣言が空である: ${decl}"
    continue
  fi

  for out in ${outs}; do
    if [ ! -f "${out}" ]; then
      err "missing" "${src}: 宣言した生成物が無い: ${out}
   → 生成物を消したか片方の言語を作り忘れている。human-docs スキルで作るか、
     もう生成しないなら ${src} の generates 宣言から外すこと"
      continue
    fi
    back=$(marker_of "${out}")
    back_src=$(echo "${back}" | sed -n 's/.*generated-from:[[:space:]]*\([^[:space:]]*\).*/\1/p')
    if [ "${back_src}" != "${src}" ]; then
      err "mismatch" "${out}: generates で ${src} が名指しているのに、ページのマーカーは '${back_src:-なし}' を指している"
    fi
  done
done <<EOF
$(find "${BUNDLE}" -type f -name '*.md' | sort)
EOF

if [ "${ERRORS}" -ne 0 ]; then
  echo "" >&2
  echo "docs-freshness: ${ERRORS} error(s)" >&2
  exit 1
fi

echo "docs-freshness: 0 error(s)（${PAGE_COUNT} pages / ${DECL_COUNT} sources）"
