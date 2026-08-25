#!/usr/bin/env bash
# GitHub トークンの必要権限を**実測**する（#514）。docs の権限表はこれまで
# 導出であって実測ではなかった。
#
#   bash .claude/skills/live-e2e/scripts/github-permissions.sh probe
#   bash .claude/skills/live-e2e/scripts/github-permissions.sh probe --write
#   GH_PROBE_TOKEN='ghp_…' bash .../github-permissions.sh probe --write
#
# 既定のトークンは `.env` の `E2E_GH_TOKEN`。`GH_PROBE_TOKEN` を渡すとそちらを
# 使う（権限を削ったトークンを順に試すため）。**トークンは一切表示しない**し、
# **argv にも載せない**（`ps` から読めるため。curl の設定ファイルを 0600 で作り
# `--config` で渡す）。表示するのは種別（`ghp_` / `github_pat_` / `gho_` …）と
# scope ヘッダだけ。
#
# ## なぜ専用のスクリプトなのか
#
# プラグインが投げるのは **read 3 操作（`fetch_query` / `resolve_query` /
# `VIEWER_QUERY`）＋ write 1 操作（`UPDATE_STATUS_MUTATION`）** の計 4 つ。
# `totsuka doctor --online` はこのうち `viewer` **1 つ**しか叩かない（F-59）ので、
# **doctor が緑でも fetch が空になる権限構成が存在する**。逆に実 `run` は権限
# だけを切り分けられない（LLM・herdr・worktree が同時に動く）。その隙間を埋める。
#
# ## クエリは本番の「先頭ページ」と同じ。ただし resolve だけは足している
#
# `fetch` は `client.rs` の `fetch_query` と**同じ本文**を、`cursor: null`（＝
# 先頭ページ）で投げる。`items(first: 50, after: $cursor)` という件数まで揃えて
# あるのは、権限の判定に関係しなくても**「同じものを測った」と言えるようにする
# ため**である。
#
# `resolve` は本番の `resolve_query` に **`status { name optionId }` を足して
# いる**。冪等な write プローブ（下記）に現在の option id が要るためで、ここだけ
# は本番と同一ではない。足したのは選択フィールド 1 つで、Projects 権限の要否は
# 変わらない。
#
# **測るのは先頭ページだけ**である。`hasNextPage` が真なら「以降のページは見て
# いない」と明示する — 見ていない範囲について結論を書かないため。
#
# ## 一番大事な設計上の点: 「エラーが出なかった」を pass と読まない
#
# GraphQL の権限不足は **HTTP 200 + `data` あり + フィールドが `null`** で
# 出うる（`errors` に `INSUFFICIENT_SCOPES` が並ぶこともあるが、並ばずに
# 黙って `null` になる形もある）。したがってこのスクリプトは
# **フィールド単位で present / null を判定**する。プラグインが実際に読む
# フィールドだけを見る:
#
#   body / labels / assignees / repository.name / title / url / number
#
# 同時に、**`errors` を含む応答はその操作の成功に数えない**。プラグイン本体の
# `check_errors` が errors を拒否する以上、ここだけ緩いと本体と違う結論になる。
#
# 判定は**母集団全体**で行う。`title` / `url` / `number` / `repository.name` は
# 読める Issue なら必ず非 null なので、**1 件でも null なら FAIL** にする
# （「62 件中 1 件だけ読めるトークン」を pass にすると、プラグインが残りを黙って
# 取りこぼす構成を「十分」と呼ぶことになる）。`body` だけは別扱いで、**`null`
# （権限）と `""`（本文が空）を区別する** — 全件 `""` の board では権限の話が
# できないので skip にする。`assignees` / `labels` も同じ理由で
# `nodes: null`（権限）と `nodes: []`（付いていない）を分ける。
#
# ## write プローブは破壊的ではない（ただし前提がある）
#
# `--write` は item の **現在の Status と同じ option** を書き戻す。
# `updateProjectV2ItemFieldValue` は冪等（client.rs の `update_status` が
# 「Idempotent: setting the same option again yields the same state」と書いて
# いるのと同じ性質）なので、権限だけを測って盤面を動かさない。
#
# **前提**: 現在の option を読む resolve と、書き戻す mutation は別リクエスト
# なので、その間に**他の actor が同じ item の Status を変えると古い値へ巻き戻す**。
# 実行中は board を他から触らないこと。Status が未設定の item しかない場合は
# 測れないので skip する（推測で何かを書き込むことはしない）。
#
# ## 対象は public GitHub だけ
#
# プラグインの `GithubConfig` には `api_url`（GitHub Enterprise 用）の上書きが
# あるが、**このスクリプトは対応していない** — endpoint を固定しており、scope と
# rate_limit の問い合わせ先も公開 API である。Enterprise で測る用途には使えない。
#
# ## レート
#
# 1 周の消費は下の `rate` 行が毎回表示する。先頭ページしか引かないので
# `gh project item-list` 経由（102 points）より安い。
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
# [[projects]] の `owner_type` と同じ意味で、GraphQL のルートフィールドが
# 変わる（client.rs の `OwnerType::graphql_root`）。**既定は user なので、org 所有
# の board を測るときは必ず `E2E_GH_OWNER_TYPE=organization` を渡すこと** —
# 渡さないと `user(login:)` を叩いて「board が見えない」と誤報告する。
# config.toml からは読まない（このスクリプトは totsuka の設定に依存せず、
# トークンだけを差し替えて回せることを優先している）。
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
# 本体の `ReqwestTransport` は 30 秒 timeout を持つ。無いと接続が固まったときに
# 最終結果を出さないまま吊る。
CONNECT_TIMEOUT=10
MAX_TIME=30

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

# トークンを argv に載せない。`-H "Authorization: Bearer $TOKEN"` は `ps` から
# 読めるので、0600 の設定ファイル経由で渡す（`printf` はシェル組み込みなので
# ここでも argv には出ない）。
CURLRC="$(mktemp "${TMPDIR:-/tmp}/ghperm.XXXXXX")"
chmod 600 "$CURLRC"
trap 'rm -f "$CURLRC"' EXIT INT TERM
printf 'header = "Authorization: Bearer %s"\nheader = "User-Agent: %s"\n' "$TOKEN" "$UA" >"$CURLRC"

pass=0
fail=0
skip=0   # 測ろうとしたが測れなかった（材料不足）
notrun=0 # 意図的に実行していない（--write 未指定）
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
nr() {
  notrun=$((notrun + 1))
  printf '  [--]   %s\n' "$*"
}

# gql <json-body> — 生の応答を返す。HTTP ステータスは最終行に `#http:<code>`。
#
# **`|| true` が要る。** `set -euo pipefail` の下では `x="$(cmd)"` の終了状態は
# 置換したコマンドのものになるので、curl が接続に失敗した瞬間にスクリプトごと
# 落ち、`結果:` も終了コードの契約（0/1/2/3）も出せない。**診断のための道具が
# 診断できずに死ぬ**のが一番困る。失敗は空文字として下流の判定に渡す。
gql() {
  curl -sS --config "$CURLRC" \
    --connect-timeout "$CONNECT_TIMEOUT" --max-time "$MAX_TIME" \
    -w '\n#http:%{http_code}' \
    -H 'Content-Type: application/json' \
    --data-binary "$1" "$API" || true
}

http_of() { printf '%s' "$1" | tail -1 | sed 's/^#http://'; }
body_of() { printf '%s' "$1" | sed '$d'; }

# jqr <json> <filter> [既定値] — JSON でない応答（502 の HTML、WAF のページ、
# curl が落ちて空）でも落ちずに既定値を返す。**生の `jq` を応答へ直接当てない**
# こと: HTTP ステータスを見る前に走るので、`code != 200` の分岐へ到達する前に
# jq の終了コードでスクリプトが死ぬ。
jqr() {
  printf '%s' "$1" | jq -r "$2" 2>/dev/null || printf '%s' "${3-}"
}
jqc() {
  printf '%s' "$1" | jq -c "$2" 2>/dev/null || printf '%s' "${3-}"
}

# errors_of — GraphQL の errors を 1 行ずつ。空なら何も出さない。
errors_of() {
  printf '%s' "$1" | jq -r '(.errors // [])[] | "\(.type // "-"): \(.message)"' 2>/dev/null || true
}
has_errors() { [ -n "$(errors_of "$1")" ]; }

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
# classic PAT / OAuth はこのヘッダで scope が読める。
hdr="$(curl -sS -D - -o /dev/null --config "$CURLRC" \
  --connect-timeout "$CONNECT_TIMEOUT" --max-time "$MAX_TIME" \
  https://api.github.com/ 2>/dev/null | tr -d '\r' || true)"
# **「ヘッダが無い」と「ヘッダはあるが空」は別物**である。前者は scope 概念を
# 持たないトークン（fine-grained PAT / App）、後者は **scope をひとつも持たない
# classic PAT**。同じ表示にすると、種別の裏取りにならないどころか、直前の
# `kind`（接頭辞から決めたもの）と食い違った表示になる。
scope_line="$(printf '%s' "$hdr" | \grep -i '^x-oauth-scopes:' || true)"
if [ -z "$scope_line" ]; then
  say '  x-oauth-scopes: （ヘッダ自体が無い = fine-grained PAT / App トークン。scope 概念を持たない）'
else
  # `cut -d' ' -f2-` は区切りが無い行（`x-oauth-scopes:` のように末尾スペース
  # 無し）をそのまま通してしまい、**ヘッダ名を scope 値として表示する**。
  # 値はヘッダ名を落としてから取る。
  scopes="$(printf '%s' "$scope_line" | sed 's/^[^:]*: *//')"
  if [ -n "$scopes" ]; then
    say "  x-oauth-scopes: ${scopes}"
  else
    say '  x-oauth-scopes: （ヘッダはあるが空 = scope を 1 つも持たない classic PAT / OAuth）'
  fi
fi
say "  対象: ${ROOT}(login: ${OWNER}) / projectV2(number: ${PROJECT}) / field ${STATUS_FIELD}"
[ "$ROOT" = user ] && say '  ※ owner_type 既定 = user。org 所有 board なら E2E_GH_OWNER_TYPE=organization'
say ''

# --- 1. viewer（疎通。doctor --online が唯一叩く操作） ----------------------
say '== 1. viewer { login }  （VIEWER_QUERY / doctor --online） =='
resp="$(gql '{"query":"query { viewer { login } }"}')"
code="$(http_of "$resp")"
b="$(body_of "$resp")"
errs="$(errors_of "$b")"
login="$(jqr "$b" '.data.viewer.login // empty')"
if [ "$code" = 200 ] && [ -n "$login" ] && ! has_errors "$b"; then
  ok "viewer.login = ${login}  (http ${code})"
else
  ng "viewer が取れない (http ${code})"
fi
[ -n "$errs" ] && printf '         errors: %s\n' "$errs"
say ''

# --- 2. fetch（Project アイテム + Issue の中身） ---------------------------
# client.rs の `fetch_query` と**同じ本文**を cursor: null で投げる。
say '== 2. Project アイテム取得  （fetch_query の先頭ページ / run の read 経路） =='
read -r -d '' FETCH <<EOF || true
query(\$owner: String!, \$number: Int!, \$statusField: String!, \$cursor: String) {
  ${ROOT}(login: \$owner) {
    projectV2(number: \$number) {
      items(first: 50, after: \$cursor) {
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
  '{query:$q, variables:{owner:$o, number:$n, statusField:$s, cursor:null}}')"
resp="$(gql "$body")"
code="$(http_of "$resp")"
b="$(body_of "$resp")"
errs="$(errors_of "$b")"
proj="$(jqc "$b" ".data.${ROOT}.projectV2 // empty")"

if [ "$code" != 200 ]; then
  ng "http ${code} — 応答本文: $(printf '%s' "$b" | head -c 300)"
elif has_errors "$b"; then
  ng "errors を伴う応答（プラグイン本体の check_errors はこれを拒否する）"
elif [ -z "$proj" ] || [ "$proj" = null ]; then
  ng "projectV2 が null（Projects 権限が無い / board が見えない / owner_type 違い）"
else
  # `nodes` が null で返るのは**このプローブが想定する権限不足の形そのもの**
  # なので、`// []` で受けてから数える。ここで jq が落ちると、一番診断したい
  # ケースでサマリごと出なくなる。
  nodes="$(jqc "$b" ".data.${ROOT}.projectV2.items.nodes // []" '[]')"
  n_items="$(printf '%s' "$nodes" | jq 'length')"
  more="$(jqr "$b" ".data.${ROOT}.projectV2.items.pageInfo.hasNextPage // false" false)"
  ok "projectV2 が見える / 先頭ページ ${n_items} 件"
  [ "$more" = true ] && say '         （hasNextPage=true — 以降のページは見ていない。結論は先頭ページについてのみ）'
  issues="$(printf '%s' "$nodes" | jq '[.[] | select(.content.__typename == "Issue")]')"
  n_issue="$(printf '%s' "$issues" | jq 'length')"
  if [ "$n_issue" = 0 ]; then
    sk 'Issue の item が 0 件 — フィールド単位の判定ができない（board に Issue を入れてから再実行）'
  else
    ok "Issue の item ${n_issue} 件を母集団にする"
    # 読める Issue なら必ず非 null になるフィールド。**全件そろって初めて ok**
    # にする（1 件だけ読めるトークンを「十分」と呼ばないため）。
    #
    # `.content` から辿ること。母集団の要素は **item** であって Issue では
    # ないので、`.content` を挟まないと `getpath` は必ず null を返し、
    # **全件 null = 権限不足**と読める偽陽性になる（実際に一度出した）。
    for f in 'title' 'url' 'number' 'repository.name'; do
      got="$(printf '%s' "$issues" | jq --arg f "$f" \
        '[.[] | .content | getpath($f | split(".")) | select(. != null)] | length')"
      if [ "$got" = "$n_issue" ]; then
        ok "content.${f}: ${n_issue}/${n_issue} 件で非 null"
      else
        ng "content.${f}: ${got}/${n_issue} 件しか非 null でない ← 権限不足の疑い（部分的に読めるのは「十分」ではない）"
      fi
    done
    # `body` だけは別扱い。本文が空の Issue では `""` が正しい応答なので、
    # `null`（読めない = 権限）と `""`（空）を分けないと、本文の無い board で
    # 権限が正常でも FAIL になる。
    b_null="$(printf '%s' "$issues" | jq '[.[] | .content.body | select(. == null)] | length')"
    b_nonempty="$(printf '%s' "$issues" | jq '[.[] | .content.body | select(. != null and . != "")] | length')"
    if [ "$b_null" -gt 0 ]; then
      ng "content.body: ${b_null}/${n_issue} 件が null ← 権限不足の疑い（\"\" とは別物）"
    elif [ "$b_nonempty" -gt 0 ]; then
      ok "content.body: 非空 ${b_nonempty}/${n_issue} 件（null は 0 件）"
    else
      sk 'content.body: 全件 "" — 本文が空なだけか判別不能（本文のある Issue を board に入れて再実行）'
    fi
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
# 本番の `resolve_query` に `status { name optionId }` を足したもの（冒頭の
# コメント参照）。足した分は write プローブ専用で、権限の要否は変わらない。
say '== 3. project / Status フィールド / item の id 解決  （resolve_query + status） =='
read -r -d '' RESOLVE <<EOF || true
query(\$owner: String!, \$number: Int!, \$statusField: String!, \$cursor: String) {
  ${ROOT}(login: \$owner) {
    projectV2(number: \$number) {
      id
      field(name: \$statusField) {
        ... on ProjectV2SingleSelectField { id options { id name } }
      }
      items(first: 100, after: \$cursor) {
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
  '{query:$q, variables:{owner:$o, number:$n, statusField:$s, cursor:null}}')"
resp="$(gql "$body")"
code="$(http_of "$resp")"
rb="$(body_of "$resp")"
errs="$(errors_of "$rb")"
proj_id="$(jqr "$rb" ".data.${ROOT}.projectV2.id // empty")"
field_id="$(jqr "$rb" ".data.${ROOT}.projectV2.field.id // empty")"
n_opt="$(jqr "$rb" ".data.${ROOT}.projectV2.field.options // [] | length" 0)"
r_more="$(jqr "$rb" ".data.${ROOT}.projectV2.items.pageInfo.hasNextPage // false" false)"
if [ "$code" = 200 ] && [ -n "$proj_id" ] && ! has_errors "$rb"; then
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
  # **意図して実行していない**ので skip（= 測れなかった）とは別に数える。
  # read だけを測る通常の呼び出しが「未測定」に見えると、終了コードの意味が
  # 壊れる。
  nr '--write を付けたときだけ実行する（read だけ測りたい場面があるため）'
elif [ -z "$proj_id" ] || [ -z "$field_id" ]; then
  sk '3 が通っていないので測れない（id が無い）'
else
  # **現在の Status と同じ option を書き戻す**ので盤面は動かない。
  # 前提: この resolve と下の mutation の間に他の actor が同じ item の Status を
  # 変えないこと（冒頭のコメント参照）。
  target="$(jqc "$rb" "[.data.${ROOT}.projectV2.items.nodes // [] | .[] | select(.status.optionId != null)] | first // empty")"
  if [ -z "$target" ]; then
    if [ "$r_more" = true ]; then
      sk 'Status 設定済みの item が先頭ページに無い（hasNextPage=true なので board 全体の結論ではない）'
    else
      sk 'Status が設定済みの item が 1 件も無い — 冪等な write プローブが作れない（推測で書き込みはしない）'
    fi
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
    wrote="$(jqr "$mb" '.data.updateProjectV2ItemFieldValue.projectV2Item.id // empty')"
    if [ "$code" = 200 ] && [ -n "$wrote" ] && ! has_errors "$mb"; then
      ok "同じ option (${opt_nm}) を書き戻せた = write あり（盤面は不変）"
    else
      # 原因を断定しない。ここで確かめたのは「200 で item id が返らなかった」
      # だけで、権限のほか resolve からの id が古い（TOCTOU）・SSO/SAML 未認可・
      # 二次レート制限・一時的な 5xx・Project 側の自動化 も同じ形になる。
      # 上の 2 つの `ng` は疑いとして書いているのに、ここだけ断定していた。
      ng "write が通らない (http ${code}) ← Projects の write 権限が無い疑い（他に SSO 未認可・二次レート制限・id の陳腐化でも同じ形になる。errors を見ること）"
    fi
    [ -n "$errs" ] && printf '         errors: %s\n' "$errs"
  fi
fi
say ''

# --- rate ------------------------------------------------------------------
rl="$(curl -sS --config "$CURLRC" \
  --connect-timeout "$CONNECT_TIMEOUT" --max-time "$MAX_TIME" \
  https://api.github.com/rate_limit 2>/dev/null | jq -c '.resources.graphql // empty' || true)"
[ -n "$rl" ] && say "rate (graphql): ${rl}"

say ''
say "結果: ok=${pass} FAIL=${fail} skip=${skip} 未実行=${notrun}"
# 終了コードは 3 状態を区別する。skip は「測ろうとしたが材料が無かった」で
# あって pass ではない。未実行（--write なし）は**意図した通り**なので 0。
if [ "$fail" -gt 0 ]; then exit 1; fi
if [ "$skip" -gt 0 ]; then exit 3; fi
exit 0
