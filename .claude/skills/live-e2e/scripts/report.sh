#!/usr/bin/env bash
# 検証結果を集める。自動で判定できるものは pass/fail を出し、目視項目は「未確認」として
# 列挙する。自動分だけを見て「全部通った」と書かないための仕切り。
#
#   bash .claude/skills/live-e2e/scripts/report.sh                  # 結果
#   bash .claude/skills/live-e2e/scripts/report.sh --cleanup-hints  # 後始末の対象を列挙
set -euo pipefail
# `tt` はシェル関数なので子プロセスには継承されない。共通定義を読む。
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"
: "${E2E_HOME:?source .env してください}"
OWNER="${E2E_GH_OWNER:-}"
WEB="${E2E_GH_REPO_WEB:-totsuka-sandbox-web}"
CLI="${E2E_GH_REPO_CLI:-totsuka-sandbox-cli}"

if [ "${1:-}" = "--cleanup-hints" ]; then
  echo "== 後始末の対象（既定は残す。消すときだけ手を出す）=="
  echo "-- worktree"
  for r in "$WEB" "$CLI"; do
    [ -d "$E2E_HOME/repo/$r" ] && git -C "$E2E_HOME/repo/$r" worktree list | tail -n +2
  done
  echo "-- herdr workspace（'~' は本人のもの。触らない）"
  herdr workspace list 2>/dev/null | python3 -c '
import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit()
for w in d["result"]["workspaces"]:
    print(" ", w["workspace_id"], w.get("label"))' || true
  echo "-- サンドボックスのブランチ / PR"
  for r in "$WEB" "$CLI"; do
    [ -n "$OWNER" ] || continue
    echo "  $r: $(gh api "repos/$OWNER/$r/branches" --jq '[.[].name]|join(", ")' 2>/dev/null)"
    gh pr list -R "$OWNER/$r" --state open --json number,headRefName \
      --jq '.[] | "    PR #\(.number) \(.headRefName)"' 2>/dev/null || true
  done
  echo
  echo "rm -rf \$E2E_HOME は最後の手段（トークン以外の全設定を作り直すことになる）"
  exit 0
fi

echo "== タスク =="
tt task list 2>/dev/null || echo "（state DB なし。まだ run していない）"

echo
echo "== GitHub 側（自動判定）=="
if [ -n "$OWNER" ] && [ -n "${E2E_GH_PROJECT:-}" ]; then
  gh project item-list "$E2E_GH_PROJECT" --owner "$OWNER" --format json | python3 -c '
import json,sys
for i in json.load(sys.stdin)["items"]:
    c=i.get("content",{})
    print("  %-24s #%-3s %-12s %s" % (c.get("repository","?").split("/")[-1], c.get("number"),
          i.get("status","(none)"), (c.get("title") or "")[:40]))'
  for r in "$WEB" "$CLI"; do
    b="$(gh api "repos/$OWNER/$r/branches" --jq '[.[].name]|length' 2>/dev/null || echo 0)"
    p="$(gh pr list -R "$OWNER/$r" --state all --json number --jq 'length' 2>/dev/null || echo 0)"
    echo "  $r: branches=$b  PRs=$p"
  done
else
  echo "  （E2E_GH_OWNER / E2E_GH_PROJECT 未設定）"
fi

echo
echo "== 目視でしか確認できない項目（人間に判定を返すこと）=="
cat <<'MSG'
  [ ] スレッド内エフェメラルの中身（返信案・承認/却下ボタン）
  [ ] 承認ボタンの confirm ダイアログ
  [ ] 押下後にエフェメラルが消える／✅ に変わる
  [ ] 二重押下で「処理済みです」が出る
  [ ] herdr の pane レイアウト（分割方向・比率）
  [ ] plan モードの設計プレビュー（F-34）
  [ ] macOS 通知センターへの通知（F-90 / F-92）
MSG

echo
echo "== 今回のスコープ外（報告に「未検証」と明記する）=="
cat <<'MSG'
  - waiting_input からの復帰（F-35 / F-44）
  - verification = "human" の検収（tt task verify）
  - session/attach による回復（§5.3）
  - click-to-focus（F-94）
  - orca（agent_ide のもう一方）
MSG
