#!/usr/bin/env bash
# okf-index-build.sh — 各ディレクトリの index.md の concept 一覧を正規化する（#360）。
#
# 「生成」ではなく**正規化**である。並び順と表示タイトルは index.md 自身が持つ
# データで、このスクリプトはそれを保存する:
#
#   - 既に載っている concept は**その順番のまま**。並びは curate されていて
#     ファイル名順ではない（components はレイヤ順、glossary は概念の導入順、
#     product は英語 canonical が先で `.ja.md` が後 — ファイル名昇順にすると
#     `.ja.md` が先に来る）。
#   - `[Title]` も既存の表記を残す。frontmatter と意図的に違うものがある
#     （`orchestrator-core` ⇄ `orchestrator-core クレート` 等 10 件）。
#     新規追加のときだけ frontmatter の `title` を使う。
#
# 機械が持つのは次の 4 つだけ:
#   1. description を frontmatter から転記し直す（`docs/CLAUDE.md` の全文一致規約）
#   2. 未掲載の concept を末尾へ追記する（ファイル名昇順）
#   3. 消えた concept の行を落とす
#   4. 同じリンク先の重複行を畳む（`merge=union` が両側の行を残した後の後始末）
#
# 4 が要るのは `.gitattributes` の `docs/**/index.md merge=union` と対になって
# いるから: union はコンフリクトを出さない代わりに両側の行をそのまま残すので、
# 最後に決定的な形へ寄せる主体がどこかに要る。
#
# 対象は `<!-- okf:index:begin ... -->` と `<!-- okf:index:end -->` に挟まれた
# 範囲だけ。前後の散文・見出し・サブディレクトリへのリンクは触らない
# （ルート `docs/index.md` はディレクトリ一覧を手書きの説明文で持つので対象外）。
#
# 使い方:
#   scripts/okf-index-build.sh [bundleDir=docs]           # 書き換える
#   scripts/okf-index-build.sh [bundleDir=docs] --check   # 差分があれば表示して exit 1
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

BEGIN_MARK="<!-- okf:index:begin"
END_MARK="<!-- okf:index:end -->"

# index への掲載対象外（scripts/okf-lint.sh の is_reserved / is_exempt と同じ集合）
skip_file() {
  case "$1" in index.md | log.md | README.md | CLAUDE.md) return 0 ;; *) return 1 ;; esac
}

# frontmatter の 1 キーを取り出す（scripts/okf-lint.sh の fm_value と同じ意味論:
# トップレベルのキーのみ、前後の空白を落とし、引用符で囲まれていれば剥がす）。
fm_value() {
  awk -v want="$2" '
    NR==1 { if ($0 != "---") exit; fm=1; next }
    fm && $0=="---" { exit }
    fm {
      if ($0 ~ /^[ ]/) next
      c = index($0, ":")
      if (c == 0) next
      if (substr($0, 1, c-1) != want) next
      val = substr($0, c+1)
      sub(/^[ ]+/, "", val); sub(/[ ]+$/, "", val)
      n = length(val)
      if (n >= 2 && substr(val,1,1) == "\"" && substr(val,n,1) == "\"") {
        val = substr(val, 2, n-2); gsub(/\\"/, "\"", val); gsub(/\\\\/, "\\", val)
      } else if (n >= 2 && substr(val,1,1) == "'"'"'" && substr(val,n,1) == "'"'"'") {
        val = substr(val, 2, n-2); gsub(/'"'"''"'"'/, "'"'"'", val)
      }
      print val
      exit
    }
  ' "$1"
}

# マーカー区間の既存エントリを "<link>\t<title>" で列挙（重複は先勝ち）。
existing_entries() {
  awk -v b="${BEGIN_MARK}" -v e="${END_MARK}" '
    index($0, b) == 1 { inside = 1; next }
    index($0, e) == 1 { inside = 0; next }
    !inside { next }
    {
      line = $0
      sub(/^[ ]*[*-][ ]+/, "", line)
      if (line !~ /^\[/) next
      p = index(line, "](")
      if (p == 0) next
      title = substr(line, 2, p - 2)
      rest = substr(line, p + 2)
      q = index(rest, ")")
      if (q == 0) next
      link = substr(rest, 1, q - 1)
      if (seen[link]++) next
      print link "\t" title
    }
  ' "$1"
}

# 1 ディレクトリぶんの index.md を正規化して stdout へ出す。
render_index() {
  idx="$1"
  dir="$(dirname "${idx}")"

  if ! grep -qF "${BEGIN_MARK}" "${idx}" || ! grep -qF "${END_MARK}" "${idx}"; then
    echo "okf-index-build: 生成マーカーが無い: ${idx}" >&2
    echo "  → concept 一覧を ${BEGIN_MARK} … --> と ${END_MARK} で囲む" >&2
    return 1
  fi

  # ディレクトリ内の concept（ファイル名昇順）
  concepts=""
  for f in "${dir}"/*.md; do
    [ -f "${f}" ] || continue
    b="$(basename "${f}")"
    skip_file "${b}" && continue
    concepts="${concepts}${b}
"
  done
  concepts="$(printf '%s' "${concepts}" | LC_ALL=C sort)"

  # 出力順 = 既存の順（実在するものだけ）→ 未掲載のものをファイル名昇順で追記
  order=""
  titles=""
  while IFS="$(printf '\t')" read -r link title; do
    [ -n "${link:-}" ] || continue
    [ -f "${dir}/${link}" ] || continue
    skip_file "${link}" && continue
    order="${order}${link}
"
    titles="${titles}${link}	${title}
"
  done <<EOF
$(existing_entries "${idx}")
EOF
  while IFS= read -r c; do
    [ -n "${c}" ] || continue
    printf '%s' "${order}" | grep -qxF "${c}" && continue
    order="${order}${c}
"
  done <<EOF
${concepts}
EOF

  # マーカー区間の外はそのまま、中は order どおりに書き直す
  awk -v b="${BEGIN_MARK}" -v e="${END_MARK}" '
    index($0, b) == 1 { print; print "@@OKF_INDEX_BODY@@"; inside = 1; next }
    index($0, e) == 1 { inside = 0; print; next }
    !inside { print }
  ' "${idx}" |
    while IFS= read -r line; do
      if [ "${line}" = "@@OKF_INDEX_BODY@@" ]; then
        printf '%s' "${order}" | while IFS= read -r link; do
          [ -n "${link}" ] || continue
          title="$(printf '%s' "${titles}" |
            awk -F'\t' -v k="${link}" '$1 == k { print substr($0, index($0, "\t") + 1); exit }')"
          [ -n "${title}" ] || title="$(fm_value "${dir}/${link}" title)"
          [ -n "${title}" ] || title="${link%.md}"
          desc="$(fm_value "${dir}/${link}" description)"
          printf '* [%s](%s) - %s\n' "${title}" "${link}" "${desc}"
        done
      else
        printf '%s\n' "${line}"
      fi
    done
}

RC=0
TMP="$(mktemp "${TMPDIR:-/tmp}/okf-index-build.XXXXXX")"
trap 'rm -f "${TMP}"' EXIT

# ルート index.md はディレクトリ一覧を手書き説明で持つので対象外。log.d は概念置き場ではない。
for idx in $(find "${BUNDLE}" -mindepth 2 -name index.md -not -path "${BUNDLE}/log.d/*" | LC_ALL=C sort); do
  if ! render_index "${idx}" >"${TMP}"; then
    RC=1
    continue
  fi
  if [ "${CHECK}" -eq 1 ]; then
    if ! diff -u "${idx}" "${TMP}" >/dev/null 2>&1; then
      echo "okf-index-build: ${idx} が concept と同期していない（左=現在, 右=生成）:" >&2
      diff -u "${idx}" "${TMP}" >&2 || true
      echo "  → 直し方: bash scripts/okf-index-build.sh" >&2
      RC=1
    fi
  else
    cat "${TMP}" >"${idx}"
  fi
done

exit "${RC}"
