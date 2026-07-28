#!/usr/bin/env bash
# okf-search.sh — OKF frontmatter フィルタ検索（依存: bash 3.2+, POSIX awk/grep/sed のみ）
#
# concept の本文ではなく frontmatter（type/resource/tags/timestamp/status/owner 等）を
# クエリキーとして絞り込む。本文の全文検索は行わない — 絞り込んだファイル一覧を
# 呼び出し元（Claude / okf-search スキル）が読み、実際の抽出・要約は AI 側で行う設計。
#
# 使い方:
#   scripts/okf-search.sh [bundleDir=docs] [フィルタ...] [出力オプション]
#
# フィルタ（すべて AND。未指定のキーはフィルタしない）:
#   --type VALUE          type の完全一致
#   --status VALUE        status の完全一致
#   --owner VALUE         owner の完全一致
#   --resource VALUE      resource の完全一致
#   --resource-like TEXT  resource の部分一致
#   --tag TAG             tags にこのタグを含む（繰り返し指定 or カンマ区切りで複数、AND）
#   --field KEY=VALUE     任意の frontmatter キーの完全一致（繰り返し指定可、AND）
#   --after TIMESTAMP     timestamp >= TIMESTAMP（ISO 8601 文字列比較）
#   --before TIMESTAMP    timestamp <= TIMESTAMP（ISO 8601 文字列比較）
#
# 出力オプション:
#   --paths-only          パスのみを1行1件で出力（xargs 等にパイプしやすい）
#   --list-values FIELD   FIELD（tags 含む）の distinct 値と件数の一覧を出力し、他のフィルタは無視する
#
# 例:
#   scripts/okf-search.sh --type Decision
#   scripts/okf-search.sh --status deprecated --paths-only
#   scripts/okf-search.sh --tag okf --after 2026-01-01
#   scripts/okf-search.sh --field owner=platform-team
#   scripts/okf-search.sh --list-values type
#
# 終了コード: マッチ0件でも 0。引数エラー時のみ 2。bundleDir が存在しない場合は 1。
set -u

usage() {
  sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
}

BUNDLE=""
OPT_TYPE=""
OPT_STATUS=""
OPT_OWNER=""
OPT_RESOURCE=""
OPT_RESOURCE_LIKE=""
OPT_AFTER=""
OPT_BEFORE=""
TAGS_REQ=""
FIELDS_REQ=""
PATHS_ONLY=0
LIST_FIELD=""
US="$(printf '\037')" # unit separator（フィルタの複数値を1変数にまとめて awk に渡す区切り）

while [ $# -gt 0 ]; do
  case "$1" in
  --type)
    OPT_TYPE="$2"
    shift 2
    ;;
  --status)
    OPT_STATUS="$2"
    shift 2
    ;;
  --owner)
    OPT_OWNER="$2"
    shift 2
    ;;
  --resource)
    OPT_RESOURCE="$2"
    shift 2
    ;;
  --resource-like)
    OPT_RESOURCE_LIKE="$2"
    shift 2
    ;;
  --after)
    OPT_AFTER="$2"
    shift 2
    ;;
  --before)
    OPT_BEFORE="$2"
    shift 2
    ;;
  --tag)
    old_ifs="$IFS"
    IFS=','
    for t in $2; do
      IFS="$old_ifs"
      t="$(printf '%s' "$t" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
      [ -n "$t" ] && TAGS_REQ="${TAGS_REQ}${t}${US}"
    done
    IFS="$old_ifs"
    shift 2
    ;;
  --field)
    case "$2" in
    *=*) ;;
    *)
      echo "okf-search: --field は KEY=VALUE 形式で指定してください: $2" >&2
      exit 2
      ;;
    esac
    FIELDS_REQ="${FIELDS_REQ}${2}${US}"
    shift 2
    ;;
  --paths-only)
    PATHS_ONLY=1
    shift
    ;;
  --list-values)
    LIST_FIELD="$2"
    shift 2
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  --*)
    echo "okf-search: unknown option: $1" >&2
    exit 2
    ;;
  *)
    if [ -n "$BUNDLE" ]; then
      echo "okf-search: unexpected argument: $1" >&2
      exit 2
    fi
    BUNDLE="$1"
    shift
    ;;
  esac
done
BUNDLE="${BUNDLE:-docs}"

[ -d "$BUNDLE" ] || {
  echo "okf-search: bundle directory not found: $BUNDLE" >&2
  exit 1
}
BUNDLE="${BUNDLE%/}"

AWK_PROG="/tmp/okf-search-filter.$$.awk"
RESULTS="/tmp/okf-search-results.$$"
trap 'rm -f "$AWK_PROG" "$RESULTS"' EXIT

cat >"$AWK_PROG" <<'AWK'
BEGIN {
  state = 0
  ntagreq = 0
  if (tags_req != "") ntagreq = split(tags_req, tagreq_arr, "\037")
  nfieldreq = 0
  if (fields_req != "") nfieldreq = split(fields_req, fieldreq_arr, "\037")
}
NR == 1 {
  if ($0 == "---") { state = 1; next }
  exit 1
}
state == 1 && $0 == "---" {
  if (list_field != "") {
    if (list_field == "tags") {
      tagstr = fmval["tags"]
      gsub(/^\[/, "", tagstr); gsub(/\]$/, "", tagstr)
      ntags = split(tagstr, tagarr, ",")
      for (i = 1; i <= ntags; i++) {
        t = tagarr[i]
        gsub(/^[ \t]+|[ \t]+$/, "", t)
        if (t != "") print t
      }
    } else if (fmval[list_field] != "") {
      print fmval[list_field]
    }
    exit 0
  }

  if (type_f != "" && fmval["type"] != type_f) exit 1
  if (status_f != "" && fmval["status"] != status_f) exit 1
  if (owner_f != "" && fmval["owner"] != owner_f) exit 1
  if (resource_f != "" && fmval["resource"] != resource_f) exit 1
  if (resource_like != "" && index(fmval["resource"], resource_like) == 0) exit 1
  if (after_f != "" && (fmval["timestamp"] == "" || fmval["timestamp"] < after_f)) exit 1
  if (before_f != "" && (fmval["timestamp"] == "" || fmval["timestamp"] > before_f)) exit 1

  tagstr = fmval["tags"]
  gsub(/^\[/, "", tagstr); gsub(/\]$/, "", tagstr)
  ntags = split(tagstr, tagarr, ",")
  for (i = 1; i <= ntags; i++) {
    t = tagarr[i]
    gsub(/^[ \t]+|[ \t]+$/, "", t)
    if (t != "") have_tag[t] = 1
  }
  for (i = 1; i <= ntagreq; i++) {
    if (tagreq_arr[i] == "") continue
    if (!(tagreq_arr[i] in have_tag)) exit 1
  }

  for (i = 1; i <= nfieldreq; i++) {
    if (fieldreq_arr[i] == "") continue
    eq = index(fieldreq_arr[i], "=")
    k = substr(fieldreq_arr[i], 1, eq - 1)
    v = substr(fieldreq_arr[i], eq + 1)
    if (fmval[k] != v) exit 1
  }

  printf "%s\t%s\t%s\t%s\t%s — %s\n", FILENAME, fmval["type"], fmval["status"], fmval["timestamp"], fmval["title"], fmval["description"]
  exit 0
}
state == 1 {
  line = $0
  if (match(line, /^[A-Za-z_][A-Za-z0-9_]*:/)) {
    key = substr(line, 1, RSTART + RLENGTH - 2)
    val = substr(line, RSTART + RLENGTH)
    gsub(/^[ \t]+|[ \t]+$/, "", val)
    # 引用符は YAML の構文であって値の一部ではない（`: ` や ` #` を含む
    # description は引用が必須 — docs/CLAUDE.md）。外してから比較・表示する。
    n = length(val)
    if (n >= 2 && substr(val, 1, 1) == "\"" && substr(val, n, 1) == "\"") {
      val = substr(val, 2, n - 2)
      gsub(/\\"/, "\"", val); gsub(/\\\\/, "\\", val)
    } else if (n >= 2 && substr(val, 1, 1) == "'" && substr(val, n, 1) == "'") {
      val = substr(val, 2, n - 2)
      gsub(/''/, "'", val)
    }
    fmval[key] = val
  }
  next
}
AWK

is_reserved() { case "$1" in index.md | log.md) return 0 ;; *) return 1 ;; esac }

find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -type f -name '*.md' -print |
  while IFS= read -r file; do
    base="$(basename "$file")"
    is_reserved "$base" && continue
    awk -v type_f="$OPT_TYPE" -v status_f="$OPT_STATUS" -v owner_f="$OPT_OWNER" \
      -v resource_f="$OPT_RESOURCE" -v resource_like="$OPT_RESOURCE_LIKE" \
      -v after_f="$OPT_AFTER" -v before_f="$OPT_BEFORE" \
      -v tags_req="$TAGS_REQ" -v fields_req="$FIELDS_REQ" -v list_field="$LIST_FIELD" \
      -f "$AWK_PROG" "$file"
  done >"$RESULTS"

if [ -n "$LIST_FIELD" ]; then
  echo "# distinct values for '$LIST_FIELD' in $BUNDLE" >&2
  sort "$RESULTS" | uniq -c | sort -rn
  exit 0
fi

if [ "$PATHS_ONLY" -eq 1 ]; then
  cut -f1 "$RESULTS"
else
  echo -e "path\ttype\tstatus\ttimestamp\ttitle — description"
  cat "$RESULTS"
fi

N=$(wc -l <"$RESULTS" | tr -d ' ')
echo "okf-search: ${N} matched (bundle: $BUNDLE)" >&2
