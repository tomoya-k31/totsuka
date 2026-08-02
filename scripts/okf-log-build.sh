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
#   日付は降順（`docs/CLAUDE.md` の「新しい日付が上」）。
#   **同日内はファイル名（= slug）の昇順であって、時刻順ではない。** 断片には
#   作成時刻が残らないので決定的な鍵はファイル名しかなく、slug は任意の語なので
#   後から足したエントリが先のエントリより上に出ることがある（`abc-…` は
#   `zzz-…` より前）。同日内の並びに意味を持たせないこと。
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
      ! printf '%s' "${b}" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}-[a-z0-9][a-z0-9-]*\.md$'; then
      echo "okf-log-build: 断片名が規約外: ${FRAGDIR}/${b}" >&2
      echo "  → YYYY-MM-DD-<slug>.md（slug は必須・英小文字/数字/ハイフン）" >&2
      bad=1
      continue
    fi
    # 中身も検査する。断片は lint の frontmatter 走査から prune されており、
    # log-sync は「生成物と材料が一致するか」しか見ないので、ここで弾かないと
    # 規約違反がそのまま log.md へ流れて**全チェックが緑のまま**出荷される。
    if head -n 1 "${f}" | grep -q '^---[[:space:]]*$'; then
      echo "okf-log-build: 断片に frontmatter がある: ${FRAGDIR}/${b}" >&2
      echo "  → 断片は concept ではない。エントリ本体（\`* **Update**: …\`）だけを書く" >&2
      bad=1
      continue
    fi
    if grep -q '^## ' "${f}"; then
      echo "okf-log-build: 断片に \`## \` 見出しがある: ${FRAGDIR}/${b}" >&2
      echo "  → 日付見出しはファイル名から生成側が出す。断片には書かない" >&2
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

  # 断片ゼロで生成すると log.md が見出し 1 行に切り詰まる。log.md は移行後
  # **唯一の履歴の置き場ではない**（材料は log.d/）ものの、切り詰めたものを
  # コミットすれば履歴は消える。しかも log-sync は「断片と一致」で緑になるので
  # 気づけない。断片が無いのは正常な状態ではないので、書かずに止める。
  if [ -z "${frags}" ]; then
    echo "okf-log-build: 断片が 1 つも無い: ${FRAGDIR}/" >&2
    echo "  → log.md を空で上書きしないため中止する。worktree が壊れていないか確認する" >&2
    echo "    (sparse checkout / stash / 別ディレクトリでの実行が典型)" >&2
    return 1
  fi

  printf '# Bundle Update Log\n'

  dates="$(printf '%s\n' "${frags}" | cut -c1-10 | LC_ALL=C sort -ru)"
  printf '%s\n' "${dates}" | while IFS= read -r d; do
    [ -n "${d}" ] || continue
    printf '\n## %s\n\n' "${d}"
    # A day's fragments concatenate into ONE Markdown list, so their spacing has
    # to be consistent or rumdl's MD076 fires — in both directions. A day whose
    # entries are all single-line is a *tight* list and must have no blank lines
    # between items; a day where any entry carries continuation paragraphs is a
    # *loose* list and must have them between all items. Deciding per day is
    # what keeps a day mixing the two kinds correct.
    #
    # This only became reachable when ADR-0031 made fragments per-PR: before
    # that a date had one fragment and there was nothing to separate.
    loose="$(
      printf '%s\n' "${frags}" | while IFS= read -r b; do
        case "${b}" in "${d}"-*) ;; *) continue ;; esac
        if emit_fragment "${FRAGDIR}/${b}" | grep -qE '^[[:space:]]*$'; then
          printf 'y'
          break
        fi
      done
    )"
    first=1
    printf '%s\n' "${frags}" | while IFS= read -r b; do
      case "${b}" in "${d}"-*) ;; *) continue ;; esac
      if [ "${first}" -eq 0 ] && [ -n "${loose}" ]; then
        printf '\n'
      fi
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
