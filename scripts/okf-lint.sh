#!/usr/bin/env bash
# okf-lint.sh — OKF v0.1 バンドル検証（依存: bash 3.2+, POSIX awk/grep/sed のみ）
#
# チェック内容:
#   [E] frontmatter : index.md / log.md 以外の全 .md が YAML frontmatter を持つ
#   [E] fm-yaml     : frontmatter が YAML として壊れていない（後述の部分集合で検証）
#   [E] type        : frontmatter に空でない `type` がある (SPEC §4.1 / §9)
#   [E] description : frontmatter に空でない `description` がある (docs/CLAUDE.md)
#   [E] index-fm    : ルート以外の index.md が frontmatter を持たない (SPEC §6/§11)
#   [E] index-exists: concept/サブディレクトリを含むディレクトリに index.md がある
#   [E] index-listed: 各 concept / サブディレクトリが index.md からリンクされている
#   [E] index-desc  : index.md の転記が concept の description と一致する
#   [E] log-format  : log.md の日付見出しが `## YYYY-MM-DD` 形式
#
# fm-yaml について: 本バンドルの frontmatter は「1 行 1 キーの平坦なマッピング」
# しか使わないので、YAML パーサを外部依存として持ち込まず、その部分集合を awk で
# 厳格に検証する（ネスト・複数行スカラー・ブロックシーケンスは書けない）。狙いは
# 「パースは通るが値が壊れる」型の事故を落とすこと:
#   - 引用符なしスカラー中の ` #` 以降がコメントとして捨てられる（PR #303）
#   - 引用符なしスカラー中の `: ` で YAML がマッピング解釈に入り失敗する
#   - 値の欠落・キーの重複・引用符やフロー表記の閉じ忘れ・タブ
#
# リンク切れ検査は lychee に委譲（インストール済みの場合のみ / --no-links でスキップ）:
#   lychee --offline --root-dir <bundle> <bundle>
#
# 使い方: scripts/okf-lint.sh [bundleDir=docs] [--strict] [--no-links]
#   --strict   : リンク切れ(lychee)もエラー扱いにする（既定は警告）
# 終了コード: エラー1件以上で 1
set -u

BUNDLE="docs"
STRICT=0
NO_LINKS=0
for a in "$@"; do
  case "$a" in
  --strict) STRICT=1 ;;
  --no-links) NO_LINKS=1 ;;
  --*)
    echo "unknown option: $a" >&2
    exit 2
    ;;
  *) BUNDLE="$a" ;;
  esac
done

[ -d "$BUNDLE" ] || {
  echo "okf-lint: bundle directory not found: $BUNDLE" >&2
  exit 1
}
# 末尾スラッシュ除去
BUNDLE="${BUNDLE%/}"

# awk の出力を `IFS` で割るための実タブ（bash 3.2 には $'\t' があるが、
# 意図を明示するために 1 箇所に置く）
TAB="$(printf '\t')"

ERRORS=0
WARNINGS=0
error() {
  echo "ERROR [$1] $2: $3"
  ERRORS=$((ERRORS + 1))
}
warn() {
  echo "WARN  [$1] $2: $3"
  WARNINGS=$((WARNINGS + 1))
}

# index への掲載を要求しないメタファイル
is_exempt() { case "$1" in README.md | CLAUDE.md) return 0 ;; *) return 1 ;; esac }
is_reserved() { case "$1" in index.md | log.md) return 0 ;; *) return 1 ;; esac }

# frontmatter 判定: 1行目が --- で、2行目以降に閉じ --- があるか (0=あり)
has_frontmatter() {
  awk 'NR==1 { if ($0 != "---") { exit } ; next }
       $0 == "---" { ok=1; exit }
       END { exit (ok ? 0 : 1) }' "$1"
}

# frontmatter ブロック内に空でない type: があるか (0=あり)
has_type() {
  # frontmatter ブロック（1行目---から次の---まで）を切り出して type: を探す
  awk 'NR==1 && $0=="---" {fm=1; next}
       fm && $0=="---" {exit}
       fm {print}' "$1" | grep -Eq '^type:[[:space:]]*[^[:space:]]'
}

# frontmatter を「1 行 1 キーの平坦なマッピング」として厳格に検証する。
# 第2引数が 1 のとき `description` の存在も要求する（index.md / log.md は 0）。
# 出力: 問題ごとに "<check>\t<detail>" を 1 行。問題なしなら無出力。
fm_lint() {
  awk -v need_desc="$2" '
    NR==1 { if ($0 != "---") exit; fm=1; next }
    fm && $0=="---" { seen_end=1; exit }
    fm {
      if (index($0, "\t") > 0) { print "fm-yaml\t" NR " 行目: タブ文字は YAML で使えない"; next }
      if ($0 ~ /^[ ]*$/) next
      if ($0 ~ /^#/) next
      if ($0 ~ /^[ ]/) { print "fm-yaml\t" NR " 行目: 行頭にインデントがある（frontmatter は 1 行 1 キーの平坦なマッピングに限る）"; next }
      if ($0 !~ /^[A-Za-z_][A-Za-z0-9_-]*:/) { print "fm-yaml\t" NR " 行目: `key: value` の形になっていない"; next }

      c = index($0, ":")
      key = substr($0, 1, c-1)
      rest = substr($0, c+1)
      if (rest != "" && substr(rest, 1, 1) != " ") {
        print "fm-yaml\t" NR " 行目: `" key ":` の後に半角スペースが要る"; next
      }
      if (key in keys) {
        print "fm-yaml\t" NR " 行目: キー `" key "` が重複している（後の値で上書きされる）"; next
      }
      keys[key] = 1

      val = rest
      sub(/^[ ]+/, "", val); sub(/[ ]+$/, "", val)
      if (val == "") { print "fm-yaml\t" NR " 行目: `" key "` の値が空（YAML では null になる）"; next }

      head = substr(val, 1, 1)
      last = substr(val, length(val), 1)

      if (head == "\"" || head == "'"'"'") {
        if (length(val) < 2 || last != head)
          print "fm-yaml\t" NR " 行目: `" key "` の引用符が閉じていない"
        next
      }
      if (head == "[" || head == "{") {
        close_ch = (head == "[") ? "]" : "}"
        if (last != close_ch)
          print "fm-yaml\t" NR " 行目: `" key "` のフロー表記 `" head "` が `" close_ch "` で閉じていない"
        else if (index(val, " #") > 0)
          print "fm-yaml\t" NR " 行目: `" key "` の値にコメント開始 ` #` がある（`#` を含む語は引用符で囲む）"
        next
      }

      # 引用符なしの平文スカラー
      if (head == "#")
        print "fm-yaml\t" NR " 行目: `" key "` の値が `#` で始まり全体がコメントとして捨てられる（引用符で囲む）"
      else if (index(val, " #") > 0)
        print "fm-yaml\t" NR " 行目: `" key "` の値の ` #` 以降が YAML コメントとして捨てられる（引用符で囲む）"
      else if (index(val, ": ") > 0 || last == ":")
        print "fm-yaml\t" NR " 行目: `" key "` の平文スカラーに `: ` があり YAML がマッピングとして解釈して失敗する（引用符で囲む）"
      else if (head=="&" || head=="*" || head=="!" || head=="|" || head==">" || head=="%" || head=="@" || head=="`" || head==",")
        print "fm-yaml\t" NR " 行目: `" key "` の値が YAML の指示文字 `" head "` で始まる（引用符で囲む）"
      # `-` と `?` は「直後に空白があるとき」だけ指示文字（ブロックシーケンス
      # 項目 / 明示キー）になる。`-1` や `?foo` は正しい平文スカラーなので、
      # 上の一覧に混ぜると誤検出になる。
      else if ((head=="-" || head=="?") && (length(val)==1 || substr(val, 2, 1) == " "))
        print "fm-yaml\t" NR " 行目: `" key "` の値が `" head " ` で始まりブロック構造として解釈される（引用符で囲む）"
    }
    # 終端 `---` の欠落は pass1 では has_frontmatter が先に [frontmatter] で
    # 落とすが、pass2 の index-desc ガードは has_frontmatter を通さずここを
    # 直接呼ぶ。自己完結させておかないと、本文を frontmatter と誤読したまま
    # 「壊れていない」と判定してしまう。
    END {
      if (fm && !seen_end) {
        print "fm-yaml\tfrontmatter の終端 `---` がない（本文が frontmatter として読まれる）"
      } else if (fm && need_desc == 1 && !("description" in keys)) {
        print "description\tfrontmatter に空でない `description` がない（index 生成に使うため必須 / docs/CLAUDE.md）"
      }
    }
  ' "$1"
}

# frontmatter から 1 キーの値を取り出す（引用符は外す）。無ければ無出力。
fm_value() {
  awk -v want="$2" '
    NR==1 { if ($0 != "---") exit; fm=1; next }
    fm && $0=="---" { exit }
    fm {
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

# index.md の 1 エントリから「リンク先」と「転記された description」を取り出す。
# 形式は docs/CLAUDE.md の `* [Title](file.md) - description`。
# 出力: "<リンク先>\t<description>" を 1 行ずつ。
index_entries() {
  awk '
    BEGIN { code=0; comment=0 }
    /^```/ { code = 1 - code; next }
    code { next }
    /<!--/ { comment=1 }
    comment { if ($0 ~ /-->/) comment=0; next }
    {
      line = $0
      sub(/^[ ]*[*-][ ]+/, "", line)
      if (line !~ /^\[/) next
      p = index(line, "](")
      if (p == 0) next
      rest = substr(line, p+2)
      q = index(rest, ")")
      if (q == 0) next
      link = substr(rest, 1, q-1)
      desc = substr(rest, q+1)
      if (substr(desc, 1, 3) != " - ") next
      desc = substr(desc, 4)
      sub(/[ ]+$/, "", desc)
      print link "\t" desc
    }
  ' "$1"
}

# 本文からバンドル内リンクのターゲットを抽出（コードブロック・HTMLコメント・インラインコードを除去）
extract_links() {
  awk 'BEGIN{code=0}
       /^```/{code=1-code; next}
       !code {print}' "$1" |
    sed -e 's/<!--.*-->//g' -e 's/`[^`]*`//g' |
    grep -oE '\]\([^) ]+\)' |
    sed -e 's/^](//' -e 's/)$//' -e 's/[#?].*$//' |
    grep -vE '^[a-zA-Z][a-zA-Z0-9+.-]*:' || true # http:, mailto: 等の外部URLを除外
}

# ---------- 1) frontmatter / type / index-fm / log-format ----------
find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -type f -name '*.md' -print |
  while IFS= read -r file; do
    rel="${file#"$BUNDLE"/}"
    base="$(basename "$file")"

    if is_reserved "$base"; then
      if [ "$base" = "index.md" ] && [ "$rel" != "index.md" ]; then
        if has_frontmatter "$file"; then
          error "index-fm" "$rel" "ルート以外の index.md に frontmatter は書けない (SPEC §6/§11)"
        fi
      fi
      # ルート index.md は okf_version 宣言だけを持つ予約ファイル。description は
      # 要らないが、YAML として壊れていないことは他と同じく担保する。
      if [ "$rel" = "index.md" ] && has_frontmatter "$file"; then
        fm_lint "$file" 0 | while IFS="$TAB" read -r chk detail; do
          error "$chk" "$rel" "$detail"
        done
      fi
      if [ "$base" = "log.md" ]; then
        grep -E '^## ' "$file" | grep -vE '^## [0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]*$' |
          while IFS= read -r bad; do
            error "log-format" "$rel" "日付見出しが ISO 8601 (## YYYY-MM-DD) でない: \"$bad\""
          done
      fi
      continue
    fi

    if ! has_frontmatter "$file"; then
      error "frontmatter" "$rel" "YAML frontmatter がない（--- で囲んだブロックを先頭に置く）"
      continue
    fi
    if ! has_type "$file"; then
      error "type" "$rel" "frontmatter に空でない \`type\` がない (SPEC §4.1 REQUIRED)"
    fi
    fm_lint "$file" 1 | while IFS="$TAB" read -r chk detail; do
      error "$chk" "$rel" "$detail"
    done
  done >/tmp/okf-lint-pass1.$$
# サブシェル内のカウンタは失われるため、出力行から集計し直す
cat /tmp/okf-lint-pass1.$$

# ---------- 2) index-exists / index-listed ----------
find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -type d -print |
  while IFS= read -r dir; do
    reldir="${dir#"$BUNDLE"}"
    reldir="${reldir#/}"

    # このディレクトリ直下の concept と、.md を含むサブディレクトリを収集
    concepts=""
    subdirs=""
    for entry in "$dir"/*; do
      [ -e "$entry" ] || continue
      name="$(basename "$entry")"
      case "$name" in .* | node_modules) continue ;; esac
      if [ -f "$entry" ]; then
        case "$name" in *.md) ;; *) continue ;; esac
        is_reserved "$name" && continue
        is_exempt "$name" && continue
        concepts="$concepts $name"
      elif [ -d "$entry" ]; then
        if find "$entry" -type f -name '*.md' -print -quit | grep -q .; then
          subdirs="$subdirs $name"
        fi
      fi
    done
    [ -z "$concepts$subdirs" ] && continue

    idx="$dir/index.md"
    relidx="${idx#"$BUNDLE"/}"
    if [ ! -f "$idx" ]; then
      error "index-exists" "${reldir:-（root）}" "index.md がない（progressive disclosure が途切れる）"
      continue
    fi

    # index.md 内のリンクをバンドル相対に正規化した一覧を作る
    links="$(extract_links "$idx" | while IFS= read -r t; do
      case "$t" in
      /*) echo "${t#/}" ;;
      ./*)
        t="${t#./}"
        [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t"
        ;;
      *) [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t" ;;
      esac
    done | sed 's|/$||')"

    # index.md の各エントリの「転記された description」を、リンク先を鍵に引けるようにする
    entries="$(index_entries "$idx" | while IFS="$TAB" read -r t d; do
      case "$t" in
      /*) t="${t#/}" ;;
      ./*)
        t="${t#./}"
        [ -n "$reldir" ] && t="$reldir/$t"
        ;;
      *) [ -n "$reldir" ] && t="$reldir/$t" ;;
      esac
      printf '%s\t%s\n' "$t" "$d"
    done)"

    for c in $concepts; do
      target="${reldir:+$reldir/}$c"
      if echo "$links" | grep -qxF "$target"; then
        # index の転記は frontmatter の description の全文でなければならない。
        # ずれたまま放置されると okf-search の結果と index の説明が食い違う。
        # frontmatter 自体が壊れている場合は、そちらを直すのが先なので比較しない
        # （壊れた値を転記先として提示すると誤誘導になる）。
        if [ -n "$(fm_lint "$dir/$c" 1)" ]; then
          continue
        fi
        want="$(fm_value "$dir/$c" description)"
        got="$(printf '%s\n' "$entries" |
          awk -v k="$target" 'BEGIN{FS="\t"} $1==k {print substr($0, index($0,"\t")+1); exit}')"
        if [ -z "$got" ]; then
          error "index-desc" "$relidx" "$c のエントリが \`* [Title]($c) - <description>\` の形になっていない"
        elif [ -n "$want" ] && [ "$got" != "$want" ]; then
          error "index-desc" "$relidx" "$c の転記が frontmatter の description と一致しない
    index      : $got
    frontmatter: $want"
        fi
      else
        error "index-listed" "$relidx" "concept が未掲載: $c"
      fi
    done
    for s in $subdirs; do
      target="${reldir:+$reldir/}$s"
      echo "$links" | grep -qxF "$target" ||
        echo "$links" | grep -qxF "$target/index.md" ||
        error "index-listed" "$relidx" "サブディレクトリが未掲載: $s/"
    done
  done >/tmp/okf-lint-pass2.$$
cat /tmp/okf-lint-pass2.$$

ERRORS=$(cat /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$ | grep -c '^ERROR' || true)
WARNINGS=$(cat /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$ | grep -c '^WARN' || true)
rm -f /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$

# ---------- 3) リンク切れ（lychee に委譲）----------
if [ "$NO_LINKS" -eq 0 ]; then
  if command -v lychee >/dev/null 2>&1; then
    # --offline: ネットワークに出ない（外部URLは対象外、ファイルリンクのみ検査）
    # --root-dir: /path/file.md 形式のバンドルルート相対リンクを解決
    if ! lychee --offline --no-progress --root-dir "$(cd "$BUNDLE" && pwd)" "$BUNDLE"; then
      if [ "$STRICT" -eq 1 ]; then
        echo "ERROR [link] リンク切れを検出（--strict のためエラー扱い）"
        ERRORS=$((ERRORS + 1))
      else
        echo "WARN  [link] リンク切れあり（未執筆 concept なら許容可 / SPEC §5.3。--strict でエラー化）"
        WARNINGS=$((WARNINGS + 1))
      fi
    fi
  else
    echo "note: lychee 未インストールのためリンク検査をスキップ (brew install lychee / --no-links で明示スキップ)"
  fi
fi

echo ""
echo "okf-lint: ${ERRORS} error(s), ${WARNINGS} warning(s)$([ "$STRICT" -eq 1 ] && echo ' (--strict)')"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
