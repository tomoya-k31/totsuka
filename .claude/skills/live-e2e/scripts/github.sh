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
#   bash .claude/skills/live-e2e/scripts/github.sh prime-item <web|cli> <issue#> <item-id>  # item id をキャッシュ
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
# **不変な id はキャッシュしてある**（下記 `cached`）。ただし**効き方は一様ではない**:
#
#   project id / field・option id  … プロジェクト単位。2 回目以降は常に 0 points
#   item id                        … **item 単位**。S1 は毎回新しい issue を作るので、
#                                     新しい issue では必ずミスして 102 points 払う
#
# したがって「212 → 1 point」になるのは**同じ item へ 2 回目以降の `set_status` を
# 打ったとき**だけ。S1 を 1 周する実際の消費はこちら:
#
#   初回              seed 212 + verify 102 = 314
#   2 周目以降(新 issue) seed 103 + verify 102 = 205   （約 35% 減）
#
# `item-add --format json` が返す item id をキャッシュへ直接書けば seed 側の 102 も
# 消せる（`prime-item` サブコマンド。scenarios.md の S1 手順が使っている）。
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
# **消し方**: `rm -rf "$E2E_HOME/state/live-e2e/cache"`。消すべき場面:
#
#   - Project の Status option を**編集または追加**した（追加でも刺さる。
#     古い `field_ids` に新しい option が無いので「実在するのに存在しません」）
#   - Project から item を外して入れ直した（item id が変わる）
#   - `E2E_GH_OWNER` を切り替えた（キーに owner を含めてあるので通常は当たらないが、
#     同じ owner で Project を作り直した場合は当たる）
#
# 「これ以外で古くなることは無い」とは書かない — 書けるだけの根拠が無い。
# 迷ったら消してよい（初回だけ 200 points 払い直すだけ）。
cache_dir() { echo "${E2E_HOME}/state/live-e2e/cache"; }
# キャッシュファイルの末尾に必ず置く行。**切れたファイルを確実に見分けるため**で、
# 行数のような近似ではない（`field_ids` を 2 行に切っても「2 行以上」は通る）。
CACHE_EOF='#--- complete ---'

cached() {  # cached <key> <コマンド...>  — 標準出力をキャッシュして返す
  local key="$1"; shift
  local f; f="$(cache_dir)/${key}"
  # 書く側は tmp+mv で不可分なので**切れたファイルは生まれない**。読む側の
  # マーカー検査は、外から壊された場合（手で消しかけた・別ツールが触った）に
  # **黙って部分的な内容を配らない**ためにある。`[ -s ]` は空しか弾けず、
  # 切れたファイルは非空なので素通りする — `field_ids` で option 行が落ちると
  # 「実在する option を存在しないと言う」形になり、原因に辿り着けない。
  if [ -s "$f" ]; then
    if [ "$(tail -1 "$f")" = "$CACHE_EOF" ]; then sed '$d' "$f"; return 0; fi
    echo "キャッシュが途中で切れています: $f → 取り直します" >&2
    rm -f "$f"
  fi
  mkdir -p "$(cache_dir)"
  # **空をキャッシュしない。** 一時的な失敗を永続化すると、以降ずっと壊れる。
  local out; out="$("$@")" || return 1
  [ -n "$out" ] || return 1
  # **一時ファイル + rename で書く。** `tee "$f"` は `$f` を直接 truncate する
  # ので、Ctrl-C・同時実行・ディスク満杯で**途中まで書けたファイル**が残る。
  # 「空はキャッシュしない」ガードが守るのは空だけで、**切れたファイルは非空
  # なので `[ -s ]` を素通りする** — `field_ids` は複数行なので、option 行が
  # 落ちて「実在する option を存在しないと言う」形で刺さる。同一 FS の rename
  # は不可分なので、これで「全部あるか、無いか」になる。
  printf '%s\n%s\n' "$out" "$CACHE_EOF" > "$f.tmp.$$" && mv -f "$f.tmp.$$" "$f"
  printf '%s\n' "$out"
}

field_ids_uncached() {
  gh project field-list "${E2E_GH_PROJECT}" --owner "$OWNER" --format json | python3 -c '
import json,sys
f=[x for x in json.load(sys.stdin)["fields"] if x["name"]=="Status"][0]
print(f["id"])
for o in f["options"]: print("%s\t%s" % (o["name"], o["id"]))'
}
field_ids() { cached "fields-${OWNER}-${E2E_GH_PROJECT}" field_ids_uncached; }

project_id_uncached() {
  gh project view "${E2E_GH_PROJECT}" --owner "$OWNER" --format json \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'
}
project_id() { cached "project-${OWNER}-${E2E_GH_PROJECT}" project_id_uncached; }

# item の id も、その item が Project に**在り続ける限り**変わらない。
# `item-list --limit 100` は 102 points なので、ここも 2 回目以降は 0 にする。
#
# **Project から item を外して入れ直すと id は変わる。** そのときは
# `rm -rf "$E2E_HOME/state/live-e2e/cache"`。e2e の通常の流れ（検証ごとに
# 新しい issue を作る）では別のキーになるので当たらない。
item_id() { cached "item-${OWNER}-${E2E_GH_PROJECT}-$1-$2" item_id_uncached "$1" "$2"; }

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
  # `|| true` が要る。`item_id` は `cached` 経由になったので、item が無いとき
  # **空を返すのではなく `return 1`** する。`set -euo pipefail` 下では代入の
  # 終了ステータスがコマンド置換のものになるため、これが無いと**次の行に
  # 到達せず無言で落ちる** — 「Project に入っていない issue に打った」ことを
  # 人間へ伝える唯一の出口が消える。
  local iid; iid="$(item_id "$1" "$2" || true)"
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

# 引数の検証を 1 箇所に集める。**キャッシュキーと GitHub のパスに入る値**なので、
# `repo_of` の `*)` が任意文字列を素通しするままだと `../` がキーへ入る（A-6）。
# 終了コードも揃える — `${2:?…}` は非対話 bash では exit 1 になるので、同じ
# 「引数の指定ミス」で 1 と 2 が混ざっていた。
need_target() {  # need_target <サブコマンド名> <web|cli> <issue#>
  local cmd="$1"; shift
  case "${1:-}" in
    web|cli) ;;
    *) echo "usage: github.sh $cmd <web|cli> <issue#>" >&2; exit 2;;
  esac
  case "${2:-}" in
    ''|*[!0-9]*) echo "usage: github.sh $cmd <web|cli> <issue#>  — issue 番号は数字です" >&2; exit 2;;
  esac
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
  need_target seed "$@"
  st="${3:-Todo}"; r="$(repo_of "$1")"; n="$2"
  # **`wait` のための基準時刻を、Status を倒す「前」に残す。**
  #
  # これが無いと `wait` は「この issue のタスク」を正しく見つけたうえで、それが
  # **前回の実行の done タスク**でも即座に成功と報告する（使い回しの issue で
  # 必ず起きる）。
  #
  # 順序が重要で、`set_status` の**後**に書くと取りこぼす: `set_status` は
  # GraphQL を数往復するので数秒かかり、その間に poll（既定 60 秒間隔）が
  # 走ると、**本物のタスクの `updated_at` が基準より前**になって STALE 扱いに
  # なる。先に書けば基準は必ず取り込みより前になる — 誤って**古いほうへ倒れる**
  # ことはあっても、**新しいものを取りこぼすことはない**。
  mkdir -p "$E2E_HOME/state/live-e2e"
  date -u +%Y-%m-%dT%H:%M:%SZ > "$E2E_HOME/state/live-e2e/seed-$r-$n"
  set_status "$r" "$n" "$st"
  echo "$r#$n → $st"
  ;;
prime-item)
  # `gh project item-add --format json` が返した item id を、`item_id` の
  # キャッシュへ直接入れる。**S1 は毎回新しい issue を作るので、これが無いと
  # `set_status` のたびに `item-list --limit 100` の 102 points を払う。**
  # 冒頭のレート表が「item-add が返す id を使い、item-list を引き直さない」と
  # 書いていた正解を、手順から呼べる形にしたもの。
  need_target prime-item "$@"
  r="$(repo_of "$1")"; n="$2"; iid="${3:?item id が要ります（item-add --format json の .id）}"
  mkdir -p "$(cache_dir)"
  printf '%s\n%s\n' "$iid" "$CACHE_EOF" > "$(cache_dir)/item-${OWNER}-${E2E_GH_PROJECT}-$r-$n"
  echo "$r#$n の item id をキャッシュしました（item-list の 102 points を節約）"
  ;;
clear)
  need_target clear "$@"
  set_status "$(repo_of "$1")" "$2" --; echo "$(repo_of "$1")#$2 → (none)"
  ;;
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
  need_target wait "$@"
  r="$(repo_of "$1")"; n="$2"; limit="${3:-1800}"
  # `|| true` が要る。`set -euo pipefail` 下では代入の終了ステータスがコマンド
  # 置換のものになるので、これが無いと `gh` の失敗（issue が無い・未認証）で
  # **次の行へ到達せず**、用意した案内文と exit 2 が到達不能になる。
  node="$(gh issue view "$n" --repo "$OWNER/$r" --json id --jq .id 2>/dev/null || true)"
  [ -n "$node" ] || { echo "issue $r#$n の node id を取得できません" >&2; exit 2; }
  # seed 時刻より後に動いたタスクだけを受け付ける。無ければ「基準なし」で走るが、
  # **前回の done を掴みうることを明示して警告する** — 黙って通すのが一番悪い。
  # `-f` ではなく `-s`。**0 バイトのファイルは `-f` を通る**ので、基準が空のまま
  # 「seed 以降のみ: 」と表示して検査だけ消える、という一番わかりにくい形になる。
  since_file="$E2E_HOME/state/live-e2e/seed-$r-$n"
  since="$([ -s "$since_file" ] && cat "$since_file" || true)"
  if [ -n "$since" ]; then
    echo "待機対象: $r#$n ($node) / seed 以降のみ: $since"
  elif [ "${ALLOW_NO_BASELINE:-0}" = "1" ]; then
    echo "待機対象: $r#$n ($node)"
    echo "  警告: seed の記録がありません（ALLOW_NO_BASELINE=1）。" >&2
    echo "        前回の実行の done タスクを掴む可能性があります。" >&2
  else
    # **警告だけ出して exit 0 は「黙って通す」と同じ。** `&&` チェーンや
    # エージェントから駆動している限り PASS として通ってしまう。既定は落とす。
    echo "seed の記録がありません: $since_file" >&2
    echo "  そのまま待つと**前回の実行の done タスク**を掴んで即座に成功と報告します。" >&2
    echo "  github.sh seed を通してから待つか、承知のうえで ALLOW_NO_BASELINE=1 を付けてください。" >&2
    exit 2
  fi
  deadline=$(( $(date +%s) + limit ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    # **観測の故障を「まだ取り込まれていません」に化けさせない。** 以前は
    # `tt` と python の両方に `2>/dev/null` が掛かっていたので、state DB が
    # 開けない・`--json` の形が違う・python が無い、のすべてが空文字列に潰れ、
    # 「取り込み待ち」と同じ見た目で 1800 秒を使い切っていた。
    if ! tasks_json="$(tt task list --json 2>&1)"; then
      echo "tt task list が失敗しました:" >&2; echo "$tasks_json" >&2; exit 2
    fi
    line="$(printf '%s' "$tasks_json" | NODE="$node" SINCE="$since" python3 -c '
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
')" || { echo "タスク一覧を読めませんでした（tt task list --json の形が変わった可能性）" >&2; exit 2; }
    case "$line" in
      STALE*) echo "$(date +%H:%M:%S) （seed 前の古いタスクのみ: ${line#STALE	}）"; sleep 20; continue;;
    esac
    if [ -z "$line" ]; then
      echo "$(date +%H:%M:%S) （まだ取り込まれていません）"
    else
      echo "$(date +%H:%M:%S) task $line"
      # **state だけを完全一致で見る。** 行全体に対する `*done*` だと
      # **タスクの title にも当たる** — `line` は `<id>\t<state>\t<title>` で、
      # title は issue のタイトルそのものだからである。`feat: done 判定の
      # リグレッション検収` のようなタイトルは live-e2e でむしろ自然で、
      # それだけで state が `queued` のまま「終端に達した」と報告してしまう。
      # **この PR が塞いだ「赤くならずに緑になる」を別の入口から入れ直さない。**
      #
      # `cancelled` も終端（`domain/state.rs` の `is_terminal()` は
      # done / failed / cancelled）。`verifying` は人間が `tt task verify` を
      # 叩くまで動かないので、`verification = "human"` を回すときはここへ足す。
      state="${line#*$'\t'}"; state="${state%%$'\t'*}"
      case "$state" in
        done|failed|cancelled|escalated|waiting_input) exit 0;;
      esac
    fi
    sleep 20
  done
  echo "タイムアウト（${limit}s）"; exit 1
  ;;
verify)
  need_target verify "$@"
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

  # **「今回の run が push したか」を見る。累積数ではない。**
  #
  # 以前はサンドボックス全体のブランチ数と PR 数が閾値以上かを見ていた。
  # ブランチも PR も run をまたいで残るので、**2 回目以降はエージェントが
  # 1 行も push せず PR も作らなくても両方 `[ok]` になる**。しかも S1 は
  # 「毎回新しい issue を作る」ので、前回までの残骸が必ず存在する。
  # `wait` が正しく止めた直後に `verify` が緑を出す構図だった。
  #
  # 基準は `seed` が残す時刻（`$since_file`）。無ければ判定できないので、
  # **緑にせず「判定不能」として ng に数える** — 判定できないことを
  # 「合格」と読み替えないのがこのファイル全体の方針。
  since_file="$E2E_HOME/state/live-e2e/seed-$r-$n"
  if [ -s "$since_file" ]; then
    since="$(cat "$since_file")"
    # `--json` は作成時刻を持つので、seed 以降に作られたものだけを数える。
    fresh_prs="$(gh pr list -R "$OWNER/$r" --state all --limit 100 --json number,createdAt,headRefName \
      | SINCE="$since" python3 -c '
import json, os, sys
since = os.environ["SINCE"][:19]
rows = [p for p in json.load(sys.stdin) if (p.get("createdAt") or "")[:19] >= since]
print(json.dumps([{"number": p["number"], "head": p["headRefName"]} for p in rows]))')"
    count="$(echo "$fresh_prs" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
    if [ "$count" -ge 1 ]; then
      heads="$(echo "$fresh_prs" | python3 -c 'import json,sys; print(", ".join("#%d %s" % (p["number"], p["head"]) for p in json.load(sys.stdin)))')"
      chk 1 "ADR-0026 この run で PR が作られた（${heads}）"
      chk 1 "F-86 そのブランチが push されている（PR がある＝push 済み）"
    else
      chk 0 "ADR-0026 この run で作られた PR が無い（seed 以降: ${since}）"
      chk 0 "F-86 この run で push されたブランチが無い"
    fi
  else
    chk 0 "F-86 / ADR-0026 は判定できません（seed の記録が無い。github.sh seed を通してください）"
  fi

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
