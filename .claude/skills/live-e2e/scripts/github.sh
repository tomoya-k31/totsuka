#!/usr/bin/env bash
# GitHub 側の駆動と観測。人間の関与ゼロで完結する。
#
#   bash .claude/skills/live-e2e/scripts/github.sh bootstrap        # サンドボックス repo 2 つ + Project + seed Issue
#   bash .claude/skills/live-e2e/scripts/github.sh status           # Project の item と Status 一覧
#   bash .claude/skills/live-e2e/scripts/github.sh seed <web|cli> <issue#> [Status]  # 既定 Todo。design を試すなら Design
#
# `seed` の `<issue#>` は **issue 番号**であって seed の連番ではない。しかも
# **既に閉じている issue／Project に入っていない issue に打っても何も起きない**
# （前者は取り込まれず、後者は `item が見つかりません` で終わる）。使い回しの
# issue で回そうとして時間を溶かしたことがあるので、**新しい検証は毎回 issue を
# 作る**のが確実:
#
#   gh issue create --repo "$E2E_GH_OWNER/$E2E_GH_REPO_WEB" --title ... --body ...
#   gh project item-add "$E2E_GH_PROJECT" --owner "$E2E_GH_OWNER" --url <issue url>
#   bash .../github.sh seed web <新 issue#>
#
# 注意: Project #7 は新規 item を自動で Todo にする。design を回したいときは
# item-add 後すぐ（poll_interval_secs より早く。既定 60s）Design へ倒すこと。遅れると
# github-task が先に拾って implement が走る。
#   bash .claude/skills/live-e2e/scripts/github.sh clear <web|cli> <issue#>  # Status を外す
#   bash .claude/skills/live-e2e/scripts/github.sh wait <web|cli> <issue#> [sec]  # **その issue の**タスクが終端に達するまで追う
#   bash .claude/skills/live-e2e/scripts/github.sh verify <web|cli> <issue#> # F-84/F-86 を判定
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
# **不変な id はキャッシュしてある**（下記 `cached`）。実測で `set_status` 1 回は
# **212 → 1 point**（初回だけ 212、以降は 1）。キャッシュしているのは
# project id / Status の field・option id / item id の 3 つで、いずれも
# 「そのプロジェクト・その item が在り続ける限り変わらない」もの。
#
# 対して **poll は 2 points/回**（Project #7 / 62 items / 2 ページで実測）。
# つまり**削るべきは poll ではなくこちら側**で、poll を 60s から詰めても
# 得られる待ち時間に対してレートの対価が合わない。
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

# Project の id と Status フィールド/option の id は、**そのプロジェクトが存在する
# 限り変わらない**。それを毎回引き直しているのが e2e で一番高い操作だった:
#
#   field-list        → 102 points
#   project view      →   1 point
#   → set_status 1 回 ≒ 200 points（item-list 102 と合わせて）
#
# poll は実測 2 points/回なので、**`set_status` 1 回で poll 100 回分**を使っていた。
# 不変のものはディスクに残して、2 回目以降は 0 points で済ませる。
#
# 消したいときは `rm -rf "$E2E_HOME/state/live-e2e/cache"`。Project の Status
# option を編集したら消すこと（それ以外で古くなることは無い）。
cache_dir() { echo "${E2E_HOME}/state/live-e2e/cache"; }
cached() {  # cached <key> <コマンド...>  — 標準出力をキャッシュして返す
  local key="$1"; shift
  local f; f="$(cache_dir)/${key}"
  if [ -s "$f" ]; then cat "$f"; return 0; fi
  mkdir -p "$(cache_dir)"
  # **空をキャッシュしない。** 一時的な失敗を永続化すると、以降ずっと壊れる。
  local out; out="$("$@")" || return 1
  [ -n "$out" ] || return 1
  printf '%s\n' "$out" | tee "$f"
}

field_ids_uncached() {
  gh project field-list "${E2E_GH_PROJECT}" --owner "$OWNER" --format json | python3 -c '
import json,sys
f=[x for x in json.load(sys.stdin)["fields"] if x["name"]=="Status"][0]
print(f["id"])
for o in f["options"]: print("%s\t%s" % (o["name"], o["id"]))'
}
field_ids() { cached "fields-${E2E_GH_PROJECT}" field_ids_uncached; }

project_id_uncached() {
  gh project view "${E2E_GH_PROJECT}" --owner "$OWNER" --format json \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'
}
project_id() { cached "project-${E2E_GH_PROJECT}" project_id_uncached; }

# item の id も、その item が Project に**在り続ける限り**変わらない。
# `item-list --limit 100` は 102 points なので、ここも 2 回目以降は 0 にする。
#
# **Project から item を外して入れ直すと id は変わる。** そのときは
# `rm -rf "$E2E_HOME/state/live-e2e/cache"`。e2e の通常の流れ（検証ごとに
# 新しい issue を作る）では別のキーになるので当たらない。
item_id() { cached "item-${E2E_GH_PROJECT}-$1-$2" item_id_uncached "$1" "$2"; }

item_id_uncached() {  # item_id_uncached <repo> <issue#>
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
  local pid; pid="$(project_id)"
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
seed)
  st="${3:-Todo}"; r="$(repo_of "$1")"; n="$2"
  # **`wait` のための基準時刻を、Status を倒す「前」に残す。**
  #
  # これが無いと `wait` は「この issue のタスク」を正しく見つけたうえで、それが
  # **前回の実行の done タスク**でも即座に成功と報告する（使い回しの issue で
  # 必ず起きる）。
  #
  # 順序が重要で、`set_status` の**後**に書くと取りこぼす: `set_status` は
  # GraphQL を数往復するので数秒かかり、その間に poll（既定 15 秒間隔）が
  # 走ると、**本物のタスクの `updated_at` が基準より前**になって STALE 扱いに
  # なる。先に書けば基準は必ず取り込みより前になる — 誤って**古いほうへ倒れる**
  # ことはあっても、**新しいものを取りこぼすことはない**。
  mkdir -p "$E2E_HOME/state/live-e2e"
  date -u +%Y-%m-%dT%H:%M:%SZ > "$E2E_HOME/state/live-e2e/seed-$r-$n"
  set_status "$r" "$n" "$st"
  echo "$r#$n → $st"
  ;;
clear) set_status "$(repo_of "$1")" "$2" --;  echo "$(repo_of "$1")#$2 → (none)";;
wait)
  # **必ず「その issue のタスク」を待つ。**
  #
  # 以前はここが `tt task list | grep ' github ' | head -1` だった。一覧は新しい順
  # なので、**seed が何も生まなかったとき（閉じた issue に打った・Project に入って
  # いない等）に、前回の done タスクを掴んで即 exit 0 する**。実際に一度それで
  # 「PASS した」と誤認した（2026-08-23）。**観測が対象を取り違える形は、失敗より
  # たちが悪い** — 赤くならずに緑になる。
  #
  # 同一性は `source_task_id`（GitHub の issue node id）で取る。`tt task list --json`
  # がそのまま持っているので、突き合わせに追加の API 呼び出しは要らない。
  case "${1:-}" in web|cli) ;; *)
    echo "usage: github.sh wait <web|cli> <issue#> [sec]" >&2; exit 2;; esac
  r="$(repo_of "$1")"; n="${2:?issue 番号が要ります}"; limit="${3:-1800}"
  node="$(gh issue view "$n" --repo "$OWNER/$r" --json id --jq .id)"
  [ -n "$node" ] || { echo "issue $r#$n の node id を取得できません" >&2; exit 2; }
  # seed 時刻より後に動いたタスクだけを受け付ける。無ければ「基準なし」で走るが、
  # **前回の done を掴みうることを明示して警告する** — 黙って通すのが一番悪い。
  since_file="$E2E_HOME/state/live-e2e/seed-$r-$n"
  if [ -f "$since_file" ]; then
    since="$(cat "$since_file")"
    echo "待機対象: $r#$n ($node) / seed 以降のみ: $since"
  else
    since=""
    echo "待機対象: $r#$n ($node)"
    echo "  警告: seed の記録がありません。**前回の実行の done タスクを掴む可能性があります** —" >&2
    echo '        github.sh seed を通してから待つか、結果の updated_at を自分で確かめてください。' >&2
  fi
  deadline=$(( $(date +%s) + limit ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    line="$(tt task list --json 2>/dev/null | NODE="$node" SINCE="$since" python3 -c '
import json, os, sys
want, since = os.environ["NODE"], os.environ.get("SINCE", "")
data = json.load(sys.stdin)
tasks = data if isinstance(data, list) else data.get("tasks", [])
for t in tasks:
    if t.get("source_task_id") != want:
        continue
    # ISO 8601 の UTC 文字列どうしなので辞書順比較でよい。ただし totsuka は
    # 小数部を可変長で出すので、比較する前に日時部分だけに切り詰める。
    if since and (t.get("updated_at") or "")[:19] < since[:19]:
        print("STALE\t%s\t%s" % (t["id"], t["state"]))
        break
    print("%s\t%s\t%s" % (t["id"], t["state"], t["title"][:48]))
    break
' 2>/dev/null || true)"
    case "$line" in
      STALE*) echo "$(date +%H:%M:%S) （seed 前の古いタスクのみ: ${line#STALE	}）"; sleep 20; continue;;
    esac
    if [ -z "$line" ]; then
      echo "$(date +%H:%M:%S) （まだ取り込まれていません）"
    else
      echo "$(date +%H:%M:%S) task $line"
      case "$line" in *done*|*failed*|*escalated*|*waiting_input*) exit 0;; esac
    fi
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

  # F-07（書き戻し）はここでは判定しない。github プラグインは publish せず
  # （#398）、implement の指示文も「PR を作り URL を最終メッセージに含めよ」と
  # しか言わないので、Issue コメントは誰も付けない。書き戻しの検証は Slack の
  # シナリオ（承認 → 本人名義でスレッド返信）が担う。

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
  # 行番号で切らない。ヘッダを足すたびに usage が黙って途中で切れる
  # （実際にこの PR で 13 行目以降が出なくなった）。先頭の連続する
  # コメント行を、最初の非コメント行まで出す。
  awk 'NR==1 && /^#!/ {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
  ;;
esac
