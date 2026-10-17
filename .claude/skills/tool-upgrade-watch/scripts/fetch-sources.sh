#!/usr/bin/env bash
# fetch-sources.sh — Claude Code / Codex / opencode の更新情報を 1 か所に落とす。
#
# 使い方: bash fetch-sources.sh <out_dir>
#
# 取得物（<out_dir>/<tool>/ 配下）:
#   versions.txt      ローカル版・最新 stable 版・直近の公開日時
#   changelog.md      リリースノート（直近ぶん）
#   docs-*.md         hooks / settings / CLI / plugins などの現行ドキュメント
# 取れなかったものは <out_dir>/FAILED.txt に 1 行ずつ残し、処理は続ける
# （1 ソースの障害で全体の判定を止めない。判定側が FAILED を読んで欠けを報告する）。
#
# URL の出所と注意点は ../references/sources.md。URL を変えたらそちらも直すこと。
set -uo pipefail

out="${1:?usage: fetch-sources.sh <out_dir>}"
mkdir -p "$out"/{claude,codex,opencode}
: >"$out/FAILED.txt"

fail() { printf '%s\n' "$1" >>"$out/FAILED.txt"; }

# curl -L: Codex の docs は developers.openai.com → learn.chatgpt.com へ移転済みで、
# 今後も動きうるのでリダイレクトは必ず追う。
get() { # $1=url $2=dest
  curl -fsSL --max-time 30 "$1" -o "$2" 2>/dev/null || fail "$1"
}

local_version() { # $1=cmd
  if command -v "$1" >/dev/null 2>&1; then
    "$1" --version 2>/dev/null | head -n 1
  else
    echo "not installed"
  fi
}

# ---- Claude Code ------------------------------------------------------------
c="$out/claude"
{
  echo "local:  $(local_version claude)"
  echo "dist-tags: $(npm view @anthropic-ai/claude-code dist-tags --json 2>/dev/null | tr -d '\n ')"
} >"$c/versions.txt"
get https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md "$c/changelog.md"
get https://code.claude.com/docs/llms.txt "$c/llms.txt"
for page in hooks hooks-guide settings cli-reference plugins-reference permissions permission-modes sessions; do
  get "https://code.claude.com/docs/en/$page.md" "$c/docs-$page.md"
done

# ---- Codex ------------------------------------------------------------------
x="$out/codex"
{
  echo "local:  $(local_version codex)"
  echo "npm version: $(npm view @openai/codex version 2>/dev/null)"
} >"$x/versions.txt"
# 正典は GitHub Releases。alpha の prerelease が 1 日に何本も出て本文は空に近いので落とす。
# 1 ページ 100 件のうち stable は 10 本前後しかないので、4 ページ（≒ 台帳の基準版 0.145.0 まで）遡る。
: >"$x/changelog.md"
for pg in 1 2 3 4; do
  gh api "repos/openai/codex/releases?per_page=100&page=$pg" \
    --jq '[.[] | select(.prerelease == false)] | .[] | "## \(.tag_name) (\(.published_at))\n\n\(.body)\n"' \
    >>"$x/changelog.md" 2>/dev/null || fail "gh api repos/openai/codex/releases page=$pg"
done
# main の CHANGELOG.md は `## Unreleased` だけが中身（次版の予告として読む）。
get https://raw.githubusercontent.com/openai/codex/main/CHANGELOG.md "$x/unreleased.md"
get https://learn.chatgpt.com/docs/llms.txt "$x/llms.txt"
get https://learn.chatgpt.com/docs/hooks.md "$x/docs-hooks.md"
get https://learn.chatgpt.com/docs/config-file/config-reference.md "$x/docs-config-reference.md"
get 'https://learn.chatgpt.com/docs/developer-commands.md?surface=cli' "$x/docs-cli.md"
# `-c` 上書きと設定層の優先順位はこの 2 ページにしか書かれていない。
get https://learn.chatgpt.com/docs/config-file/config-basic.md "$x/docs-config-basic.md"
get https://learn.chatgpt.com/docs/config-file/config-advanced.md "$x/docs-config-advanced.md"

# ---- opencode ---------------------------------------------------------------
# OpenCode 1（branch dev / npm opencode-ai / GitHub Releases あり）と
# OpenCode 2（branch 2.0 / npm @opencode/cli / Releases 無し・tag のみ）が並走している。
# ローカルに入っている方の系列を見る（入っていなければ両方）。
o="$out/opencode"
lv="$(local_version opencode)"
{
  echo "local:  $lv"
  echo "v1 latest (opencode-ai):   $(npm view opencode-ai version 2>/dev/null)"
  echo "v2 latest (@opencode/cli): $(npm view @opencode/cli version 2>/dev/null)"
} >"$o/versions.txt"
# v1 の docs は dev branch の .mdx が正典。v2 は 2.0 branch の .mdx が更新されておらず v1 の API の
# ままなので（session.idle 等、v2 に無いイベントが載っている）、v2 は opencode.ai/v2/docs の HTML を読む。
case "$lv" in
1.* | *v1.* | *" 1."*) tracks="v1" ;;
2.* | *v2.* | *" 2."*) tracks="v2" ;;
*) tracks="v1 v2" ;;
esac
for t in $tracks; do
  for page in plugins config cli agents permissions; do
    if [ "$t" = v1 ]; then
      get "https://raw.githubusercontent.com/anomalyco/opencode/dev/packages/web/src/content/docs/$page.mdx" \
        "$o/docs-v1-$page.mdx"
    else
      get "https://opencode.ai/v2/docs/$page" "$o/docs-v2-$page.html"
    fi
  done
done
if gh api 'repos/anomalyco/opencode/releases?per_page=30' \
  --jq '.[] | "## \(.tag_name) (\(.published_at))\n\n\(.body)\n"' \
  >"$o/changelog-v1.md" 2>/dev/null; then :; else fail "gh api repos/anomalyco/opencode/releases"; fi
# v2 系は GitHub Releases が無い。HTML の changelog を落としておき、判定側が本文を読む。
get https://opencode.ai/changelog "$o/changelog.html"

echo "fetched into $out"
if [ -s "$out/FAILED.txt" ]; then
  echo "FAILED sources:"
  cat "$out/FAILED.txt"
fi
exit 0
