#!/usr/bin/env bash
# Slack 側の駆動と観測。API でできることだけを担う。
#
#   bash scripts/slack.sh channels          # チャンネル一覧（ID を調べる）
#   bash scripts/slack.sh messages [n]      # 直近の投稿（bot_id 付きかも見える）
#   bash scripts/slack.sh react <ts>        # トリガー絵文字を付ける（A 名義）
#   bash scripts/slack.sh unreact <ts>      # 外す（付け直しで再トリガーしたいとき）
#   bash scripts/slack.sh draft             # self-DM とナッジ DM の下書き記録
#   bash scripts/slack.sh reply <ts>        # スレッドの返信（承認後の確認）
#   bash scripts/slack.sh watch [sec]       # slack タスクが終端に達するまで追う
#
# 「投稿」だけは意図的に無い。API 投稿には bot_id が付き、判定表①が必ず弾くため
# （user token でも同じ）。メンションもリアクション対象も、人間が手で打つ必要がある。
set -euo pipefail
: "${E2E_SLACK_A:?source .env してください}"
: "${E2E_SLACK_CHANNEL:?}"

api() {  # api <method> [curl args...]
  local m="$1"; shift
  curl -s -H "Authorization: Bearer $E2E_SLACK_A" "https://slack.com/api/$m" "$@"
}
jq_py() { python3 -c "$1"; }

cmd="${1:-help}"; shift || true
case "$cmd" in
channels)
  api "conversations.list?types=public_channel,private_channel&limit=200" | jq_py '
import json,sys
d=json.load(sys.stdin)
print("ok=",d.get("ok"), d.get("error") or "")
for c in d.get("channels",[]):
    print(c["id"], "private" if c.get("is_private") else "public ", "member=%s"%c.get("is_member"), c["name"])'
  ;;
messages)
  n="${1:-5}"
  api "conversations.history?channel=$E2E_SLACK_CHANNEL&limit=$n" | jq_py '
import json,sys
d=json.load(sys.stdin)
for m in d.get("messages",[]):
    # bot_id が付いた投稿はタスク化されない（判定表①）。ここで見分けられる。
    flag = "API投稿(起動しない)" if m.get("bot_id") or m.get("subtype") else "人間の投稿"
    print("%s  %-18s user=%s  %s" % (m.get("ts"), flag, m.get("user"), (m.get("text") or "").replace("\n"," ")[:70]))'
  ;;
react|unreact)
  ts="${1:?ts が要ります}"
  method=$([ "$cmd" = react ] && echo reactions.add || echo reactions.remove)
  api "$method" -X POST -d "channel=$E2E_SLACK_CHANNEL" -d "timestamp=$ts" \
      -d "name=${E2E_SLACK_EMOJI:-totsuka-test}" | jq_py '
import json,sys; d=json.load(sys.stdin); print("ok=",d.get("ok"), d.get("error") or "")'
  ;;
draft)
  dm=$(api conversations.open -X POST -H 'Content-type: application/json' \
        -d "{\"users\":\"${E2E_SLACK_USER_A}\"}" | jq_py '
import json,sys; print(json.load(sys.stdin).get("channel",{}).get("id",""))')
  echo "self-DM: $dm"
  api "conversations.history?channel=$dm&limit=3" | jq_py '
import json,sys
d=json.load(sys.stdin)
print("ok=",d.get("ok"), d.get("error") or "")
for m in d.get("messages",[]):
    print(" ", m.get("ts"), (m.get("text") or "")[:80])'
  echo
  echo "※ スレッド内エフェメラルは API では読めない。到着の証拠はこの self-DM 記録と"
  echo "   bot ナッジ DM のみ。本体と confirm ダイアログは目視で確認する。"
  ;;
reply)
  ts="${1:?thread ts が要ります}"
  api "conversations.replies?channel=$E2E_SLACK_CHANNEL&ts=$ts&limit=20" | jq_py '
import json,sys
d=json.load(sys.stdin)
ms=d.get("messages",[])
print("ok=",d.get("ok"), d.get("error") or "", "count=",len(ms))
for m in ms:
    print("---", "user=%s"%m.get("user"), "ts=%s"%m.get("ts"))
    print("   ", (m.get("text") or "").replace("\n"," / ")[:220])'
  ;;
watch)
  limit="${1:-900}"; deadline=$(( $(date +%s) + limit ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    line=$(tt task list 2>/dev/null | grep ' slack ' | head -1 || true)
    echo "$(date +%H:%M:%S) ${line:-（slack タスクなし）}"
    case "$line" in *done*|*failed*|*escalated*|*waiting*) exit 0;; esac
    sleep 15
  done
  echo "タイムアウト（${limit}s）"; exit 1
  ;;
*)
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  ;;
esac
