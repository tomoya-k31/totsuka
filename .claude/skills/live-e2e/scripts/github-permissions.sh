#!/usr/bin/env bash
# GitHub トークンの必要権限を**実測**する（#514）。docs の権限表はこれまで
# 導出であって実測ではなかった。
#
#   bash .claude/skills/live-e2e/scripts/github-permissions.sh probe
#   bash .claude/skills/live-e2e/scripts/github-permissions.sh probe --write
#   GH_PROBE_TOKEN='github_pat_…' bash .../github-permissions.sh probe --write
#
# 既定のトークンは `.env` の `E2E_GH_TOKEN`。`GH_PROBE_TOKEN` を渡すとそちらを
# 使う（権限を削った PAT を順に試すため）。**トークンは一切表示しない** —
# 表示するのは種別（`ghp_` / `github_pat_` / `gho_` …）と scope ヘッダだけ。
#
# ## なぜ専用のスクリプトなのか
#
# `totsuka doctor --online` は `viewer` しか叩かない（F-59）。つまり **read の
# 4 操作のうち 1 つしか通らない**ので、doctor が緑でも fetch が空になる権限
# 構成が存在する。逆に実 `run` は権限だけを切り分けられない（LLM・herdr・
# worktree が同時に動く）。ここは**プラグインが実際に投げる 4 操作だけ**を、
# 同じエンドポイント・同じヘッダ・同じクエリ本文で投げる。
#
# ## 一番大事な設計上の点: 「エラーが出なかった」を pass と読まない
#
# GraphQL の権限不足は **HTTP 200 + `data` あり + フィールドが `null`** で
# 出うる（`errors` に `INSUFFICIENT_SCOPES` が並ぶこともあるが、並ばずに
# 黙って `null` になる形もある）。したがってこのスクリプトは
# **フィールド単位で present / null を判定**し、`errors` の有無とは独立に
# 報告する。プラグインが実際に読むフィールドだけを見る:
#
#   body / labels / assignees / repository.name / title / url / number
#
# ## write プローブは破壊的ではない
#
# `--write` は item の **現在の Status と同じ option** を書き戻す。
# `updateProjectV2ItemFieldValue` は冪等（client.rs の
# `update_status` が「Idempotent: setting the same option again yields the
# same state」と書いているのと同じ性質）なので、権限だけを測って盤面を
# 動かさない。Status が未設定の item しかない場合は測れないので、その旨を
# 報告して skip する（推測で何かを書き込むことはしない）。
#
# ## レート
#
# 1 周の実測は下の `rate` 行が毎回表示する。fetch / resolve が items(first:100)
# を 1 ページだけ引くので、`gh project item-list` 経由（102 points）より安い。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# _common.sh は E2E_HOME を必須にする。このスクリプトは totsuka の状態を
# 触らないので、E2E_HOME が無くても `.env` さえ読めれば動くようにする。
if [ -f "$HERE/../../../../.env" ]; then
  set -a
  # shellcheck disable=SC1091
  . "$HERE/../../../../.env"
  set +a
fi

: "${E2E_GH_OWNER:?E2E_GH_OWNER が未設定です。リポジトリルートで source .env してください}"
OWNER="$E2E_GH_OWNER"
PROJECT="${E2E_GH_PROJECT:?E2E_GH_PROJECT が未設定です}"
# plugins/github.toml の `owner_type` と同じ意味。GraphQL のルートフィールドが
# 変わる（client.rs の `OwnerType::graphql_root`）。
OWNER_TYPE="${E2E_GH_OWNER_TYPE:-user}"
STATUS_FIELD="${E2E_GH_STATUS_FIELD:-Status}"
TOKEN="${GH_PROBE_TOKEN:-${E2E_GH_TOKEN:-}}"
[ -n "$TOKEN" ] || {
  echo "トークンがありません（GH_PROBE_TOKEN か E2E_GH_TOKEN）" >&2
  exit 2
}

API='https://api.github.com/graphql'
# transport.rs の `attempt` が送るのと同じ User-Agent。GitHub は UA 無しの
# GraphQL を拒否する。
UA='totsuka-task-source-github'

case "$OWNER_TYPE" in
user) ROOT='user' ;;
organization) ROOT='organization' ;;
*)
  echo "E2E_GH_OWNER_TYPE は user か organization（現在: $OWNER_TYPE）" >&2
  exit 2
  ;;
esac

# サブコマンドは `probe` の 1 つだけ。`--write` は位置に依存させない
# （`probe --write` と `--write probe` で挙動が変わるのは事故のもと）。
WRITE=0
for a in "$@"; do
  case "$a" in
  --write) WRITE=1 ;;
  probe | '') ;;
  *)
    echo "不明な引数: $a（使い方: github-permissions.sh probe [--write]）" >&2
    exit 2
    ;;
  esac
done

pass=0
fail=0
skip=0
say() { printf '%s\n' "$*"; }
ok() {
  pass=$((pass + 1))
  printf '  [ok]   %s\n' "$*"
}
ng() {
  fail=$((fail + 1))
  printf '  [FAIL] %s\n' "$*"
}
sk() {
  skip=$((skip + 1))
  printf '  [skip] %s\n' "$*"
}

# gql <json-body> — 生の応答を返す。HTTP ステータスは最終行に `#http:<code>`。
gql() {
  curl -sS -w '\n#http:%{http_code}' \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "User-Agent: ${UA}" \
    -H 'Content-Type: application/json' \
    --data-binary "$1" "$API"
}

http_of() { printf '%s' "$1" | tail -1 | sed 's/^#http://'; }
body_of() { printf '%s' "$1" | sed '$d'; }

# errors_of — GraphQL の errors を 1 行ずつ。空なら何も出さない。
errors_of() {
  printf '%s' "$1" | jq -r '(.errors // [])[] | "\(.type // "-"): \(.message)"' 2>/dev/null || true
}

# --- 0. トークンの素性（値は絶対に出さない） --------------------------------
say '== token =='
case "$TOKEN" in
github_pat_*) kind='fine-grained PAT' ;;
ghp_*) kind='classic PAT' ;;
gho_*) kind='OAuth token（gh auth token 由来）' ;;
ghs_*) kind='GitHub App installation token' ;;
*) kind='不明' ;;
esac
say "  種別: ${kind}"
# classic PAT / OAuth はこのヘッダで scope が読める。fine-grained PAT では
# **ヘッダごと出ない** — 「空」ではなく「無い」ことが種別の裏取りになる。
hdr="$(curl -sS -D - -o /dev/null -H "Authorization: Bearer ${TOKEN}" -H "User-Agent: ${UA}" \
  https://api.github.com/ 2>/dev/null | tr -d '\r' || true)"
scopes="$(printf '%s' "$hdr" | \grep -i '^x-oauth-scopes:' | cut -d' ' -f2- || true)"
if [ -n "$scopes" ]; then
  say "  x-oauth-scopes: ${scopes}"
else
  say '  x-oauth-scopes: （ヘッダ無し = fine-grained PAT / App トークン。scope 概念が無い）'
fi
say "  対象: ${ROOT}(login: ${OWNER}) / projectV2(number: ${PROJECT}) / field ${STATUS_FIELD}"
say ''

# --- 1. viewer（疎通。doctor --online が唯一叩く操作） ----------------------
say '== 1. viewer { login }  （VIEWER_QUERY / doctor --online） =='
resp="$(gql '{"query":"query { viewer { login } }"}')"
code="$(http_of "$resp")"
b="$(body_of "$resp")"
errs="$(errors_of "$b")"
login="$(printf '%s' "$b" | jq -r '.data.viewer.login // empty')"
if [ "$code" = 200 ] && [ -n "$login" ]; then
  ok "viewer.login = ${login}  (http ${code})"
else
  ng "viewer が取れない (http ${code})"
fi
[ -n "$errs" ] && printf '         errors: %s\n' "$errs"
say ''

# --- 2. fetch（Project アイテム + Issue の中身） ---------------------------
# client.rs の `fetch_query` と同じ選択集合。**1 ページだけ**引く（権限の
# 判定にページ送りは要らない）。
say '== 2. Project アイテム取得  （fetch_query / totsuka run の read 経路） =='
read -r -d '' FETCH <<EOF || true
query(\$owner: String!, \$number: Int!, \$statusField: String!) {
  ${ROOT}(login: \$owner) {
    projectV2(number: \$number) {
      items(first: 100) {
        pageInfo { hasNextPage endCursor }
        nodes {
          status: fieldValueByName(name: \$statusField) {
            ... on ProjectV2ItemFieldSingleSelectValue { name }
          }
          content {
            __typename
            ... on Issue {
              id number title body url
              repository { name }
              assignees(first: 10) { nodes { login } }
              labels(first: 100) { nodes { name } }
            }
          }
        }
      }
    }
  }
}
EOF
body="$(jq -n --arg q "$FETCH" --arg o "$OWNER" --argjson n "$PROJECT" --arg s "$STATUS_FIELD" \
  '{query:$q, variables:{owner:$o, number:$n, statusField:$s}}')"
resp="$(gql "$body")"
code="$(http_of "$resp")"
b="$(body_of "$resp")"
errs="$(errors_of "$b")"
proj="$(printf '%s' "$b" | jq -c ".data.${ROOT}.projectV2 // empty")"

if [ "$code" != 200 ]; then
  ng "http ${code} — 応答本文: $(printf '%s' "$b" | head -c 300)"
elif [ -z "$proj" ] || [ "$proj" = null ]; then
  ng "projectV2 が null（Projects 権限が無い / board が見えない）"
else
  n_items="$(printf '%s' "$b" | jq ".data.${ROOT}.projectV2.items.nodes | length")"
  ok "projectV2 が見える / items ${n_items} 件"
  # ここが本題。**Issue の中身がフィールド単位で返っているか**を見る。
  # `content` が Issue でないもの（DraftIssue / PullRequest）は母集団から外す。
  issues="$(printf '%s' "$b" | jq "[.data.${ROOT}.projectV2.items.nodes[]
      | select(.content.__typename == \"Issue\")]")"
  n_issue="$(printf '%s' "$issues" | jq 'length')"
  if [ "$n_issue" = 0 ]; then
    sk 'Issue の item が 0 件 — フィールド単位の判定ができない（board に Issue を入れてから再実行）'
  else
    ok "Issue の item ${n_issue} 件を母集団にする"
    # プラグインが実際に読むフィールドだけを見る。**1 件でも非 null なら
    # 「読めている」**とする（null は item 固有の理由でも起きる: body 空、
    # ラベル無しなど）。逆に**全件 null** は権限を疑う根拠になる。
    for f in 'title' 'body' 'url' 'number' 'repository.name'; do
      # `.content` から辿ること。母集団の要素は **item** であって Issue では
      # ないので、`.content` を挟まないと `getpath` は必ず null を返し、
      # **全件 null = 権限不足**と読める偽陽性になる（実際に一度出した）。
      got="$(printf '%s' "$issues" | jq --arg f "$f" \
        '[.[] | .content | getpath($f | split(".")) | select(. != null and . != "")] | length')"
      if [ "$got" -gt 0 ]; then
        ok "content.${f}: ${got}/${n_issue} 件で非 null"
      else
        ng "content.${f}: 全 ${n_issue} 件が null/空 ← 権限不足の疑い"
      fi
    done
    for f in 'assignees' 'labels'; do
      # nodes が null（権限不足）と、nodes が [] （本当に付いていない）は
      # **別物**。前者だけが権限の話なので、分けて数える。
      nul="$(printf '%s' "$issues" | jq --arg f "$f" '[.[] | .content[$f].nodes | select(. == null)] | length')"
      non="$(printf '%s' "$issues" | jq --arg f "$f" '[.[] | .content[$f].nodes | select(. != null and length > 0)] | length')"
      if [ "$nul" -gt 0 ]; then
        ng "content.${f}.nodes: ${nul}/${n_issue} 件が null ← 権限不足の疑い（[] とは別物）"
      elif [ "$non" -gt 0 ]; then
        ok "content.${f}.nodes: 非空 ${non}/${n_issue} 件（null は 0 件）"
      else
        sk "content.${f}.nodes: 全件 [] — 実際に付いていないのか判別不能（付いた item を board に入れて再実行）"
      fi
    done
  fi
fi
[ -n "$errs" ] && printf '         errors: %s\n' "$errs"
say ''

# --- 3. resolve（project / field / item の id） -----------------------------
say '== 3. project / Status フィールド / item の id 解決  （resolve_query） =='
read -r -d '' RESOLVE <<EOF || true
query(\$owner: String!, \$number: Int!, \$statusField: String!) {
  ${ROOT}(login: \$owner) {
    projectV2(number: \$number) {
      id
      field(name: \$statusField) {
        ... on ProjectV2SingleSelectField { id options { id name } }
      }
      items(first: 100) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          status: fieldValueByName(name: \$statusField) {
            ... on ProjectV2ItemFieldSingleSelectValue { name optionId }
          }
          content { ... on Issue { id } }
        }
      }
    }
  }
}
EOF
body="$(jq -n --arg q "$RESOLVE" --arg o "$OWNER" --argjson n "$PROJECT" --arg s "$STATUS_FIELD" \
  '{query:$q, variables:{owner:$o, number:$n, statusField:$s}}')"
resp="$(gql "$body")"
code="$(http_of "$resp")"
rb="$(body_of "$resp")"
errs="$(errors_of "$rb")"
proj_id="$(printf '%s' "$rb" | jq -r ".data.${ROOT}.projectV2.id // empty")"
field_id="$(printf '%s' "$rb" | jq -r ".data.${ROOT}.projectV2.field.id // empty")"
n_opt="$(printf '%s' "$rb" | jq ".data.${ROOT}.projectV2.field.options // [] | length")"
if [ "$code" = 200 ] && [ -n "$proj_id" ]; then
  ok "projectV2.id 取得"
else
  ng "projectV2.id が取れない (http ${code})"
fi
if [ -n "$field_id" ] && [ "$n_opt" -gt 0 ]; then
  ok "field(${STATUS_FIELD}).id と option ${n_opt} 件"
else
  ng "Status フィールド / option が取れない（フィールド名違い or 権限不足）"
fi
[ -n "$errs" ] && printf '         errors: %s\n' "$errs"
say ''

# --- 4. mutation（カード移動 = 唯一の write 経路） --------------------------
say '== 4. updateProjectV2ItemFieldValue  （UPDATE_STATUS_MUTATION / 唯一の write） =='
if [ "$WRITE" != 1 ]; then
  sk '--write を付けたときだけ実行する（read だけ測りたい場面があるため）'
elif [ -z "$proj_id" ] || [ -z "$field_id" ]; then
  sk '3 が通っていないので測れない（id が無い）'
else
  # **現在の Status と同じ option を書き戻す**ので盤面は動かない。
  target="$(printf '%s' "$rb" | jq -c "[.data.${ROOT}.projectV2.items.nodes[]
      | select(.status.optionId != null)] | first // empty")"
  if [ -z "$target" ]; then
    sk 'Status が設定済みの item が 1 件も無い — 冪等な write プローブが作れない（推測で書き込みはしない）'
  else
    item_id="$(printf '%s' "$target" | jq -r '.id')"
    opt_id="$(printf '%s' "$target" | jq -r '.status.optionId')"
    opt_nm="$(printf '%s' "$target" | jq -r '.status.name')"
    body="$(jq -n --arg p "$proj_id" --arg i "$item_id" --arg f "$field_id" --arg o "$opt_id" '{
      query: "mutation($project: ID!, $item: ID!, $field: ID!, $option: String!) { updateProjectV2ItemFieldValue(input: { projectId: $project, itemId: $item, fieldId: $field, value: { singleSelectOptionId: $option } }) { projectV2Item { id } } }",
      variables: {project:$p, item:$i, field:$f, option:$o}}')"
    resp="$(gql "$body")"
    code="$(http_of "$resp")"
    mb="$(body_of "$resp")"
    errs="$(errors_of "$mb")"
    wrote="$(printf '%s' "$mb" | jq -r '.data.updateProjectV2ItemFieldValue.projectV2Item.id // empty')"
    if [ "$code" = 200 ] && [ -n "$wrote" ]; then
      ok "同じ option (${opt_nm}) を書き戻せた = write あり（盤面は不変）"
    else
      ng "write できない (http ${code}) ← Projects: Read までしか無い"
    fi
    [ -n "$errs" ] && printf '         errors: %s\n' "$errs"
  fi
fi
say ''

# --- rate ------------------------------------------------------------------
rl="$(curl -sS -H "Authorization: Bearer ${TOKEN}" -H "User-Agent: ${UA}" \
  https://api.github.com/rate_limit 2>/dev/null | jq -c '.resources.graphql // empty' || true)"
[ -n "$rl" ] && say "rate (graphql): ${rl}"

say ''
say "結果: ok=${pass} FAIL=${fail} skip=${skip}"
# skip は「測れなかった」であって pass ではない。終了コードで区別する。
if [ "$fail" -gt 0 ]; then exit 1; fi
if [ "$skip" -gt 0 ]; then exit 3; fi
exit 0
