#!/usr/bin/env bash
# docs-freshness.sh — 人間向け docs/ が ai-docs/ の現在の内容から作られているかを検査する。
#
# なぜ必要か（#458 / ADR-0047）:
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
#   生成ページにソースの content hash を埋め、ソースの現在値と照合するだけ。
#   **「内容が正しい」ことは検査しない。**「ソースが変わったのに生成物が
#   追随していない」ことだけを検出する。
#
# マーカー:
#   生成ページの先頭付近（既定では言語スイッチャの次）に 1 行入れる:
#     <!-- generated-from: ai-docs/development/config-reference.md sha256:<64hex> -->
#   HTML コメントなので描画されない。1 ファイルに 1 つだけ置く。
#   日本語版と英語版が同一ソースから作られる場合、両方に同じマーカーが入る。
#
# 手で書くページ:
#   下の EXEMPT に列挙したものだけがマーカー無しで許される（人間向けの目次など、
#   ai-docs のどのファイルの生成物でもないページ）。それ以外にマーカーが無ければ
#   エラーにする — 「マーカーを付け忘れたページ」が検査をすり抜けて
#   永久に古いままになるのを防ぐため。
#
# 使い方:
#   scripts/docs-freshness.sh [humanDir=docs]          # 検査。ズレていれば exit 1
#   scripts/docs-freshness.sh --marker <sourcePath>    # 貼り付けるマーカー 1 行を出力
#
#   `--marker` は docs/ を一切書き換えない。これは意図的で、
#   「hash だけ更新して内容は古いまま」という**検査を黙って無効化する近道**を
#   作らないため。内容を書き直した後に、その場でマーカーを差し替えて使う。
set -u

HUMAN="docs"
MODE="check"
MARKER_SRC=""

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
  *) HUMAN="$1" ;;
  esac
  shift
done
HUMAN="${HUMAN%/}"

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

# `find | while read` はサブシェルで回ると ERRORS が親に返らないので、
# ファイル一覧を先に取ってから for で回す（bash 3.2 なので mapfile は使わない）。
PAGES=$(find "${HUMAN}" -type f -name '*.md' | sort)

for page in ${PAGES}; do
  rel="${page#"${HUMAN}"/}"

  line=$(grep -m 1 -o '<!-- generated-from:[^>]*-->' "${page}" 2>/dev/null || true)

  if [ -z "${line}" ]; then
    if is_exempt "${rel}"; then
      continue
    fi
    err "marker" "${page}: generated-from マーカーが無い（手書きページなら docs-freshness.sh の EXEMPT に足す）"
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
done

if [ "${ERRORS}" -ne 0 ]; then
  echo "" >&2
  echo "docs-freshness: ${ERRORS} error(s)" >&2
  exit 1
fi

echo "docs-freshness: 0 error(s)"
