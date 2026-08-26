#!/usr/bin/env bash
# #556 Phase 0: claim 設計（self-assign 排他 + スイムレーン reopen）の前提を
# **実測**する。設計はこの結果が通らなければ成立しない（issue #556 の
# 「着手前に必ず測るもの」）。
#
#   bash .claude/skills/live-e2e/scripts/github-claim-probe.sh probe
#   GH_PROBE_TOKEN='…' bash .../github-claim-probe.sh probe   # 別トークンで測る
#
# 既定トークンは `.env` の `E2E_GH_TOKEN`。github-permissions.sh と同じ流儀:
# トークンは表示しない・argv に載せない（curlrc 0600 経由）・`|| true` で
# curl 失敗でも結果と終了コードを出す。
#
# ## 何をするか（sandbox にしか書かない）
#
# probe 専用の issue を $E2E_GH_REPO_WEB に**新規作成**し、それに対してだけ
# assign/unassign と board の Status 移動を行い、最後に board から外して
# issue を閉じる。既存の issue / item には触らない。
#
# | # | 測ること | 設計上の意味 |
# |---|---|---|
# | P-1 | このトークンで self-assign が読み戻しに現れるか | 現れなければ claim 方式ごと不成立。API エラーと「200 で黙殺」を区別する |
# | P-2 | assign→unassign→re-assign 後の timelineItems(ASSIGNED_EVENT) の件数・順序・last: の意味 | 裁定は createdAt+id の自前ソートだが、取得が末尾ページであることは last: に依存する |
# | P-3 | mutation → 読み戻し反映の遅延（add/remove 混合。標本数は出力の `n=` が正 — P-1/P-2 の分も加算され、途中エラーで減りうる） | `claim_verify_delay_ms` の既定値の根拠 |
# | P-4 | Status 列の移動で fieldValue の updatedAt が進むか + creator | **PR R（reopen の message_key）の前提**。creator は参考記録 |
# | P-5 | 同一 option への再セットで updatedAt が進むか | 参考（validate エラー化で運用上は moot） |
#
# ## 測っていないこと（結果をこれより強く読まない）
#
# - 「push 権限が無い assignee は黙って無視される」の**負のパス**は測れない
#   （sandbox に push 権限の無い第二アカウントが無い）。P-1 が示すのは
#   「このトークン + この login の組で self-assign できる」という十分条件だけ。
# - 単一アカウントなので creator の「最後の書き手 vs 最初の作成者」は判別
#   できない（両方とも自分になる）。updatedAt の進み方だけが判定できる。
#
# ## レート
#
# 1 周 ≈ 40 リクエスト（ポーリング込み・上限側）。全て単発の GraphQL。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "${HERE}/../../../../.env" ]; then
  set -a
  # shellcheck disable=SC1091
  . "${HERE}/../../../../.env"
  set +a
fi

OWNER="${E2E_GH_OWNER:?E2E_GH_OWNER が未設定です。リポジトリルートで source .env してください}"
REPO="${E2E_GH_REPO_WEB:-totsuka-sandbox-web}"
PROJECT="${E2E_GH_PROJECT:?E2E_GH_PROJECT が未設定です}"
OWNER_TYPE="${E2E_GH_OWNER_TYPE:-user}"
STATUS_FIELD="${E2E_GH_STATUS_FIELD:-Status}"
TOKEN="${GH_PROBE_TOKEN:-${E2E_GH_TOKEN:-}}"
[ -n "${TOKEN}" ] || {
  echo "トークンがありません（GH_PROBE_TOKEN か E2E_GH_TOKEN）" >&2
  exit 2
}
case "${OWNER_TYPE}" in
user) ROOT='user' ;;
organization) ROOT='organization' ;;
*)
  echo "E2E_GH_OWNER_TYPE は user か organization（現在: ${OWNER_TYPE}）" >&2
  exit 2
  ;;
esac
case "${1:-probe}" in
probe) ;;
*)
  echo "使い方: github-claim-probe.sh probe" >&2
  exit 2
  ;;
esac

API='https://api.github.com/graphql'
UA='totsuka-task-source-github'
CONNECT_TIMEOUT=10
MAX_TIME=30
# 読み戻しポーリング: 150ms 間隔・12s 上限。P-3 の分解能と上限。
POLL_MS=150
POLL_LIMIT_MS=12000

CURLRC="$(mktemp "${TMPDIR:-/tmp}/ghclaim.XXXXXX")"
chmod 600 "${CURLRC}"
trap 'rm -f "${CURLRC}"' EXIT INT TERM
printf 'header = "Authorization: Bearer %s"\nheader = "User-Agent: %s"\n' "${TOKEN}" "${UA}" >"${CURLRC}"

pass=0 fail=0 note=0
say() { printf '%s\n' "$*"; }
ok() { pass=$((pass + 1)); printf '  [ok]   %s\n' "$*"; }
ng() { fail=$((fail + 1)); printf '  [FAIL] %s\n' "$*"; }
rec() { note=$((note + 1)); printf '  [rec]  %s\n' "$*"; }

gql() {
  curl -sS --config "${CURLRC}" \
    --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${MAX_TIME}" \
    -w '\n#http:%{http_code}' \
    -H 'Content-Type: application/json' \
    --data-binary "$1" "${API}" || true
}
http_of() { printf '%s' "$1" | tail -1 | sed 's/^#http://'; }
body_of() { printf '%s' "$1" | sed '$d'; }
jqr() { printf '%s' "$1" | jq -r "$2" 2>/dev/null || printf '%s' "${3-}"; }
errors_of() {
  printf '%s' "$1" | jq -r '(.errors // [])[] | "\(.type // "-"): \(.message)"' 2>/dev/null || true
}
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
# JSON 文字列リテラル化（クエリを変数展開で組むときの注入・改行対策）
jstr() { printf '%s' "$1" | jq -Rs .; }

# call <説明> <query> [variables-json] → body を stdout。HTTP/errors はここで検査。
call() {
  local desc="$1" q="$2" vars="${3:-null}"
  local body resp code
  body="$(jq -cn --arg q "${q}" --argjson v "${vars}" '{query:$q, variables:$v}')"
  resp="$(gql "${body}")"
  code="$(http_of "${resp}")"
  if [ "${code}" != "200" ]; then
    say "  [FAIL] ${desc}: HTTP ${code}" >&2
    return 1
  fi
  local errs
  errs="$(errors_of "$(body_of "${resp}")")"
  if [ -n "${errs}" ]; then
    say "  [FAIL] ${desc}: GraphQL errors:" >&2
    printf '%s\n' "${errs}" | sed 's/^/         /' >&2
    return 1
  fi
  body_of "${resp}"
}

READ_ISSUE_Q='query($id: ID!) {
  node(id: $id) { ... on Issue { assignees(first: 10) { nodes { login } } } }
}'
# 読み戻しポーリング: want=present|absent の状態になるまでの経過 ms を stdout へ。
# 到達しなければ "timeout"。
wait_assignee() {
  local want="$1" t0 t now body logins
  t0="$(now_ms)"
  while :; do
    body="$(call 'read assignees' "${READ_ISSUE_Q}" "$(jq -cn --arg id "${ISSUE_ID}" '{id:$id}')")" || {
      printf 'error'
      return 0
    }
    logins="$(jqr "${body}" '[.data.node.assignees.nodes[].login] | join(",")')"
    now="$(now_ms)"
    t=$((now - t0))
    case "${want}" in
    present) case ",${logins}," in *",${LOGIN},"*)
      printf '%s' "${t}"
      return 0
      ;;
    esac ;;
    absent) case ",${logins}," in *",${LOGIN},"*) ;; *)
      printf '%s' "${t}"
      return 0
      ;;
    esac ;;
    esac
    if [ "${t}" -ge "${POLL_LIMIT_MS}" ]; then
      printf 'timeout'
      return 0
    fi
    sleep 0.15
  done
}
add_me() {
  call 'addAssigneesToAssignable' \
    'mutation($a: ID!, $u: [ID!]!) { addAssigneesToAssignable(input: {assignableId: $a, assigneeIds: $u}) { clientMutationId } }' \
    "$(jq -cn --arg a "${ISSUE_ID}" --arg u "${MY_ID}" '{a:$a, u:[$u]}')" >/dev/null
}
remove_me() {
  call 'removeAssigneesFromAssignable' \
    'mutation($a: ID!, $u: [ID!]!) { removeAssigneesFromAssignable(input: {assignableId: $a, assigneeIds: $u}) { clientMutationId } }' \
    "$(jq -cn --arg a "${ISSUE_ID}" --arg u "${MY_ID}" '{a:$a, u:[$u]}')" >/dev/null
}

# --- 0. token / viewer -------------------------------------------------------
say '== 0. token =='
case "${TOKEN}" in
github_pat_*) kind='fine-grained PAT' ;;
ghp_*) kind='classic PAT' ;;
gho_*) kind='OAuth token（gh auth token 由来）' ;;
ghs_*) kind='GitHub App installation token' ;;
*) kind='不明' ;;
esac
say "  種別: ${kind}"
body="$(call 'viewer' 'query { viewer { login id } }')" || exit 1
LOGIN="$(jqr "${body}" '.data.viewer.login')"
MY_ID="$(jqr "${body}" '.data.viewer.id')"
say "  viewer: ${LOGIN}"
[ "${LOGIN}" = "${OWNER}" ] || say "  [warn] viewer(${LOGIN}) != E2E_GH_OWNER(${OWNER}) — 判定は viewer 基準で続行"

# --- 1. probe issue 作成 ------------------------------------------------------
say '== 1. probe issue =='
body="$(call 'repository id' \
  'query($o: String!, $r: String!) { repository(owner: $o, name: $r) { id isPrivate } }' \
  "$(jq -cn --arg o "${OWNER}" --arg r "${REPO}" '{o:$o, r:$r}')")" || exit 1
REPO_ID="$(jqr "${body}" '.data.repository.id')"
PRIV="$(jqr "${body}" '.data.repository.isPrivate')"
[ -n "${REPO_ID}" ] && [ "${REPO_ID}" != "null" ] || {
  ng "repository ${OWNER}/${REPO} が読めない"
  exit 1
}
say "  repo: ${OWNER}/${REPO} (private=${PRIV})"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
body="$(call 'createIssue' \
  'mutation($r: ID!, $t: String!, $b: String) { createIssue(input: {repositoryId: $r, title: $t, body: $b}) { issue { id number url } } }' \
  "$(jq -cn --arg r "${REPO_ID}" --arg t "claim-probe #556 ${STAMP}" \
    --arg b 'totsuka #556 Phase 0 の実測 issue。github-claim-probe.sh が作成し、終了時に閉じる。' \
    '{r:$r, t:$t, b:$b}')")" || exit 1
ISSUE_ID="$(jqr "${body}" '.data.createIssue.issue.id')"
ISSUE_URL="$(jqr "${body}" '.data.createIssue.issue.url')"
say "  issue: ${ISSUE_URL}"

cleanup_issue() {
  # 途中失敗でも sandbox を散らかさない。失敗は報告だけして握る。
  if [ -n "${ITEM_ID:-}" ]; then
    call 'deleteProjectV2Item' \
      'mutation($p: ID!, $i: ID!) { deleteProjectV2Item(input: {projectId: $p, itemId: $i}) { deletedItemId } }' \
      "$(jq -cn --arg p "${PROJECT_ID}" --arg i "${ITEM_ID}" '{p:$p, i:$i}')" >/dev/null ||
      say '  [warn] board からの削除に失敗（手で消してください）'
  fi
  if [ -n "${ISSUE_ID:-}" ]; then
    call 'closeIssue' \
      'mutation($i: ID!) { closeIssue(input: {issueId: $i, stateReason: NOT_PLANNED}) { issue { state } } }' \
      "$(jq -cn --arg i "${ISSUE_ID}" '{i:$i}')" >/dev/null ||
      say '  [warn] issue の close に失敗（手で閉じてください）'
  fi
  rm -f "${CURLRC}"
}
trap cleanup_issue EXIT INT TERM

# --- 2. P-1 self-assign ------------------------------------------------------
say '== 2. P-1 self-assign（読み戻し必須） =='
if ! add_me; then
  ng 'P-1 addAssignees が API エラー（トークン権限側の不足 — 黙殺ではない）'
  exit 1
fi
t="$(wait_assignee present)"
case "${t}" in
timeout)
  ng "P-1 200 で通ったのに ${POLL_LIMIT_MS}ms 待っても assignee に現れない = 黙殺の実測（docs の 'silently ignored'）。この login の repo アクセスを確認"
  exit 1
  ;;
error) ng 'P-1 読み戻しが失敗'; exit 1 ;;
*) ok "P-1 self-assign が読み戻しに反映（${t}ms）— claim 方式の前提成立" ;;
esac
SAMPLES="${t}"

# --- 3. P-2 timeline ---------------------------------------------------------
say '== 3. P-2 AssignedEvent の順序 =='
remove_me || true
t="$(wait_assignee absent)"
case "${t}" in timeout | error) ng "P-2 unassign が反映されない（${t}）"; exit 1 ;; *) SAMPLES="${SAMPLES} ${t}" ;; esac
sleep 2 # 2 つの AssignedEvent の createdAt を秒単位で分離する
add_me || { ng 'P-2 re-assign が失敗'; exit 1; }
t="$(wait_assignee present)"
case "${t}" in timeout | error) ng "P-2 re-assign が反映されない（${t}）"; exit 1 ;; *) SAMPLES="${SAMPLES} ${t}" ;; esac

TL_Q='query($id: ID!) {
  node(id: $id) { ... on Issue {
    last: timelineItems(last: 100, itemTypes: [ASSIGNED_EVENT]) {
      totalCount
      nodes { ... on AssignedEvent { id createdAt assignee { ... on User { login } } } }
    }
    first: timelineItems(first: 100, itemTypes: [ASSIGNED_EVENT]) {
      nodes { ... on AssignedEvent { id createdAt } }
    }
  } }
}'
body="$(call 'timelineItems' "${TL_Q}" "$(jq -cn --arg id "${ISSUE_ID}" '{id:$id}')")" || exit 1
count="$(jqr "${body}" '.data.node.last.totalCount')"
logins="$(jqr "${body}" '[.data.node.last.nodes[].assignee.login] | unique | join(",")')"
sorted="$(jqr "${body}" '[.data.node.last.nodes[].createdAt] == ([.data.node.last.nodes[].createdAt] | sort)')"
distinct="$(jqr "${body}" '[.data.node.last.nodes[].createdAt] | unique | length')"
same_page="$(jqr "${body}" '([.data.node.last.nodes[].id] | sort) == ([.data.node.first.nodes[].id] | sort)')"
ids_ok="$(jqr "${body}" '[.data.node.last.nodes[].id] | all(length > 0)')"
if [ "${count}" = "2" ] && [ "${logins}" = "${LOGIN}" ]; then
  ok "P-2 ASSIGNED_EVENT が 2 件・assignee は全て ${LOGIN}（unassign 後も履歴が残る）"
else
  ng "P-2 期待と不一致: totalCount=${count} assignees=${logins}（期待: 2 件・${LOGIN} のみ）"
fi
if [ "${sorted}" = "true" ] && [ "${distinct}" = "2" ]; then
  ok 'P-2 返却順が createdAt 昇順（de-facto 時系列。実装は自前ソートのまま）'
elif [ "${distinct}" != "2" ]; then
  rec "P-2 createdAt が ${distinct} 値に縮退（2s スリープでも同時刻）— 順序判定は不能、tie-break が id 頼みになる実証"
else
  rec 'P-2 返却順は createdAt 昇順ではない — 自前ソート必須の実証（設計どおりなので FAIL ではない）'
fi
[ "${same_page}" = "true" ] && ok 'P-2 first:100 と last:100 が同じ集合（全量 2 件なので当然だが取得系の破損なし）'
[ "${ids_ok}" = "true" ] && ok 'P-2 全イベントに非空の node id（tie-break 材料あり）'

# --- 4. P-3 反映遅延 ----------------------------------------------------------
say '== 4. P-3 mutation→読み戻しの遅延 =='
i=0
while [ "${i}" -lt 5 ]; do
  remove_me || break
  t="$(wait_assignee absent)"
  case "${t}" in timeout | error) break ;; *) SAMPLES="${SAMPLES} ${t}" ;; esac
  add_me || break
  t="$(wait_assignee present)"
  case "${t}" in timeout | error) break ;; *) SAMPLES="${SAMPLES} ${t}" ;; esac
  i=$((i + 1))
done
stats="$(printf '%s\n' ${SAMPLES} | python3 -c '
import sys
xs = sorted(int(l) for l in sys.stdin if l.strip())
if not xs:
    print("標本なし"); raise SystemExit
def pct(p):
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]
print(f"n={len(xs)} min={xs[0]}ms p50={pct(50)}ms p95={pct(95)}ms max={xs[-1]}ms")
')"
rec "P-3 反映遅延（ポーリング分解能 ${POLL_MS}ms 込み）: ${stats}"

# --- 5. board: P-4 / P-5 ------------------------------------------------------
say '== 5. P-4/P-5 Status セルの updatedAt =='
PROJ_Q="query(\$o: String!, \$n: Int!, \$f: String!) {
  ${ROOT}(login: \$o) { projectV2(number: \$n) {
    id
    field(name: \$f) { ... on ProjectV2SingleSelectField { id options { id name } } }
  } }
}"
body="$(call 'project/field' "${PROJ_Q}" \
  "$(jq -cn --arg o "${OWNER}" --argjson n "${PROJECT}" --arg f "${STATUS_FIELD}" '{o:$o, n:$n, f:$f}')")" || exit 1
PROJECT_ID="$(jqr "${body}" ".data.${ROOT}.projectV2.id")"
FIELD_ID="$(jqr "${body}" ".data.${ROOT}.projectV2.field.id")"
OPT_A="$(jqr "${body}" ".data.${ROOT}.projectV2.field.options[0].id")"
OPT_A_NAME="$(jqr "${body}" ".data.${ROOT}.projectV2.field.options[0].name")"
OPT_B="$(jqr "${body}" ".data.${ROOT}.projectV2.field.options[1].id")"
OPT_B_NAME="$(jqr "${body}" ".data.${ROOT}.projectV2.field.options[1].name")"
if [ -z "${PROJECT_ID}" ] || [ "${PROJECT_ID}" = "null" ] || [ -z "${OPT_B}" ] || [ "${OPT_B}" = "null" ]; then
  ng "P-4 project #${PROJECT} / field ${STATUS_FIELD} / option 2 つが揃わない"
  exit 1
fi
body="$(call 'addProjectV2ItemById' \
  'mutation($p: ID!, $c: ID!) { addProjectV2ItemById(input: {projectId: $p, contentId: $c}) { item { id } } }' \
  "$(jq -cn --arg p "${PROJECT_ID}" --arg c "${ISSUE_ID}" '{p:$p, c:$c}')")" || exit 1
ITEM_ID="$(jqr "${body}" '.data.addProjectV2ItemById.item.id')"

set_status() {
  call 'updateProjectV2ItemFieldValue' \
    'mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) { updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i, fieldId: $f, value: {singleSelectOptionId: $o}}) { clientMutationId } }' \
    "$(jq -cn --arg p "${PROJECT_ID}" --arg i "${ITEM_ID}" --arg f "${FIELD_ID}" --arg o "$1" '{p:$p, i:$i, f:$f, o:$o}')" >/dev/null
}
read_status() {
  call 'read status cell' \
    'query($id: ID!, $f: String!) { node(id: $id) { ... on ProjectV2Item {
       fieldValueByName(name: $f) { ... on ProjectV2ItemFieldSingleSelectValue { name optionId updatedAt creator { login } } } } } }' \
    "$(jq -cn --arg id "${ITEM_ID}" --arg f "${STATUS_FIELD}" '{id:$id, f:$f}')"
}
# セルの更新を待つ（optionId が want になるまで）。updatedAt を stdout へ。
wait_status() {
  local want="$1" t0 body opt
  t0="$(now_ms)"
  while :; do
    body="$(read_status)" || { printf 'error'; return 0; }
    opt="$(jqr "${body}" '.data.node.fieldValueByName.optionId')"
    if [ "${opt}" = "${want}" ]; then
      jqr "${body}" '.data.node.fieldValueByName | "\(.updatedAt)\t\(.creator.login // "-")\t\(.name)"'
      return 0
    fi
    [ $(($(now_ms) - t0)) -ge "${POLL_LIMIT_MS}" ] && { printf 'timeout'; return 0; }
    sleep 0.15
  done
}

set_status "${OPT_A}" || { ng 'P-4 Status 書き込みが失敗'; exit 1; }
r1="$(wait_status "${OPT_A}")"
case "${r1}" in timeout | error) ng "P-4 Status(${OPT_A_NAME}) が読めない（${r1}）"; exit 1 ;; esac
u1="$(printf '%s' "${r1}" | cut -f1)"
c1="$(printf '%s' "${r1}" | cut -f2)"
sleep 2
set_status "${OPT_B}" || { ng 'P-4 Status 移動が失敗'; exit 1; }
r2="$(wait_status "${OPT_B}")"
case "${r2}" in timeout | error) ng "P-4 Status(${OPT_B_NAME}) が読めない（${r2}）"; exit 1 ;; esac
u2="$(printf '%s' "${r2}" | cut -f1)"
if [ "${u2}" != "${u1}" ] && [ "$(printf '%s\n%s\n' "${u1}" "${u2}" | sort | tail -1)" = "${u2}" ]; then
  ok "P-4 列移動（${OPT_A_NAME}→${OPT_B_NAME}）で updatedAt が進む: ${u1} → ${u2} — reopen の message_key の前提成立"
else
  ng "P-4 列移動で updatedAt が進まない: ${u1} → ${u2} — PR R の edge 検出が不成立"
fi
rec "P-4 creator=${c1}（単一アカウントのため last-writer/first-creator は判別不能。参考記録）"

sleep 2
set_status "${OPT_B}" || { ng 'P-5 同一 option 再セットが失敗'; exit 1; }
sleep 1.5 # 冪等書き込みの反映を待つ（変化が無い可能性があるのでポーリング不能）
r3="$(read_status)" || true
u3="$(jqr "${r3}" '.data.node.fieldValueByName.updatedAt')"
if [ "${u3}" = "${u2}" ]; then
  rec "P-5 同一 option への再セットでは updatedAt が進まない（${u3} のまま）— 冪等書き込みは edge を刻まない"
else
  rec "P-5 同一 option への再セットでも updatedAt が進む: ${u2} → ${u3} — set_status==trigger の validate エラー化が必須である実証"
fi

# --- 結果 --------------------------------------------------------------------
say ''
say "結果: pass=${pass} fail=${fail} 記録=${note}（probe issue は閉じて board からも外した）"
[ "${fail}" -eq 0 ] || exit 1
