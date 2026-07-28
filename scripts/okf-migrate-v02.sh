#!/usr/bin/env bash
# okf-migrate-v02.sh — OKF v0.1 → v0.2 の frontmatter 一括変換（依存: bash 3.2+, POSIX awk のみ）
#
# SPEC v0.2 §13.1 の破壊的変更 2 件と、§5.4 で標準キーになった `status` の
# 語彙ずれを機械的に解消する。冪等（変換済みのファイルは触らない）。
#
#   1. `timestamp: X`      → `generated: { by: <ACTOR>, at: X }`   (§5.2)
#      v0.1 の `timestamp` は「最後に意味のある変更をした日時」。v0.2 では
#      `generated.at` がそれを担い、`by`（actor / §7）が必須になる。過去の
#      ファイル単位の作者は復元できないため、`by` は一括で --actor の値を使う。
#   2. `status: active|accepted` → `status: stable`                (§5.4)
#      v0.2 の語彙は draft | stable | deprecated の 3 値。draft/deprecated は据え置き。
#   3. 本文の `# Citations` リスト → frontmatter の `sources`      (§5.1 / §13.1)
#      1 引用行 = 1 sources エントリ。`resource` は行内の最初のリンク先または
#      裸の URL。どちらも無い行は行文そのものを scope descriptor として扱う
#      （§5.1 が明示的に許容している）。`id` は元の採番を保つ `ref-N`。
#   4. `okf_version: "0.1"` → `okf_version: "0.2"`                 (§12)
#
# 変換しないもの（機械化できない/すべきでないため、必要なら手で足す）:
#   - `verified` / `stale_after`: 検証や陳腐化の事実は既存ファイルから導出できない
#   - 本文の per-claim 脚注化: 本リポジトリの本文には `[N]` 番号参照が 0 件のため
#     脚注を張る先が無い（sources への移設だけで情報は落ちない）
#
# 1 行に複数リンクを含む引用行は、2 本目以降の URL が `title` の中に平文として
# しか残らない。取りこぼしを黙って通さないよう WARN で列挙するので、対象行は
# 変換後に手で sources エントリを足すこと。
#
# 使い方: scripts/okf-migrate-v02.sh [bundleDir=docs] [--dry-run] [--actor ACTOR]
#   --dry-run     : 書き換えずに diff を表示する
#   --actor ACTOR : generated.by に入れる actor（既定: human:tomoya-k31）
# 終了コード: 変換に失敗したファイルがあれば 1
set -u

BUNDLE="docs"
DRY_RUN=0
ACTOR="human:tomoya-k31"
while [ $# -gt 0 ]; do
  case "$1" in
  --dry-run) DRY_RUN=1 ;;
  --actor)
    shift
    [ $# -gt 0 ] || {
      echo "--actor に値がない" >&2
      exit 2
    }
    ACTOR="$1"
    ;;
  --*)
    echo "unknown option: $1" >&2
    exit 2
    ;;
  *) BUNDLE="$1" ;;
  esac
  shift
done

[ -d "$BUNDLE" ] || {
  echo "okf-migrate: bundle directory not found: $BUNDLE" >&2
  exit 1
}
BUNDLE="${BUNDLE%/}"

CHANGED=0
FAILED=0
WARNED=0

# 1 ファイルを変換して stdout に出す。変換の有無・警告は stderr に
# "STAT<TAB>..." / "WARN<TAB>..." で出す（stdout は変換後の本文専用）。
migrate_one() {
  # LC_ALL=C: index()/substr() をバイト単位に固定する。区切りは常に ASCII
  # (`](`, `)`, 空白) なので、日本語混じりの行でもロケール依存で切り出しが
  # ずれない。日本語そのものはバイト列としてそのまま透過する。
  LC_ALL=C awk -v actor="$ACTOR" '
    # `[label](target)` を label だけに畳む（POSIX awk 縛りのため gensub は使わない）
    function flatten(s,   out, p, k, lb, lab, rest, q) {
      out = ""
      while (1) {
        p = index(s, "](")
        if (p == 0) { out = out s; break }
        lb = 0
        for (k = p; k >= 1; k--) if (substr(s, k, 1) == "[") { lb = k; break }
        if (lb == 0) { out = out substr(s, 1, p + 1); s = substr(s, p + 2); continue }
        out = out substr(s, 1, lb - 1)
        lab = substr(s, lb + 1, p - lb - 1)
        rest = substr(s, p + 2)
        q = index(rest, ")")
        if (q == 0) { out = out lab; s = rest; continue }
        out = out lab
        s = substr(rest, q + 1)
      }
      return out
    }
    # 行内の最初の `](target)` の target。無ければ ""
    function link_target(s,   p, rest, q) {
      p = index(s, "](")
      if (p == 0) return ""
      rest = substr(s, p + 2)
      q = index(rest, ")")
      if (q == 0) return ""
      return substr(rest, 1, q - 1)
    }
    # 行内の最初の裸 URL。無ければ ""
    function bare_url(s) {
      if (match(s, /https?:\/\/[^ )]+/) == 0) return ""
      return substr(s, RSTART, RLENGTH)
    }
    function trim(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
    # YAML の二重引用符スカラーに包む
    function yq(s) {
      gsub(/\\/, "\\\\", s)
      gsub(/"/, "\\\"", s)
      return "\"" s "\""
    }
    # plain scalar で安全に書けるか（空白・引用符・先頭指示文字が無いこと）
    function plain_ok(s,   h) {
      if (s == "" || index(s, " ") > 0 || index(s, "\t") > 0) return 0
      if (index(s, "\"") > 0 || index(s, "\047") > 0) return 0
      h = substr(s, 1, 1)
      if (h == "#" || h == "&" || h == "*" || h == "!" || h == "%" || h == "@" ||
          h == "`" || h == "," || h == "[" || h == "{" || h == "|" || h == ">") return 0
      return 1
    }

    { lines[NR] = $0 }

    END {
      n = NR
      if (n == 0 || lines[1] != "---") { print "STAT\tskip-no-frontmatter" > "/dev/stderr"; exit 3 }
      fmend = 0
      for (i = 2; i <= n; i++) if (lines[i] == "---") { fmend = i; break }
      if (fmend == 0) { print "STAT\tskip-no-fm-end" > "/dev/stderr"; exit 3 }

      # 既存キーの把握（冪等性の判定に使う）
      for (i = 2; i < fmend; i++) {
        c = index(lines[i], ":")
        if (c > 0 && lines[i] ~ /^[A-Za-z_][A-Za-z0-9_-]*:/) have[substr(lines[i], 1, c - 1)] = 1
      }

      # ---- 本文から `# Citations` セクションを切り出す ----
      cstart = 0; cend = 0
      for (i = fmend + 1; i <= n; i++) {
        if (lines[i] ~ /^# Citations[ \t]*$/) { cstart = i; break }
      }
      if (cstart > 0) {
        cend = n
        for (i = cstart + 1; i <= n; i++) if (lines[i] ~ /^# /) { cend = i - 1; break }
      }

      # ---- 引用行を sources エントリへ ----
      ns = 0
      if (cstart > 0 && !("sources" in have)) {
        seq = 0
        for (i = cstart + 1; i <= cend; i++) {
          raw = trim(lines[i])
          if (raw == "") continue
          seq++
          # 先頭の採番マーカー `[N] ` / `N. ` / `- ` を落とし、id 用に N を拾う
          num = ""
          if (match(raw, /^\[[0-9]+\][ \t]*/)) {
            num = substr(raw, 2, RLENGTH - 3); sub(/[ \t]*$/, "", num)
            raw = substr(raw, RSTART + RLENGTH)
          } else if (match(raw, /^[0-9]+\.[ \t]+/)) {
            num = substr(raw, 1, RLENGTH); sub(/\..*$/, "", num)
            raw = substr(raw, RSTART + RLENGTH)
          } else {
            sub(/^[*-][ \t]+/, "", raw)
          }
          if (num == "") num = seq

          res = link_target(raw)
          from_link = (res != "")
          if (res == "") res = bare_url(raw)
          title = flatten(raw)
          if (!from_link && res != "") {
            # 裸 URL は resource に移すので、title 側からは取り除いて重複を避ける
            p = index(title, res)
            if (p > 0) title = substr(title, 1, p - 1) substr(title, p + length(res))
          }
          gsub(/[ \t][ \t]+/, " ", title)   # URL 抜き取りで空いた穴を詰める
          title = trim(title)
          sub(/^[.,;:\-—][ \t]*/, "", title)
          sub(/[ \t]*[.,;:][ \t]*$/, "", title)
          title = trim(title)

          if (res == "") { res = title; title = "" }   # §5.1 scope descriptor
          if (res == "") continue

          ns++
          s_id[ns] = "ref-" num
          s_res[ns] = res
          s_title[ns] = title

          # 2 本目以降のリンクは title の平文にしか残らない。黙って落とさない。
          cnt = 0; tmp = raw
          while ((p = index(tmp, "](")) > 0) { cnt++; tmp = substr(tmp, p + 2) }
          if (cnt > 1) print "WARN\t" FILENAME ": 引用 [" num "] に複数リンクがある（2 本目以降は title 内の平文になる。手で sources に足すこと）" > "/dev/stderr"
        }
      }

      # ---- frontmatter を書き出す ----
      changed = 0
      print "---"
      for (i = 2; i < fmend; i++) {
        line = lines[i]
        c = index(line, ":")
        key = (c > 0 && line ~ /^[A-Za-z_][A-Za-z0-9_-]*:/) ? substr(line, 1, c - 1) : ""
        val = (c > 0) ? trim(substr(line, c + 1)) : ""

        if (key == "timestamp" && !("generated" in have)) {
          v = val
          if (length(v) >= 2 && (substr(v, 1, 1) == "\"" || substr(v, 1, 1) == "\047") &&
              substr(v, length(v), 1) == substr(v, 1, 1)) v = substr(v, 2, length(v) - 2)
          print "generated: { by: " actor ", at: " v " }"
          changed = 1
          continue
        }
        if (key == "status" && (val == "active" || val == "accepted")) {
          print "status: stable"
          changed = 1
          continue
        }
        if (key == "okf_version" && (val == "\"0.1\"" || val == "0.1" || val == "\0470.1\047")) {
          print "okf_version: \"0.2\""
          changed = 1
          continue
        }
        print line
      }
      if (ns > 0) {
        print "sources:"
        for (i = 1; i <= ns; i++) {
          print "  - id: " s_id[i]
          print "    resource: " (plain_ok(s_res[i]) ? s_res[i] : yq(s_res[i]))
          if (s_title[i] != "") print "    title: " yq(s_title[i])
        }
        changed = 1
      }
      print "---"

      # ---- 本文（Citations セクションを抜く）----
      last = n
      if (cstart > 0 && ns > 0) {
        # セクション削除後に末尾が空行だけにならないよう、実体のある最終行を探す
        if (cend >= n) { last = cstart - 1; while (last > fmend && trim(lines[last]) == "") last-- }
      }
      for (i = fmend + 1; i <= last; i++) {
        if (cstart > 0 && ns > 0 && i >= cstart && i <= cend) continue
        print lines[i]
      }
      if (cstart > 0 && ns == 0) print "WARN\t" FILENAME ": # Citations があるが sources を生成できなかった（手で確認すること）" > "/dev/stderr"
      print (changed ? "STAT\tchanged" : "STAT\tunchanged") > "/dev/stderr"
    }
  ' "$1"
}

echo "okf-migrate: OKF v0.1 → v0.2  (bundle=$BUNDLE, actor=$ACTOR$([ "$DRY_RUN" -eq 1 ] && echo ', dry-run'))"
echo ""

TMPOUT="${TMPDIR:-/tmp}/okf-migrate-out.$$"
TMPERR="${TMPDIR:-/tmp}/okf-migrate-err.$$"
trap 'rm -f "$TMPOUT" "$TMPERR"' EXIT INT TERM

# `find | while` はサブシェルになりカウンタが失われるため、一覧を変数に取る
FILES="$(find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -type f -name '*.md' -print | sort)"

for file in $FILES; do
  migrate_one "$file" >"$TMPOUT" 2>"$TMPERR"
  rc=$?

  grep '^WARN' "$TMPERR" | sed 's/^WARN	/WARN  /' && WARNED=1

  if [ "$rc" -eq 3 ]; then
    continue # frontmatter を持たない予約ファイル等
  elif [ "$rc" -ne 0 ]; then
    echo "ERROR $file: awk が異常終了した (rc=$rc)"
    FAILED=$((FAILED + 1))
    continue
  fi

  grep -q '^STAT	changed' "$TMPERR" || continue

  if cmp -s "$TMPOUT" "$file"; then
    continue # 変換対象キーがあったが結果が同一（冪等）
  fi

  CHANGED=$((CHANGED + 1))
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "--- $file"
    diff -u "$file" "$TMPOUT" | tail -n +3
    echo ""
  else
    cat "$TMPOUT" >"$file"
    echo "migrated: $file"
  fi
done

echo ""
echo "okf-migrate: ${CHANGED} file(s) $([ "$DRY_RUN" -eq 1 ] && echo 'would change' || echo 'changed'), ${FAILED} failure(s)"
[ "$WARNED" -eq 0 ] || echo "note: WARN 行のファイルは変換後に手で確認すること"
[ "$FAILED" -eq 0 ] || exit 1
exit 0
