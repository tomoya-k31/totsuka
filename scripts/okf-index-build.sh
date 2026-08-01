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

BEGIN_MARK="<!-- okf:index:begin -->"
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
#
# NB: マーカーの判定は**行そのものとの完全一致**（`$0 == b`）にすること。
# 部分一致や行頭一致にすると、検証（grep -qxF）と書き換え（awk）で受理する
# 集合がずれ、「検証は通るのに区間に入れない／出られない」状態が作れてしまう。
# 出られない側はファイル末尾までを丸ごと削除する。
existing_entries() {
  awk -v b="${BEGIN_MARK}" -v e="${END_MARK}" '
    $0 == b { inside = 1; next }
    $0 == e { inside = 0; next }
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
      if (seen[link]++) {
        # 同じ link が 2 行ある = merge=union が両側の行を残した後。タイトルまで
        # 食い違っている場合、先勝ちで畳むと**片側の curate した表記が無警告で
        # 消える**。畳んでよいのは完全に同じ行のときだけ。
        if (kept[link] != title)
          print "!\t" link "\t" kept[link] "\t" title
        next
      }
      kept[link] = title
      print link "\t" title
    }
  ' "$1"
}

# 1 ディレクトリぶんの index.md を正規化して stdout へ出す。
render_index() {
  idx="$1"
  dir="$(dirname "${idx}")"

  # 完全一致で数える。行頭以外に置かれた／インデントされたマーカーは awk 側が
  # 認識しないので、ここで受理すると区間に入れない・出られないまま書き換えが走る。
  if [ "$(grep -cxF "${BEGIN_MARK}" "${idx}")" != "1" ] ||
    [ "$(grep -cxF "${END_MARK}" "${idx}")" != "1" ]; then
    echo "okf-index-build: 生成マーカーが 1 組ちょうどでない: ${idx}" >&2
    echo "  → concept 一覧を ${BEGIN_MARK} と ${END_MARK} で囲む" >&2
    echo "    （インデント・行末の余分な文字・重複は不可。行そのものが一致すること）" >&2
    return 1
  fi
  if [ "$(grep -nxF "${BEGIN_MARK}" "${idx}" | cut -d: -f1)" -gt \
    "$(grep -nxF "${END_MARK}" "${idx}" | cut -d: -f1)" ]; then
    echo "okf-index-build: 終了マーカーが開始マーカーより前にある: ${idx}" >&2
    return 1
  fi

  # 区間内にエントリ行以外があったら**消さずに止める**。ここは丸ごと再生成される
  # ので、見出し・コメント・ネストした補足箇条を書くと無警告で消える。しかも
  # index-sync は一致するまで落ち続けるため、書いた本人が lint を通すために
  # 自分で削除を確定させる経路しか残らない。
  # NB: エントリ行の判定に正規表現を使わないこと。タイトルには `]` が入りうる
  # （`ADR-0030 … の [layout] 3 ノブ…`）ので `\[[^]]*\]` は途中で切れて誤検出する。
  # existing_entries と同じ index() ベースの解釈に揃える。
  stray="$(awk -v b="${BEGIN_MARK}" -v e="${END_MARK}" '
    function is_entry(s,   p, q, rest) {
      if (s !~ /^\* \[/) return 0
      s = substr(s, 3)
      p = index(s, "](")
      if (p == 0) return 0
      rest = substr(s, p + 2)
      q = index(rest, ")")
      if (q == 0) return 0
      return substr(rest, q + 1, 3) == " - "
    }
    $0 == b { inside = 1; next }
    $0 == e { inside = 0; next }
    !inside { next }
    /^[ \t]*$/ { next }
    is_entry($0) { next }
    { print NR ": " $0 }
  ' "${idx}")"
  if [ -n "${stray}" ]; then
    echo "okf-index-build: マーカー区間にエントリ行以外がある: ${idx}" >&2
    printf '%s\n' "${stray}" >&2
    echo "  → 区間は再生成されるので中身が消える。散文・見出し・コメントは区間の外へ" >&2
    return 1
  fi

  # 区間の**外**に concept 行があると、ビルダーは未掲載とみなして区間内にも
  # 同じ行を足し、恒久的な二重掲載になる（okf-lint は先勝ちで一致判定するので
  # index-listed も index-desc も通ってしまう）。
  outside="$(awk -v b="${BEGIN_MARK}" -v e="${END_MARK}" '
    # 同ディレクトリの concept を指すエントリ行か（`/` を含むリンクは
    # サブディレクトリ扱いなので対象外）。ここも index() ベースで解く。
    function concept_row(s,   p, q, rest, link) {
      if (s !~ /^\* \[/) return 0
      s = substr(s, 3)
      p = index(s, "](")
      if (p == 0) return 0
      rest = substr(s, p + 2)
      q = index(rest, ")")
      if (q == 0) return 0
      link = substr(rest, 1, q - 1)
      if (index(link, "/") > 0) return 0
      if (link !~ /\.md$/) return 0
      return substr(rest, q + 1, 3) == " - "
    }
    $0 == b { inside = 1; next }
    $0 == e { inside = 0; next }
    inside { next }
    /^<!--/ { comment = 1 }
    comment { if ($0 ~ /-->/) comment = 0; next }
    concept_row($0) { print NR ": " $0 }
  ' "${idx}")"
  if [ -n "${outside}" ]; then
    echo "okf-index-build: マーカー区間の外に concept 行がある: ${idx}" >&2
    printf '%s\n' "${outside}" >&2
    echo "  → 区間内へ移す（そのままだと二重掲載になり、どの検査でも落ちない）" >&2
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
  clash=0
  while IFS="$(printf '\t')" read -r link title alt1 alt2; do
    [ -n "${link:-}" ] || continue
    if [ "${link}" = "!" ]; then
      # existing_entries が報告したタイトル衝突（union の後始末で潰れる形）。
      # ここでは title=リンク先 / alt1・alt2=食い違った 2 つの表記。
      echo "okf-index-build: 同じ concept にタイトルが 2 通りある: ${idx}" >&2
      echo "  ${title}" >&2
      echo "    1) ${alt1}" >&2
      echo "    2) ${alt2}" >&2
      echo "  → どちらを残すか人が決める（先勝ちで畳むと片方の変更が黙って消える）" >&2
      clash=1
      continue
    fi
    [ -f "${dir}/${link}" ] || continue
    skip_file "${link}" && continue
    order="${order}${link}
"
    titles="${titles}${link}	${title}
"
  done <<EOF
$(existing_entries "${idx}")
EOF
  [ "${clash}" -eq 0 ] || return 1
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
