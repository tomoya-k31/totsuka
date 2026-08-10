#!/usr/bin/env bash
# GitHub 側の駆動と観測。人間の関与ゼロで完結する。
#
#   bash .claude/skills/live-e2e/scripts/github.sh bootstrap        # サンドボックス repo 2 つ + Project + seed Issue
#   bash .claude/skills/live-e2e/scripts/github.sh status           # Project の item と Status 一覧
#   bash .claude/skills/live-e2e/scripts/github.sh seed <web|cli> <issue#> [Status]  # 既定 Todo。design を試すなら Design
#
# 注意: Project #7 は新規 item を自動で Todo にする。design を回したいときは
# item-add 後すぐ（poll_interval_secs=15 より早く）Design へ倒すこと。遅れると
# github-task が先に拾って implement が走る。
#   bash .claude/skills/live-e2e/scripts/github.sh clear <web|cli> <issue#>  # Status を外す
#   bash .claude/skills/live-e2e/scripts/github.sh wait [sec]       # github タスクが終端に達するまで追う
#   bash .claude/skills/live-e2e/scripts/github.sh verify <web|cli> <issue#> # F-07/F-84/F-86 を判定
#
# GraphQL のレートに注意（実測 2026-08-11）。`gh project` 系は 1 リクエスト 1 ポイント
# ではない:
#
#   item-list --limit 30   →  31 points
#   item-list --limit 100+ → 102 points   （gh が 100 ノードのページを要求するため、
#                                           100 を超えて指定しても同じ）
#   field-list             → 102 points
#   → set_status 1 回 = item_id + project view + field_ids ≒ 200 points
#
# 上限は 5000 points/時。**この中の関数をリトライループで回さないこと** — 40 回回すと
# 8000 points で使い切る（実際にやって 40 分止まった）。伝播待ちが要るなら
# `item-add --format json` が返す item id を使い、item-list を引き直さない。
#
# `--limit 100` は打ち切り回避のため。既定の 30 だと Project が 30 件を超えた時点で
# 新しい item を**黙って**見落とす。正しさに 31 → 102 points 払っている。
set -euo pipefail
# `tt` はシェル関数なので子プロセスには継承されない。共通定義を読む。
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"
: "${E2E_GH_OWNER:?source .env してください}"
OWNER="$E2E_GH_OWNER"
WEB="${E2E_GH_REPO_WEB:-totsuka-sandbox-web}"
CLI="${E2E_GH_REPO_CLI:-totsuka-sandbox-cli}"
repo_of() { case "$1" in web) echo "$WEB";; cli) echo "$CLI";; *) echo "$1";; esac; }

field_ids() {  # Status フィールドと option の id を出す
  gh project field-list "${E2E_GH_PROJECT}" --owner "$OWNER" --format json | python3 -c '
import json,sys
f=[x for x in json.load(sys.stdin)["fields"] if x["name"]=="Status"][0]
print(f["id"])
for o in f["options"]: print("%s\t%s" % (o["name"], o["id"]))'
}

item_id() {  # item_id <repo> <issue#>
  gh project item-list "${E2E_GH_PROJECT}" --owner "$OWNER" --limit 100 --format json | python3 -c '
import json,sys
repo,num=sys.argv[1],int(sys.argv[2])
items=json.load(sys.stdin)["items"]
for i in items:
    c=i.get("content",{})
    if c.get("repository","").split("/")[-1]==repo and c.get("number")==num:
        print(i["id"]); break
else:
    if len(items) >= 100:
        sys.stderr.write("warn: item-list が 100 件返した。gh は 100 ノードのページしか要求しないので\n"
                         "      --limit を上げても増えない。目的の item を id で直接掴むこと\n"
                         "      （item-add --format json が返す id を使う）\n")' "$1" "$2"
}

set_status() {  # set_status <repo> <issue#> <option-name|-->
  local iid; iid="$(item_id "$1" "$2")"
  [ -n "$iid" ] || { echo "item が見つかりません: $1#$2" >&2; exit 1; }
  local pid; pid="$(gh project view "${E2E_GH_PROJECT}" --owner "$OWNER" --format json \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  local ids fid oid; ids="$(field_ids)"; fid="$(echo "$ids" | head -1)"
  if [ "$3" = "--" ]; then
    gh project item-edit --id "$iid" --project-id "$pid" --field-id "$fid" --clear >/dev/null
  else
    # 完全一致。前方一致だと `Design` が `Design Review` にも当たり、
    # option id が 2 つ出て GraphQL が "option Id does not belong to the field" で落ちる。
    oid="$(echo "$ids" | awk -F'\t' -v n="$3" '$1 == n {print $2}')"
    # 空のまま item-edit へ渡すと GraphQL が
    # "single select option Id does not belong to the field" で落ち、
    # 「名前を間違えた」のか「別の不整合」なのか読み取れない。ここで落とす。
    if [ -z "$oid" ]; then
      { echo "Status option \`$3\` は存在しません。候補:";
        echo "$ids" | tail -n +2 | awk -F'\t' '{print "  - " $1}'; } >&2
      exit 1
    fi
    gh project item-edit --id "$iid" --project-id "$pid" --field-id "$fid" \
        --single-select-option-id "$oid" >/dev/null
  fi
}

cmd="${1:-help}"; shift || true
case "$cmd" in
bootstrap)
  bash "$(dirname "${BASH_SOURCE[0]}")/bootstrap-github.sh"
  ;;
status)
  gh project item-list "${E2E_GH_PROJECT}" --owner "$OWNER" --limit 100 --format json | python3 -c '
import json,sys
for i in json.load(sys.stdin)["items"]:
    c=i.get("content",{})
    print("%-24s #%-3s %-10s %s" % (c.get("repository","?").split("/")[-1], c.get("number"),
          i.get("status","(none)"), (c.get("title") or "")[:44]))'
  ;;
seed)  st="${3:-Todo}"; set_status "$(repo_of "$1")" "$2" "$st"; echo "$(repo_of "$1")#$2 → $st";;
clear) set_status "$(repo_of "$1")" "$2" --;  echo "$(repo_of "$1")#$2 → (none)";;
wait)
  limit="${1:-1800}"; deadline=$(( $(date +%s) + limit ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    line=$(tt task list 2>/dev/null | grep ' github ' | head -1 || true)
    echo "$(date +%H:%M:%S) ${line:-（github タスクなし）}"
    case "$line" in *done*|*failed*|*escalated*|*waiting*) exit 0;; esac
    sleep 20
  done
  echo "タイムアウト（${limit}s）"; exit 1
  ;;
verify)
  r="$(repo_of "$1")"; n="$2"; ok=0; ng=0
  chk() {  # chk <0|1> <説明>
    if [ "$1" = 1 ]; then echo "  [ok]   $2"; ok=$((ok+1))
    else echo "  [FAIL] $2"; ng=$((ng+1)); fi
  }
  echo "== $r#$n =="

  status="$(gh project item-list "${E2E_GH_PROJECT}" --owner "$OWNER" --limit 100 --format json \
    | python3 -c 'import json,sys
repo, num = sys.argv[1], int(sys.argv[2])
for i in json.load(sys.stdin)["items"]:
    c = i.get("content", {})
    if c.get("repository", "").split("/")[-1] == repo and c.get("number") == num:
        print(i.get("status", "(none)")); break' "$r" "$n")"
  if [ "$status" = "Done" ]; then chk 1 "F-84 Project の Status が Done"
  else chk 0 "F-84 Project の Status が Done（実際: ${status:-不明}）"; fi

  comments="$(gh issue view "$n" -R "$OWNER/$r" --json comments --jq '.comments | length')"
  if [ "$comments" -ge 1 ]; then chk 1 "F-07 Issue に成果物コメント（$comments 件）"
  else chk 0 "F-07 Issue に成果物コメントが無い"; fi

  # main 以外のブランチが push されていれば、エージェントが push まで到達している
  branches="$(gh api "repos/$OWNER/$r/branches" --jq '[.[].name] | length')"
  if [ "$branches" -ge 2 ]; then chk 1 "F-86 ブランチが push された（計 $branches 本）"
  else chk 0 "F-86 ブランチが push されていない（main のみ）"; fi

  prs="$(gh pr list -R "$OWNER/$r" --state all --json number --jq 'length')"
  if [ "$prs" -ge 1 ]; then chk 1 "ADR-0026 PR が作られた（計 $prs 本）"
  else chk 0 "ADR-0026 PR が作られていない"; fi

  echo "  -- ok=$ok ng=$ng"
  [ "$ng" -eq 0 ]
  ;;
*)
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  ;;
esac
