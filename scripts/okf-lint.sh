#!/usr/bin/env bash
# okf-lint.sh — OKF v0.1 バンドル検証（依存: bash 3.2+, POSIX awk/grep/sed のみ）
#
# チェック内容:
#   [E] frontmatter : index.md / log.md 以外の全 .md が YAML frontmatter を持つ
#   [E] type        : frontmatter に空でない `type` がある (SPEC §4.1 / §9)
#   [E] index-fm    : ルート以外の index.md が frontmatter を持たない (SPEC §6/§11)
#   [E] index-exists: concept/サブディレクトリを含むディレクトリに index.md がある
#   [E] index-listed: 各 concept / サブディレクトリが index.md からリンクされている
#   [E] log-format  : log.md の日付見出しが `## YYYY-MM-DD` 形式
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
    --*) echo "unknown option: $a" >&2; exit 2 ;;
    *) BUNDLE="$a" ;;
  esac
done

[ -d "$BUNDLE" ] || { echo "okf-lint: bundle directory not found: $BUNDLE" >&2; exit 1; }
# 末尾スラッシュ除去
BUNDLE="${BUNDLE%/}"

ERRORS=0
WARNINGS=0
error() { echo "ERROR [$1] $2: $3"; ERRORS=$((ERRORS + 1)); }
warn()  { echo "WARN  [$1] $2: $3"; WARNINGS=$((WARNINGS + 1)); }

# index への掲載を要求しないメタファイル
is_exempt() { case "$1" in README.md|CLAUDE.md) return 0 ;; *) return 1 ;; esac; }
is_reserved() { case "$1" in index.md|log.md) return 0 ;; *) return 1 ;; esac; }

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

# 本文からバンドル内リンクのターゲットを抽出（コードブロック・HTMLコメント・インラインコードを除去）
extract_links() {
  awk 'BEGIN{code=0}
       /^```/{code=1-code; next}
       !code {print}' "$1" \
    | sed -e 's/<!--.*-->//g' -e 's/`[^`]*`//g' \
    | grep -oE '\]\([^) ]+\)' \
    | sed -e 's/^](//' -e 's/)$//' -e 's/[#?].*$//' \
    | grep -vE '^[a-zA-Z][a-zA-Z0-9+.-]*:' || true   # http:, mailto: 等の外部URLを除外
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
  elif ! has_type "$file"; then
    error "type" "$rel" "frontmatter に空でない \`type\` がない (SPEC §4.1 REQUIRED)"
  fi
done > /tmp/okf-lint-pass1.$$
# サブシェル内のカウンタは失われるため、出力行から集計し直す
cat /tmp/okf-lint-pass1.$$

# ---------- 2) index-exists / index-listed ----------
find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -type d -print |
while IFS= read -r dir; do
  reldir="${dir#"$BUNDLE"}"; reldir="${reldir#/}"

  # このディレクトリ直下の concept と、.md を含むサブディレクトリを収集
  concepts=""
  subdirs=""
  for entry in "$dir"/*; do
    [ -e "$entry" ] || continue
    name="$(basename "$entry")"
    case "$name" in .*|node_modules) continue ;; esac
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
      ./*) t="${t#./}"; [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t" ;;
      *)  [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t" ;;
    esac
  done | sed 's|/$||')"

  for c in $concepts; do
    target="${reldir:+$reldir/}$c"
    echo "$links" | grep -qxF "$target" ||
      error "index-listed" "$relidx" "concept が未掲載: $c"
  done
  for s in $subdirs; do
    target="${reldir:+$reldir/}$s"
    echo "$links" | grep -qxF "$target" ||
    echo "$links" | grep -qxF "$target/index.md" ||
      error "index-listed" "$relidx" "サブディレクトリが未掲載: $s/"
  done
done > /tmp/okf-lint-pass2.$$
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
