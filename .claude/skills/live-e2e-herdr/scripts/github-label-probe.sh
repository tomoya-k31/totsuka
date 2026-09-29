#!/usr/bin/env bash
# ラベル書き戻し（ADR-0108、`on_*.labels`）に要るトークン権限を**実測**する（#840）。
# docs の権限表のラベル行はこれまで導出であって実測ではなかった。
#
#   bash .claude/skills/live-e2e-herdr/scripts/github-label-probe.sh probe
#   GH_PROBE_TOKEN="$(pbpaste)" bash .../github-label-probe.sh probe   # 測りたいトークン
#
# ## トークンは 2 役に分ける
#
# - **準備役** `E2E_GH_TOKEN`（`.env`）: probe 用の Issue・PR（ブランチ + コミット）・
#   ラベルを作り、読み戻し、最後に片付ける。十分な権限を持つサンドボックス用トークン
# - **測定役** `GH_PROBE_TOKEN`（省略時は準備役と同じ）: プラグインが実際に投げる
#   5 操作**だけ**を投げる
#
# 分けるのは、PR を作るのに要る権限（Contents: write など）を測定結果に混ぜないため。
# 測定役に要るのは「プラグインが要るもの」だけで、それ以外は準備役が肩代わりする。
#
# github-permissions.sh と同じ流儀: トークンは表示しない・argv に載せない（curlrc 0600
# 経由）・`|| true` で curl 失敗でも結果と終了コードを出す。
#
# ## 測る 5 操作（クエリ本文は plugins/task-source-github/src/client.rs と同一）
#
# | # | 操作 | 対象 |
# |---|---|---|
# | L-1 | `TASK_LABEL_QUERY`（node → repository.label(name:)） | Issue と PR |
# | L-2 | `ADD_LABELS_MUTATION`（addLabelsToLabelable） | Issue と PR |
# | L-3 | `REMOVE_LABELS_MUTATION`（removeLabelsFromLabelable） | Issue と PR |
# | L-4 | `CREATE_LABEL_MUTATION`（createLabel） | リポジトリ |
# | L-5 | `REPO_LABEL_QUERY`（config/validate の存在検査） | リポジトリ |
#
# **「エラーが出なかった」を pass と読まない。** mutation は準備役で**読み戻して**
# 反映を確かめる（200 で黙殺される形を FAIL として拾う）。query は `errors` の有無と
# 独立に、要るフィールドが null でないかで判定する。
#
# ## 何をするか（sandbox にしか書かない）
#
# $E2E_GH_REPO_WEB に probe 専用の Issue・ブランチ・PR・ラベル 2 つを**新規作成**し、
# それにだけ触る。終了時（失敗時も）に Issue と PR を閉じ、ブランチとラベルを消す。
# 既存の issue / PR / ラベルには触らない。
#
# ## レート
#
# 1 周 ≈ 30 リクエスト（読み戻しのポーリング込み・上限側）。全て単発の GraphQL。
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
SETUP_TOKEN="${E2E_GH_TOKEN:-}"
PROBE_TOKEN="${GH_PROBE_TOKEN:-${SETUP_TOKEN}}"
[ -n "${SETUP_TOKEN}" ] || {
  echo "準備役のトークンがありません（E2E_GH_TOKEN）" >&2
  exit 2
}
case "${1:-probe}" in
probe) ;;
*)
  echo "使い方: github-label-probe.sh probe" >&2
  exit 2
  ;;
esac

API='https://api.github.com/graphql'
UA='totsuka-task-source-github'
CONNECT_TIMEOUT=10
MAX_TIME=30
POLL_LIMIT_MS=10000

SETUP_RC="$(mktemp "${TMPDIR:-/tmp}/ghlabel-setup.XXXXXX")"
PROBE_RC="$(mktemp "${TMPDIR:-/tmp}/ghlabel-probe.XXXXXX")"
chmod 600 "${SETUP_RC}" "${PROBE_RC}"
trap 'rm -f "${SETUP_RC}" "${PROBE_RC}"' EXIT INT TERM
printf 'header = "Authorization: Bearer %s"\nheader = "User-Agent: %s"\n' "${SETUP_TOKEN}" "${UA}" >"${SETUP_RC}"
printf 'header = "Authorization: Bearer %s"\nheader = "User-Agent: %s"\n' "${PROBE_TOKEN}" "${UA}" >"${PROBE_RC}"

pass=0 fail=0
say() { printf '%s\n' "$*"; }
ok() {
  pass=$((pass + 1))
  printf '  [ok]   %s\n' "$*"
}
ng() {
  fail=$((fail + 1))
  printf '  [FAIL] %s\n' "$*"
}

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
jqr() { printf '%s' "$1" | jq -r "$2" 2>/dev/null || printf '%s' "${3-}"; }

# call <setup|probe> <説明> <query> [variables-json] → body を stdout。
# HTTP 200 以外と GraphQL errors は失敗（stderr に理由）。プラグインの
# check_errors が errors を拒否する以上、ここも緩めない。
call() {
  local who="$1" desc="$2" q="$3" vars="${4:-null}" rc body resp code errs
  case "${who}" in setup) rc="${SETUP_RC}" ;; probe) rc="${PROBE_RC}" ;; esac
  body="$(jq -cn --arg q "${q}" --argjson v "${vars}" '{query:$q, variables:$v}')"
  resp="$(curl -sS --config "${rc}" --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${MAX_TIME}" \
    -w '\n#http:%{http_code}' -H 'Content-Type: application/json' \
    --data-binary "${body}" "${API}" || true)"
  code="$(printf '%s' "${resp}" | tail -1 | sed 's/^#http://')"
  resp="$(printf '%s' "${resp}" | sed '$d')"
  if [ "${code}" != "200" ]; then
    say "         ${desc}: HTTP ${code}" >&2
    return 1
  fi
  errs="$(printf '%s' "${resp}" | jq -r '(.errors // [])[] | "\(.type // "-"): \(.message)"' 2>/dev/null || true)"
  if [ -n "${errs}" ]; then
    say "         ${desc}: GraphQL errors:" >&2
    printf '%s\n' "${errs}" | sed 's/^/           /' >&2
    return 1
  fi
  printf '%s' "${resp}"
}

# --- プラグインと同一のクエリ本文（client.rs） ---------------------------------
TASK_LABEL_QUERY='query($id: ID!, $name: String!) {
  node(id: $id) {
    ... on Issue { repository { id label(name: $name) { id } } }
    ... on PullRequest { repository { id label(name: $name) { id } } }
  }
}'
REPO_LABEL_QUERY='query($owner: String!, $repo: String!, $name: String!) {
  repository(owner: $owner, name: $repo) { label(name: $name) { id } }
}'
CREATE_LABEL_MUTATION='mutation($repo: ID!, $name: String!) {
  createLabel(input: {repositoryId: $repo, name: $name, color: "ededed"}) { label { id } }
}'
ADD_LABELS_MUTATION='mutation($l: ID!, $ids: [ID!]!) {
  addLabelsToLabelable(input: {labelableId: $l, labelIds: $ids}) { clientMutationId }
}'
REMOVE_LABELS_MUTATION='mutation($l: ID!, $ids: [ID!]!) {
  removeLabelsFromLabelable(input: {labelableId: $l, labelIds: $ids}) { clientMutationId }
}'

# --- 0. トークン ---------------------------------------------------------------
kind_of() {
  case "$1" in
  github_pat_*) printf 'fine-grained PAT' ;;
  ghp_*) printf 'classic PAT' ;;
  gho_*) printf 'OAuth token（gh auth token 由来）' ;;
  ghs_*) printf 'GitHub App installation token' ;;
  *) printf '不明' ;;
  esac
}
say '== 0. token =='
say "  測定役: $(kind_of "${PROBE_TOKEN}")$([ "${PROBE_TOKEN}" = "${SETUP_TOKEN}" ] && printf '（準備役と同じ）')"
scopes="$(curl -sS --config "${PROBE_RC}" --connect-timeout "${CONNECT_TIMEOUT}" --max-time "${MAX_TIME}" \
  -o /dev/null -D - https://api.github.com/user 2>/dev/null |
  tr -d '\r' | sed -n 's/^[Xx]-[Oo][Aa]uth-[Ss]copes: //p' || true)"
say "  測定役の scope: ${scopes:-（ヘッダ無し — fine-grained PAT なら正常）}"
body="$(call probe 'viewer' 'query { viewer { login } }')" || {
  ng '測定役のトークンで viewer が読めない'
  exit 1
}
say "  viewer: $(jqr "${body}" '.data.viewer.login')"

# --- 1. fixture（準備役） --------------------------------------------------------
say '== 1. fixture（準備役が作る） =='
body="$(call setup 'repository' \
  'query($o: String!, $r: String!) { repository(owner: $o, name: $r) { id isPrivate defaultBranchRef { name target { oid } } } }' \
  "$(jq -cn --arg o "${OWNER}" --arg r "${REPO}" '{o:$o, r:$r}')")" || exit 1
REPO_ID="$(jqr "${body}" '.data.repository.id')"
BASE="$(jqr "${body}" '.data.repository.defaultBranchRef.name')"
BASE_OID="$(jqr "${body}" '.data.repository.defaultBranchRef.target.oid')"
say "  repo: ${OWNER}/${REPO} (private=$(jqr "${body}" '.data.repository.isPrivate'))"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
LABEL="label-probe-${STAMP}"
NEW_LABEL="label-probe-${STAMP}-new"
BRANCH="label-probe-${STAMP}"

cleanup() {
  # 途中失敗でも sandbox を散らかさない。失敗は報告だけして握る。
  [ -n "${PR_ID:-}" ] && { call setup 'closePullRequest' \
    'mutation($i: ID!) { closePullRequest(input: {pullRequestId: $i}) { clientMutationId } }' \
    "$(jq -cn --arg i "${PR_ID}" '{i:$i}')" >/dev/null || say '  [warn] PR の close に失敗（手で閉じてください）'; }
  [ -n "${REF_ID:-}" ] && { call setup 'deleteRef' \
    'mutation($i: ID!) { deleteRef(input: {refId: $i}) { clientMutationId } }' \
    "$(jq -cn --arg i "${REF_ID}" '{i:$i}')" >/dev/null || say "  [warn] ブランチ ${BRANCH} の削除に失敗（手で消してください）"; }
  [ -n "${ISSUE_ID:-}" ] && { call setup 'closeIssue' \
    'mutation($i: ID!) { closeIssue(input: {issueId: $i, stateReason: NOT_PLANNED}) { clientMutationId } }' \
    "$(jq -cn --arg i "${ISSUE_ID}" '{i:$i}')" >/dev/null || say '  [warn] issue の close に失敗（手で閉じてください）'; }
  for name in "${LABEL}" "${NEW_LABEL}"; do
    id="$(jqr "$(call setup 'label id' "${REPO_LABEL_QUERY}" \
      "$(jq -cn --arg o "${OWNER}" --arg r "${REPO}" --arg n "${name}" '{owner:$o, repo:$r, name:$n}')" || true)" \
      '.data.repository.label.id // empty')"
    [ -n "${id}" ] && { call setup 'deleteLabel' \
      'mutation($i: ID!) { deleteLabel(input: {id: $i}) { clientMutationId } }' \
      "$(jq -cn --arg i "${id}" '{i:$i}')" >/dev/null || say "  [warn] ラベル ${name} の削除に失敗（手で消してください）"; }
  done
  rm -f "${SETUP_RC}" "${PROBE_RC}"
}
trap cleanup EXIT INT TERM

body="$(call setup 'createLabel(fixture)' \
  'mutation($r: ID!, $n: String!) { createLabel(input: {repositoryId: $r, name: $n, color: "ededed"}) { label { id } } }' \
  "$(jq -cn --arg r "${REPO_ID}" --arg n "${LABEL}" '{r:$r, n:$n}')")" || exit 1
LABEL_ID="$(jqr "${body}" '.data.createLabel.label.id')"

body="$(call setup 'createIssue' \
  'mutation($r: ID!, $t: String!, $b: String) { createIssue(input: {repositoryId: $r, title: $t, body: $b}) { issue { id url } } }' \
  "$(jq -cn --arg r "${REPO_ID}" --arg t "label-probe #840 ${STAMP}" \
    --arg b 'totsuka #840 の実測 issue。github-label-probe.sh が作成し、終了時に閉じる。' '{r:$r, t:$t, b:$b}')")" || exit 1
ISSUE_ID="$(jqr "${body}" '.data.createIssue.issue.id')"
say "  issue: $(jqr "${body}" '.data.createIssue.issue.url')"

body="$(call setup 'createRef' \
  'mutation($r: ID!, $n: String!, $o: GitObjectID!) { createRef(input: {repositoryId: $r, name: $n, oid: $o}) { ref { id } } }' \
  "$(jq -cn --arg r "${REPO_ID}" --arg n "refs/heads/${BRANCH}" --arg o "${BASE_OID}" '{r:$r, n:$n, o:$o}')")" || exit 1
REF_ID="$(jqr "${body}" '.data.createRef.ref.id')"
call setup 'createCommitOnBranch' \
  'mutation($b: CommittableBranch!, $o: GitObjectID!, $c: Base64String!) { createCommitOnBranch(input: {branch: $b, expectedHeadOid: $o, message: {headline: "label probe (#840)"}, fileChanges: {additions: [{path: "label-probe.txt", contents: $c}]}}) { commit { oid } } }' \
  "$(jq -cn --arg nwo "${OWNER}/${REPO}" --arg br "${BRANCH}" --arg o "${BASE_OID}" \
    --arg c "$(printf 'label probe %s\n' "${STAMP}" | base64)" \
    '{b:{repositoryNameWithOwner:$nwo, branchName:$br}, o:$o, c:$c}')" >/dev/null || exit 1
body="$(call setup 'createPullRequest' \
  'mutation($r: ID!, $base: String!, $head: String!, $t: String!, $b: String) { createPullRequest(input: {repositoryId: $r, baseRefName: $base, headRefName: $head, title: $t, body: $b}) { pullRequest { id url } } }' \
  "$(jq -cn --arg r "${REPO_ID}" --arg base "${BASE}" --arg head "${BRANCH}" --arg t "label-probe #840 ${STAMP}" \
    --arg b 'totsuka #840 の実測 PR。github-label-probe.sh が作成し、終了時に閉じてブランチを消す。' \
    '{r:$r, base:$base, head:$head, t:$t, b:$b}')")" || exit 1
PR_ID="$(jqr "${body}" '.data.createPullRequest.pullRequest.id')"
say "  pr:    $(jqr "${body}" '.data.createPullRequest.pullRequest.url')"
say "  label: ${LABEL}（準備役が作成）"

# 準備役で labels を読み戻し、want=present|absent になるまで待つ。ok / timeout / error。
wait_label() {
  local id="$1" want="$2" t0 body names has
  t0="$(now_ms)"
  while :; do
    body="$(call setup 'read labels' \
      'query($id: ID!) { node(id: $id) { ... on Issue { labels(first: 50) { nodes { name } } } ... on PullRequest { labels(first: 50) { nodes { name } } } } }' \
      "$(jq -cn --arg id "${id}" '{id:$id}')")" || {
      printf 'error'
      return 0
    }
    names=",$(jqr "${body}" '[.data.node.labels.nodes[].name] | join(",")'),"
    case "${names}" in *",${LABEL},"*) has=present ;; *) has=absent ;; esac
    if [ "${has}" = "${want}" ]; then
      printf 'ok'
      return 0
    fi
    [ $(($(now_ms) - t0)) -ge "${POLL_LIMIT_MS}" ] && {
      printf 'timeout'
      return 0
    }
    sleep 0.3
  done
}

# --- 2. L-1..L-3（Issue と PR） ---------------------------------------------------
for target in issue pr; do
  case "${target}" in issue) ID="${ISSUE_ID}" ;; pr) ID="${PR_ID}" ;; esac
  say "== 2. ${target}: L-1 解決 / L-2 付与 / L-3 除去（測定役） =="
  if body="$(call probe 'L-1 TASK_LABEL_QUERY' "${TASK_LABEL_QUERY}" \
    "$(jq -cn --arg id "${ID}" --arg n "${LABEL}" '{id:$id, name:$n}')")"; then
    got="$(jqr "${body}" '.data.node.repository.label.id // empty')"
    rid="$(jqr "${body}" '.data.node.repository.id // empty')"
    if [ "${got}" = "${LABEL_ID}" ] && [ -n "${rid}" ]; then
      ok "L-1 ${target}: repository.id と label.id が読める"
    else
      ng "L-1 ${target}: errors 無しで repository.id=${rid:-null} label.id=${got:-null}（権限不足で null の疑い）"
    fi
  else
    ng "L-1 ${target}: API エラー"
  fi

  if call probe 'L-2 addLabelsToLabelable' "${ADD_LABELS_MUTATION}" \
    "$(jq -cn --arg l "${ID}" --arg i "${LABEL_ID}" '{l:$l, ids:[$i]}')" >/dev/null; then
    case "$(wait_label "${ID}" present)" in
    ok) ok "L-2 ${target}: 付与が読み戻しに反映" ;;
    *) ng "L-2 ${target}: 200 で通ったのに ${POLL_LIMIT_MS}ms 待っても付かない（黙殺）" ;;
    esac
  else
    ng "L-2 ${target}: API エラー"
    # L-3 を測るために準備役で付けておく
    call setup 'addLabels(fixture)' "${ADD_LABELS_MUTATION}" \
      "$(jq -cn --arg l "${ID}" --arg i "${LABEL_ID}" '{l:$l, ids:[$i]}')" >/dev/null || true
    wait_label "${ID}" present >/dev/null
  fi

  if call probe 'L-3 removeLabelsFromLabelable' "${REMOVE_LABELS_MUTATION}" \
    "$(jq -cn --arg l "${ID}" --arg i "${LABEL_ID}" '{l:$l, ids:[$i]}')" >/dev/null; then
    case "$(wait_label "${ID}" absent)" in
    ok) ok "L-3 ${target}: 除去が読み戻しに反映" ;;
    *) ng "L-3 ${target}: 200 で通ったのに ${POLL_LIMIT_MS}ms 待っても外れない（黙殺）" ;;
    esac
  else
    ng "L-3 ${target}: API エラー"
  fi
done

# --- 3. L-4 / L-5（リポジトリ） ----------------------------------------------------
say '== 3. L-4 作成 / L-5 存在検査（測定役） =='
if body="$(call probe 'L-4 createLabel' "${CREATE_LABEL_MUTATION}" \
  "$(jq -cn --arg r "${REPO_ID}" --arg n "${NEW_LABEL}" '{repo:$r, name:$n}')")"; then
  if [ -n "$(jqr "${body}" '.data.createLabel.label.id // empty')" ]; then
    ok "L-4 createLabel が id を返す（${NEW_LABEL}）"
  else
    ng 'L-4 createLabel が errors 無しで id を返さない'
  fi
else
  ng 'L-4 createLabel: API エラー'
fi

if body="$(call probe 'L-5 REPO_LABEL_QUERY' "${REPO_LABEL_QUERY}" \
  "$(jq -cn --arg o "${OWNER}" --arg r "${REPO}" --arg n "${LABEL}" '{owner:$o, repo:$r, name:$n}')")"; then
  if [ "$(jqr "${body}" '.data.repository.label.id // empty')" = "${LABEL_ID}" ]; then
    ok 'L-5 repository(owner:, name:).label(name:) が読める'
  else
    ng "L-5 errors 無しで label.id が null（権限不足で null の疑い）"
  fi
else
  ng 'L-5 REPO_LABEL_QUERY: API エラー'
fi

# --- 結果 --------------------------------------------------------------------
say ''
say "結果: pass=${pass} fail=${fail}（probe の issue / PR は閉じ、ブランチとラベルは消す）"
[ "${fail}" -eq 0 ] || exit 1
