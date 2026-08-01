#!/usr/bin/env bash
# okf-log-build.sh — docs/log.md を docs/log.d/ の断片ファイルから生成する。
#
# なぜ生成物なのか（#360）:
#   docs/log.md は「新しい日付が上」という規約上、**全 PR が同じ 1 行に書き込む**
#   ファイルだった。実測で、ログを触った直近 40 commit は 40 件すべてがファイル
#   先頭への挿入で、ログ追記を含む並行 PR 同士は運ではなく**決定論的に**
#   コンフリクトしていた。コンフリクト中の PR は `refs/pull/N/merge` を作れず
#   CI が一切走らないため、代償は「解決 → 再検査 → force-push → CI 全周回」1 式。
#   各 PR が**新規ファイル**を 1 枚置く形にすれば、その衝突源が構造的に消える。
#
# 断片ファイルの規約:
#   docs/log.d/YYYY-MM-DD-<slug>.md
#     - 日付は先頭 10 文字。`## YYYY-MM-DD` 見出しは**断片には書かない**（生成側が出す）
#     - slug は必須。同日に複数 PR が書いてもファイル名が衝突しないための唯一の仕掛け
#     - 中身は `* **Creation**: …` のようなエントリ本体のみ（複数エントリ可）
#
# 並び順:
#   日付は降順（`docs/CLAUDE.md` の「新しい日付が上」）。同日内はファイル名の昇順で、
#   これは同じく `docs/CLAUDE.md` の「同日ならエントリを追記する」に一致する
#   （断片には作成時刻が残らないので、決定的な鍵はファイル名しかない）。
#
# 使い方:
#   scripts/okf-log-build.sh [bundleDir=docs]           # docs/log.md を書き出す
#   scripts/okf-log-build.sh [bundleDir=docs] --check   # 差分があれば表示して exit 1
set -u

BUNDLE="docs"
CHECK=0
for a in "$@"; do
  case "$a" in
  --check) CHECK=1 ;;
  --*)
    echo "unknown option: $a" >&2
    exit 2
    ;;
  *) BUNDLE="$a" ;;
  esac
done
BUNDLE="${BUNDLE%/}"

LOG="${BUNDLE}/log.md"
FRAGDIR="${BUNDLE}/log.d"

[ -d "${FRAGDIR}" ] || {
  echo "okf-log-build: 断片ディレクトリが無い: ${FRAGDIR}" >&2
  exit 1
}

# 断片の末尾の空行を落とし、必ず改行 1 個で終わらせる。
# 断片ごとの内部の空行（複数段落エントリ）はそのまま残す — 移行前の log.md を
# バイト単位で再現できるのは、この「本文は verbatim」が効いているから。
emit_fragment() {
  awk '{ lines[NR] = $0 }
       END {
         last = 0
         for (i = 1; i <= NR; i++) if (lines[i] ~ /[^ \t]/) last = i
         for (i = 1; i <= last; i++) print lines[i]
       }' "$1"
}

# 断片の一覧（ファイル名の昇順）。名前が規約から外れているものは**黙って落とさず**
# ここで止める — 落とすと「ログを書いたのに log.md に出ない」という、
# 生成物であることが災いして誰も気づかない壊れ方をする。
# NB: ループを `| sort` に繋がないこと。繋ぐと本体がサブシェルになり `bad=1` が
# 外へ出られず、規約外の断片を報告しながら exit 0 で素通しする（＝この関数が
# 防ごうとしている「黙って落ちる」がそのまま起きる。shellcheck SC2030 で検出）。
list_fragments() {
  names=""
  bad=0
  for f in "${FRAGDIR}"/*; do
    [ -e "${f}" ] || continue
    b="$(basename "${f}")"
    case "${b}" in
    README.md) continue ;;
    esac
    if [ ! -f "${f}" ] ||
      ! printf '%s' "${b}" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}-[a-z0-9][a-z0-9.-]*\.md$'; then
      echo "okf-log-build: 断片名が規約外: ${FRAGDIR}/${b}" >&2
      echo "  → YYYY-MM-DD-<slug>.md（slug は必須・英小文字/数字/ハイフン）" >&2
      bad=1
      continue
    fi
    names="${names}${b}
"
  done
  [ "${bad}" -eq 0 ] || return 1
  printf '%s' "${names}" | LC_ALL=C sort
}

build() {
  frags="$(list_fragments)" || return 1

  printf '# Bundle Update Log\n'
  [ -n "${frags}" ] || return 0

  dates="$(printf '%s\n' "${frags}" | cut -c1-10 | LC_ALL=C sort -ru)"
  printf '%s\n' "${dates}" | while IFS= read -r d; do
    [ -n "${d}" ] || continue
    printf '\n## %s\n\n' "${d}"
    first=1
    printf '%s\n' "${frags}" | while IFS= read -r b; do
      case "${b}" in "${d}"-*) ;; *) continue ;; esac
      [ "${first}" -eq 1 ] || printf '\n'
      emit_fragment "${FRAGDIR}/${b}"
      first=0
    done
  done
}

TMP="$(mktemp "${TMPDIR:-/tmp}/okf-log-build.XXXXXX")"
trap 'rm -f "${TMP}"' EXIT
build >"${TMP}" || exit 1

if [ "${CHECK}" -eq 1 ]; then
  [ -f "${LOG}" ] || {
    echo "okf-log-build: ${LOG} が無い → bash scripts/okf-log-build.sh" >&2
    exit 1
  }
  if diff -u "${LOG}" "${TMP}" >/dev/null 2>&1; then
    exit 0
  fi
  echo "okf-log-build: ${LOG} が ${FRAGDIR}/ と同期していない（左=現在, 右=断片から生成）:" >&2
  diff -u "${LOG}" "${TMP}" >&2 || true
  echo "  → 直し方: bash scripts/okf-log-build.sh" >&2
  exit 1
fi

cat "${TMP}" >"${LOG}"
