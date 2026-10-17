#!/usr/bin/env bash
# check-anchors.sh — 台帳の各項目のアンカーが今のコードに残っているかを調べる。
#
# 使い方: bash check-anchors.sh   （リポジトリのどこからでも）
#
# 台帳 ../references/replaceable-logic.md の `### <id>` 見出しと
# `- アンカー: \`<文字列>\` — \`<path>\`、…` 行を読み、各組を `git grep -n -F` で引く。
# 出力は 1 組 1 行: `OK|STALE <id> <path>:<line> <文字列>`。
# STALE は「コード側が既に変わった」印 — 台帳を直す（判定はしない）。
# shellcheck disable=SC2016  # バッククォートは台帳の区切り文字そのもの（展開させない）
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ledger="$here/../references/replaceable-logic.md"
root="$(git -C "$here" rev-parse --show-toplevel)"

id=""
stale=0
while IFS= read -r line; do
  case "$line" in
  '### '*) id="${line#'### '}" ;;
  '- アンカー: '*)
    # 「`文字列` — `path`」の組を 1 つずつ取り出す。
    while IFS= read -r pair; do
      s="$(printf '%s' "$pair" | sed -E 's/^`([^`]+)` — `([^`]+)`$/\1/')"
      p="$(printf '%s' "$pair" | sed -E 's/^`([^`]+)` — `([^`]+)`$/\2/')"
      p="${p/#core\//crates/orchestrator-core/src/}"
      hit="$(git -C "$root" grep -n -F -- "$s" -- "$p" 2>/dev/null | head -n 1 | cut -d: -f1-2)"
      if [ -n "$hit" ]; then
        echo "OK    $id $hit $s"
      else
        echo "STALE $id $p $s"
        stale=$((stale + 1))
      fi
    done < <(printf '%s\n' "$line" | grep -oE '`[^`]+` — `[^`]+`')
    ;;
  esac
done <"$ledger"

echo "stale anchors: $stale"
exit 0
