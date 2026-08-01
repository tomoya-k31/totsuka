#!/usr/bin/env bash
# okf-lint.sh — OKF v0.2 バンドル検証（依存: bash 3.2+, POSIX awk/grep/sed のみ）
#
# 構造チェック:
#   [E] frontmatter : index.md / log.md 以外の全 .md が YAML frontmatter を持つ
#   [E] fm-yaml     : frontmatter が YAML として壊れていない（後述の部分集合で検証）
#   [E] type        : frontmatter に空でない `type` がある (SPEC §4.1 REQUIRED)
#   [E] description : frontmatter に空でない `description` がある (docs/CLAUDE.md)
#   [E] index-fm    : ルート以外の index.md が frontmatter を持たない (SPEC §8/§11)
#   [E] index-exists: concept/サブディレクトリを含むディレクトリに index.md がある
#   [E] index-listed: 各 concept / サブディレクトリが index.md からリンクされている
#   [E] index-desc  : index.md の転記が concept の description と一致する
#   [E] log-format  : log.md の日付見出しが `## YYYY-MM-DD` 形式
#   [E] log-sync    : log.md が log.d/ の断片と一致する（断片名の規約違反もここ）
#   [E] index-sync  : 各 index.md のマーカー区間が concept と一致する
#
# log-sync / index-sync の判定は scripts/okf-log-build.sh / okf-index-build.sh の
# --check へ**委譲**する（#360）。ここに書き写すと同じ規約が 2 箇所に生まれ、
# 「lint は通るのに生成すると差分が出る」が必ずいつか起きる。ビルダーが正本。
# log.d/ 自体は走査対象外 — frontmatter も index 掲載も要らない材料置き場である。
#
# v0.2 のファミリ（§5 / §10）に対するチェック:
#   [E] status      : `status` が draft | stable | deprecated のいずれか (§5.4)
#   [E] actor       : `generated.by` / `verified[].by` が actor 記法 (§7)
#   [E] generated   : `generated` があるとき `by` が必須 (§5.2)
#   [E] datetime    : `generated.at` / `verified[].at` が ISO 8601、
#                     `stale_after` / `last_modified` / `usage_window` が YYYY-MM-DD
#   [E] sources     : `sources` の各エントリに `resource` がある (§5.1)
#   [E] computation : `type: Attested Computation` に `runtime` がある (§10.2)
#   [E] legacy      : v0.1 の `timestamp` / 本文 `# Citations` が残っていない (§13.1)
#   [W] stale       : `stale_after` を過ぎている (§5.5)
#   [W] footnote    : 本文の脚注ラベルに対応する `sources[].id` がある (§5.1)
#   [W] okf-version : ルート index.md の `okf_version` が "0.2"
#
# fm-yaml について: YAML パーサを外部依存として持ち込まず、本バンドルが使う部分集合を
# awk で厳格に検証する。v0.2 で `sources` / `parameters` / `executor` 等のネストが
# 必要になったため、v0.1 の「1 行 1 キーの平坦なマッピング」から次の部分集合へ広げた:
#   - インデントは 0 / 2 / 4 スペースのみ（タブ禁止）
#   - ブロックシーケンス項目は `  - ` （インデント 2）で始める
#   - シーケンス項目のマッピング継続行はインデント 4
#   - 複数行スカラー（`|` / `>`）とアンカー/エイリアスは使わない
# 狙いは v0.1 から変わらず「パースは通るが値が壊れる」型の事故を落とすこと:
#   - 引用符なしスカラー中の ` #` 以降がコメントとして捨てられる（PR #303）
#   - 引用符なしスカラー中の `: ` で YAML がマッピング解釈に入り失敗する
#   - 値の欠落・キーの重複・引用符やフロー表記の閉じ忘れ・タブ
#
# リンク切れ検査は lychee に委譲（インストール済みの場合のみ / --no-links でスキップ）:
#   lychee --offline --root-dir <bundle> <bundle>
#
# 使い方: scripts/okf-lint.sh [bundleDir=docs] [--strict] [--no-links]
#   --strict   : リンク切れ(lychee)もエラー扱いにする（既定は警告）
# 終了コード: エラー1件以上で 1
set -u

BUNDLE="docs"
STRICT=0
NO_LINKS=0
for a in "$@"; do
  case "$a" in
  --strict) STRICT=1 ;;
  --no-links) NO_LINKS=1 ;;
  --*)
    echo "unknown option: $a" >&2
    exit 2
    ;;
  *) BUNDLE="$a" ;;
  esac
done

[ -d "$BUNDLE" ] || {
  echo "okf-lint: bundle directory not found: $BUNDLE" >&2
  exit 1
}
# 末尾スラッシュ除去
BUNDLE="${BUNDLE%/}"

# awk の出力を `IFS` で割るための実タブ（bash 3.2 には $'\t' があるが、
# 意図を明示するために 1 箇所に置く）
TAB="$(printf '\t')"

# stale_after の期限判定に使う基準日
TODAY="$(date +%Y-%m-%d)"

# 本バンドルが宣言する OKF バージョン
OKF_VERSION="0.2"

ERRORS=0
WARNINGS=0
error() {
  echo "ERROR [$1] $2: $3"
  ERRORS=$((ERRORS + 1))
}
warn() {
  echo "WARN  [$1] $2: $3"
  WARNINGS=$((WARNINGS + 1))
}

# index への掲載を要求しないメタファイル
is_exempt() { case "$1" in README.md | CLAUDE.md) return 0 ;; *) return 1 ;; esac }
is_reserved() { case "$1" in index.md | log.md) return 0 ;; *) return 1 ;; esac }

# frontmatter 判定: 1行目が --- で、2行目以降に閉じ --- があるか (0=あり)
has_frontmatter() {
  awk 'NR==1 { if ($0 != "---") { exit } ; next }
       $0 == "---" { ok=1; exit }
       END { exit (ok ? 0 : 1) }' "$1"
}

# frontmatter ブロック内に空でない type: があるか (0=あり)
has_type() {
  # frontmatter ブロック（1行目---から次の---まで）を切り出して type: を探す
  awk 'NR==1 && $0=="---" {fm=1; next}
       fm && $0=="---" {exit}
       fm {print}' "$1" | grep -Eq '^type:[[:space:]]*[^[:space:]]'
}

# frontmatter を「インデント 0/2/4 の限定ネスト」として厳格に検証し、あわせて
# v0.2 のファミリ（§5 / §10）の意味的な検査を行う。
# 第2引数が 1 のとき `description` の存在も要求する（index.md / log.md は 0）。
# 出力: 問題ごとに "<E|W>\t<check>\t<detail>" を 1 行。問題なしなら無出力。
fm_lint() {
  awk -v need_desc="$2" -v today="$3" -v want_ver="$4" '
    function trim(s) { sub(/^[ ]+/, "", s); sub(/[ ]+$/, "", s); return s }
    function err(chk, msg) { print "E\t" chk "\t" msg }
    function wrn(chk, msg) { print "W\t" chk "\t" msg }

    function is_date(s) { return s ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/ }
    function is_datetime(s) {
      # 日付のみ / 日付+時刻（Z または ±hh:mm）を許容
      if (is_date(s)) return 1
      return s ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9](\.[0-9]+)?(Z|[+-][0-9][0-9]:[0-9][0-9])$/
    }
    function unquote(s,   n, h) {
      n = length(s); if (n < 2) return s
      h = substr(s, 1, 1)
      if ((h == "\"" || h == "'"'"'") && substr(s, n, 1) == h) return substr(s, 2, n - 2)
      return s
    }
    # actor 記法 (§7): human:<id> / process:<id> / <producer>/<version>
    function is_actor(s) {
      if (s ~ /^human:.+/) return 1
      if (s ~ /^process:.+/) return 1
      if (s ~ /^[^\/ ]+\/[^\/ ]+$/) return 1
      return 0
    }
    # フローマッピング `{ k: v, k: v }` を arr へ展開する
    function parse_flow(s, arr,   n, i, p, k, v, parts) {
      split("", arr)
      sub(/^\{[ ]*/, "", s); sub(/[ ]*\}$/, "", s)
      n = split(s, parts, ",")
      for (i = 1; i <= n; i++) {
        p = index(parts[i], ":")
        if (p == 0) continue
        k = trim(substr(parts[i], 1, p - 1))
        v = trim(substr(parts[i], p + 1))
        arr[k] = unquote(v)
      }
    }
    # 「パースは通るが値が壊れる」型のスカラー検査
    function check_scalar(key, val, ln,   head, last, close_ch) {
      if (val == "") return
      head = substr(val, 1, 1)
      last = substr(val, length(val), 1)

      if (head == "\"" || head == "'"'"'") {
        if (length(val) < 2 || last != head)
          err("fm-yaml", ln " 行目: `" key "` の引用符が閉じていない")
        return
      }
      if (head == "[" || head == "{") {
        close_ch = (head == "[") ? "]" : "}"
        if (last != close_ch)
          err("fm-yaml", ln " 行目: `" key "` のフロー表記 `" head "` が `" close_ch "` で閉じていない")
        else if (index(val, " #") > 0)
          err("fm-yaml", ln " 行目: `" key "` の値にコメント開始 ` #` がある（`#` を含む語は引用符で囲む）")
        return
      }

      # 引用符なしの平文スカラー
      if (head == "#")
        err("fm-yaml", ln " 行目: `" key "` の値が `#` で始まり全体がコメントとして捨てられる（引用符で囲む）")
      else if (index(val, " #") > 0)
        err("fm-yaml", ln " 行目: `" key "` の値の ` #` 以降が YAML コメントとして捨てられる（引用符で囲む）")
      else if (index(val, ": ") > 0 || last == ":")
        err("fm-yaml", ln " 行目: `" key "` の平文スカラーに `: ` があり YAML がマッピングとして解釈して失敗する（引用符で囲む）")
      else if (head=="&" || head=="*" || head=="!" || head=="|" || head==">" || head=="%" || head=="@" || head=="`" || head==",")
        err("fm-yaml", ln " 行目: `" key "` の値が YAML の指示文字 `" head "` で始まる（引用符で囲む）")
      # `-` と `?` は「直後に空白があるとき」だけ指示文字（ブロックシーケンス
      # 項目 / 明示キー）になる。`-1` や `?foo` は正しい平文スカラーなので、
      # 上の一覧に混ぜると誤検出になる。
      else if ((head=="-" || head=="?") && (length(val)==1 || substr(val, 2, 1) == " "))
        err("fm-yaml", ln " 行目: `" key "` の値が `" head " ` で始まりブロック構造として解釈される（引用符で囲む）")
    }

    { raw[NR] = $0 }

    END {
      if (NR == 0 || raw[1] != "---") exit
      fmend = 0
      for (i = 2; i <= NR; i++) if (raw[i] == "---") { fmend = i; break }
      if (fmend == 0) {
        # pass2 の index-desc ガードは has_frontmatter を通さずここを直接呼ぶ。
        # 自己完結させておかないと、本文を frontmatter と誤読したまま
        # 「壊れていない」と判定してしまう。
        err("fm-yaml", "frontmatter の終端 `---` がない（本文が frontmatter として読まれる）")
        exit
      }

      pend_key = ""; pend_ln = 0; pend_ind = -1
      cur_top = ""; seq_n = 0; in_item = 0
      nver = 0; nsrc = 0

      for (i = 2; i < fmend; i++) {
        line = raw[i]
        if (index(line, "\t") > 0) { err("fm-yaml", i " 行目: タブ文字は YAML で使えない"); continue }
        if (line ~ /^[ ]*$/) continue
        if (line ~ /^[ ]*#/) continue

        ind = match(line, /[^ ]/) - 1
        body = substr(line, ind + 1)

        if (ind != 0 && ind != 2 && ind != 4) {
          err("fm-yaml", i " 行目: インデントは 0 / 2 / 4 スペースのみ（本バンドルの部分集合）")
          continue
        }
        # 直前の「値が空のキー」はネストを従えていなければならない
        if (pend_key != "") {
          if (ind <= pend_ind)
            err("fm-yaml", pend_ln " 行目: `" pend_key "` の値が空（YAML では null になる。ネストを続けるなら次行を字下げする）")
          pend_key = ""
        }

        is_item = 0
        if (ind == 2 && body ~ /^-([ ]|$)/) { is_item = 1; body = trim(substr(body, 2)) }

        if (ind == 0) {
          cur_top = ""; seq_n = 0; in_item = 0
          if (body !~ /^[A-Za-z_][A-Za-z0-9_-]*:/) {
            err("fm-yaml", i " 行目: `key: value` の形になっていない")
            continue
          }
          c = index(body, ":")
          key = substr(body, 1, c - 1); rest = substr(body, c + 1)
          if (rest != "" && substr(rest, 1, 1) != " ") {
            err("fm-yaml", i " 行目: `" key ":` の後に半角スペースが要る"); continue
          }
          if (key in topkeys) {
            err("fm-yaml", i " 行目: キー `" key "` が重複している（後の値で上書きされる）"); continue
          }
          topkeys[key] = 1
          val = trim(rest)
          topval[key] = val; topln[key] = i
          cur_top = key
          if (val == "") { pend_key = key; pend_ln = i; pend_ind = 0; continue }
          check_scalar(key, val, i)

          # 単一マッピングで書かれた generated / verified / usage_window を展開する
          if (substr(val, 1, 1) == "{") {
            parse_flow(val, F)
            if (key == "generated") { gen_by = F["by"]; gen_at = F["at"]; gen_ln = i; gen_seen = 1 }
            else if (key == "verified") { nver++; ver_by[nver] = F["by"]; ver_at[nver] = F["at"]; ver_ln[nver] = i }
            else if (key == "usage_window") {
              if (F["from"] != "" && !is_date(F["from"]))
                err("datetime", i " 行目: `usage_window.from` は YYYY-MM-DD 形式で書く (§5.1)。実際の値: " F["from"])
              if (F["to"] != "" && !is_date(F["to"]))
                err("datetime", i " 行目: `usage_window.to` は YYYY-MM-DD 形式で書く (§5.1)。実際の値: " F["to"])
            }
          }
          continue
        }

        if (cur_top == "") { err("fm-yaml", i " 行目: 対応する親キーが無い字下げ行"); continue }

        if (is_item) {
          if (ind != 2) { err("fm-yaml", i " 行目: ブロックシーケンス項目はインデント 2 で書く"); continue }
          seq_n++; in_item = 1; split("", itemkeys)
          if (body == "") { err("fm-yaml", i " 行目: `- ` の後に値が無い"); continue }
          if (substr(body, 1, 1) == "{") {
            check_scalar(cur_top "[" seq_n "]", body, i)
            parse_flow(body, F)
            if (cur_top == "verified") { nver++; ver_by[nver] = F["by"]; ver_at[nver] = F["at"]; ver_ln[nver] = i }
            else if (cur_top == "sources") { nsrc++; src_res[nsrc] = F["resource"]; src_id[nsrc] = F["id"]; src_lm[nsrc] = F["last_modified"]; src_ln[nsrc] = i }
            in_item = 0
            continue
          }
          if (body !~ /^[A-Za-z_][A-Za-z0-9_-]*:/) {
            check_scalar(cur_top "[" seq_n "]", body, i)  # 平文スカラー項目
            in_item = 0
            continue
          }
          if (cur_top == "sources") { nsrc++; src_ln[nsrc] = i }
          else if (cur_top == "verified") { nver++; ver_ln[nver] = i }
          # 以降は下の共通処理でキー/値を拾う
        } else if (ind == 4 && !in_item) {
          err("fm-yaml", i " 行目: インデント 4 はシーケンス項目のマッピング継続行にだけ使う"); continue
        }

        if (body !~ /^[A-Za-z_][A-Za-z0-9_-]*:/) {
          err("fm-yaml", i " 行目: `key: value` の形になっていない"); continue
        }
        c = index(body, ":")
        key = substr(body, 1, c - 1); rest = substr(body, c + 1)
        if (rest != "" && substr(rest, 1, 1) != " ") {
          err("fm-yaml", i " 行目: `" key ":` の後に半角スペースが要る"); continue
        }
        scope = in_item ? ("item" seq_n) : ("map:" cur_top)
        if ((scope SUBSEP key) in subkeys) {
          err("fm-yaml", i " 行目: キー `" key "` が同じマッピング内で重複している"); continue
        }
        subkeys[scope, key] = 1
        val = trim(rest)
        if (val == "") { pend_key = key; pend_ln = i; pend_ind = ind; continue }
        check_scalar(key, val, i)
        val = unquote(val)

        if (in_item && cur_top == "sources") {
          if (key == "resource") src_res[nsrc] = val
          else if (key == "id") src_id[nsrc] = val
          else if (key == "last_modified") src_lm[nsrc] = val
        } else if (in_item && cur_top == "verified") {
          if (key == "by") ver_by[nver] = val
          else if (key == "at") ver_at[nver] = val
        } else if (!in_item && cur_top == "generated") {
          gen_seen = 1; gen_ln = topln["generated"]
          if (key == "by") gen_by = val
          else if (key == "at") gen_at = val
        } else if (!in_item && cur_top == "usage_window") {
          if ((key == "from" || key == "to") && !is_date(val))
            err("datetime", i " 行目: `usage_window." key "` は YYYY-MM-DD 形式で書く (§5.1)")
        }
      }
      if (pend_key != "")
        err("fm-yaml", pend_ln " 行目: `" pend_key "` の値が空（YAML では null になる）")

      # ---- v0.2 ファミリの意味的検査 ----
      if (need_desc == 1 && !("description" in topkeys))
        err("description", "frontmatter に空でない `description` がない（index 生成に使うため必須 / docs/CLAUDE.md）")

      if ("timestamp" in topkeys)
        err("legacy", topln["timestamp"] " 行目: v0.1 の `timestamp` は `generated: { by, at }` に置き換わった (§13.1)。scripts/okf-migrate-v02.sh で変換する")

      if ("status" in topkeys) {
        s = unquote(topval["status"])
        if (s != "draft" && s != "stable" && s != "deprecated")
          err("status", topln["status"] " 行目: `status` は draft | stable | deprecated のいずれか (§5.4)。実際の値: " s)
      }

      if (gen_seen) {
        if (gen_by == "")
          err("generated", gen_ln " 行目: `generated` には `by` が必須 (§5.2)")
        else if (!is_actor(gen_by))
          err("actor", gen_ln " 行目: `generated.by` が actor 記法でない (§7: human:<id> / process:<id> / <producer>/<version>)。実際の値: " gen_by)
        if (gen_at != "" && !is_datetime(gen_at))
          err("datetime", gen_ln " 行目: `generated.at` が ISO 8601 でない (§5.2)。実際の値: " gen_at)
      }

      for (k = 1; k <= nver; k++) {
        if (ver_by[k] == "")
          err("actor", ver_ln[k] " 行目: `verified[" k "]` に `by` が無い (§5.2)")
        else if (!is_actor(ver_by[k]))
          err("actor", ver_ln[k] " 行目: `verified[" k "].by` が actor 記法でない (§7)。実際の値: " ver_by[k])
        if (ver_at[k] != "" && !is_datetime(ver_at[k]))
          err("datetime", ver_ln[k] " 行目: `verified[" k "].at` が ISO 8601 でない (§5.2)。実際の値: " ver_at[k])
      }

      for (k = 1; k <= nsrc; k++) {
        if (src_res[k] == "")
          err("sources", src_ln[k] " 行目: `sources[" k "]` に `resource` が無い (§5.1 REQUIRED)")
        if (src_lm[k] != "" && !is_date(src_lm[k]))
          err("datetime", src_ln[k] " 行目: `sources[" k "].last_modified` は YYYY-MM-DD 形式で書く (§5.1)。実際の値: " src_lm[k])
      }

      if ("stale_after" in topkeys) {
        sa = unquote(topval["stale_after"])
        if (!is_date(sa))
          err("datetime", topln["stale_after"] " 行目: `stale_after` は絶対日付 YYYY-MM-DD で書く (§5.5)。実際の値: " sa)
        else if (today >= sa)
          wrn("stale", topln["stale_after"] " 行目: stale_after (" sa ") を過ぎている。内容を確認して更新するか期限を延ばすこと (§5.5)")
      }

      if ("type" in topkeys && unquote(topval["type"]) == "Attested Computation" && !("runtime" in topkeys))
        err("computation", topln["type"] " 行目: `type: Attested Computation` には `runtime` が必須 (§10.2)")

      if ("okf_version" in topkeys && unquote(topval["okf_version"]) != want_ver)
        wrn("okf-version", topln["okf_version"] " 行目: `okf_version` が \"" want_ver "\" でない。実際の値: " unquote(topval["okf_version"]))
    }
  ' "$1"
}

# 本文側の v0.2 検査。frontmatter の `sources[].id` を集めてから本文を見る。
# 出力: "<E|W>\t<check>\t<detail>"
body_lint() {
  awk '
    function trim(s) { sub(/^[ ]+/, "", s); sub(/[ ]+$/, "", s); return s }
    function unquote(s,   n, h) {
      n = length(s); if (n < 2) return s
      h = substr(s, 1, 1)
      if ((h == "\"" || h == "'"'"'") && substr(s, n, 1) == h) return substr(s, 2, n - 2)
      return s
    }
    { raw[NR] = $0 }
    END {
      if (NR == 0 || raw[1] != "---") exit
      fmend = 0
      for (i = 2; i <= NR; i++) if (raw[i] == "---") { fmend = i; break }
      if (fmend == 0) exit

      # frontmatter から sources[].id を収集（フロー表記・ブロック表記の両方）
      cur = ""
      for (i = 2; i < fmend; i++) {
        line = raw[i]
        if (line ~ /^[A-Za-z_][A-Za-z0-9_-]*:/) { cur = substr(line, 1, index(line, ":") - 1); continue }
        if (cur != "sources") continue
        if (match(line, /(^|[ {,])id:[ ]*[^,}]+/)) {
          v = substr(line, RSTART, RLENGTH)
          sub(/^.*id:[ ]*/, "", v)
          ids[unquote(trim(v))] = 1
        }
      }

      code = 0
      for (i = fmend + 1; i <= NR; i++) {
        line = raw[i]
        if (line ~ /^```/) { code = 1 - code; continue }
        if (code) continue

        if (line ~ /^# Citations[ \t]*$/)
          print "E\tlegacy\t" i " 行目: v0.1 の本文 `# Citations` は frontmatter の `sources` に置き換わった (§13.1)。scripts/okf-migrate-v02.sh で変換する"

        # 脚注の定義 `[^label]: ...` と参照 `[^label]` は sources[].id を鍵にする (§5.1)。
        # インラインコードは書式の説明で `[^id]` のように書かれるため対象外にする。
        s = line
        gsub(/`[^`]*`/, "", s)
        while (match(s, /\[\^[^]]+\]/)) {
          lab = substr(s, RSTART + 2, RLENGTH - 3)
          s = substr(s, RSTART + RLENGTH)
          if (!(lab in ids) && !(lab in warned)) {
            warned[lab] = 1
            print "W\tfootnote\t" i " 行目: 脚注ラベル `" lab "` に対応する `sources[].id` が無い (§5.1 では脚注ラベルが sources への結合キー)"
          }
        }
      }
    }
  ' "$1"
}

# frontmatter から 1 キーの値を取り出す（引用符は外す）。無ければ無出力。
# トップレベル（インデント 0）のキーだけを見る。
fm_value() {
  awk -v want="$2" '
    NR==1 { if ($0 != "---") exit; fm=1; next }
    fm && $0=="---" { exit }
    fm {
      if ($0 ~ /^[ ]/) next
      c = index($0, ":")
      if (c == 0) next
      if (substr($0, 1, c-1) != want) next
      val = substr($0, c+1)
      sub(/^[ ]+/, "", val); sub(/[ ]+$/, "", val)
      n = length(val)
      if (n >= 2 && substr(val,1,1) == "\"" && substr(val,n,1) == "\"") {
        val = substr(val, 2, n-2); gsub(/\\"/, "\"", val); gsub(/\\\\/, "\\", val)
      } else if (n >= 2 && substr(val,1,1) == "'"'"'" && substr(val,n,1) == "'"'"'") {
        val = substr(val, 2, n-2); gsub(/'"'"''"'"'/, "'"'"'", val)
      }
      print val
      exit
    }
  ' "$1"
}

# index.md の 1 エントリから「リンク先」と「転記された description」を取り出す。
# 形式は docs/CLAUDE.md の `* [Title](file.md) - description`。
# 出力: "<リンク先>\t<description>" を 1 行ずつ。
index_entries() {
  awk '
    BEGIN { code=0; comment=0 }
    /^```/ { code = 1 - code; next }
    code { next }
    /<!--/ { comment=1 }
    comment { if ($0 ~ /-->/) comment=0; next }
    {
      line = $0
      sub(/^[ ]*[*-][ ]+/, "", line)
      if (line !~ /^\[/) next
      p = index(line, "](")
      if (p == 0) next
      rest = substr(line, p+2)
      q = index(rest, ")")
      if (q == 0) next
      link = substr(rest, 1, q-1)
      desc = substr(rest, q+1)
      if (substr(desc, 1, 3) != " - ") next
      desc = substr(desc, 4)
      sub(/[ ]+$/, "", desc)
      print link "\t" desc
    }
  ' "$1"
}

# 本文からバンドル内リンクのターゲットを抽出（コードブロック・HTMLコメント・インラインコードを除去）
extract_links() {
  awk 'BEGIN{code=0}
       /^```/{code=1-code; next}
       !code {print}' "$1" |
    sed -e 's/<!--.*-->//g' -e 's/`[^`]*`//g' |
    grep -oE '\]\([^) ]+\)' |
    sed -e 's/^](//' -e 's/)$//' -e 's/[#?].*$//' |
    grep -vE '^[a-zA-Z][a-zA-Z0-9+.-]*:' || true # http:, mailto: 等の外部URLを除外
}

# fm_lint / body_lint の "<E|W>\t<check>\t<detail>" を error()/warn() へ振り分ける
report() {
  while IFS="$TAB" read -r sev chk detail; do
    [ -n "${chk:-}" ] || continue
    if [ "$sev" = "W" ]; then
      warn "$chk" "$1" "$detail"
    else
      error "$chk" "$1" "$detail"
    fi
  done
}

# log.d/ は log.md の材料置き場であって concept ディレクトリではない（#360）。
# frontmatter も index.md も要らず、ルート index.md への掲載対象でもないので、
# 両方の走査から丸ごと外す。中身の検査は okf-log-build.sh --check が担う。
FRAGMENT_DIR="log.d"

# ---------- 1) frontmatter / type / index-fm / log-format ----------
find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -name "$FRAGMENT_DIR" -prune -o -type f -name '*.md' -print |
  while IFS= read -r file; do
    rel="${file#"$BUNDLE"/}"
    base="$(basename "$file")"

    if is_reserved "$base"; then
      if [ "$base" = "index.md" ] && [ "$rel" != "index.md" ]; then
        if has_frontmatter "$file"; then
          error "index-fm" "$rel" "ルート以外の index.md に frontmatter は書けない (SPEC §8/§11)"
        fi
      fi
      # ルート index.md は okf_version 宣言だけを持つ予約ファイル。description は
      # 要らないが、YAML として壊れていないことは他と同じく担保する。
      if [ "$rel" = "index.md" ] && has_frontmatter "$file"; then
        fm_lint "$file" 0 "$TODAY" "$OKF_VERSION" | report "$rel"
      fi
      if [ "$base" = "log.md" ]; then
        grep -E '^## ' "$file" | grep -vE '^## [0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]*$' |
          while IFS= read -r bad; do
            error "log-format" "$rel" "日付見出しが ISO 8601 (## YYYY-MM-DD) でない: \"$bad\""
          done
      fi
      continue
    fi

    if ! has_frontmatter "$file"; then
      error "frontmatter" "$rel" "YAML frontmatter がない（--- で囲んだブロックを先頭に置く）"
      continue
    fi
    if ! has_type "$file"; then
      error "type" "$rel" "frontmatter に空でない \`type\` がない (SPEC §4.1 REQUIRED)"
    fi
    fm_lint "$file" 1 "$TODAY" "$OKF_VERSION" | report "$rel"
    body_lint "$file" | report "$rel"
  done >/tmp/okf-lint-pass1.$$
# サブシェル内のカウンタは失われるため、出力行から集計し直す
cat /tmp/okf-lint-pass1.$$

# ---------- 2) index-exists / index-listed ----------
find "$BUNDLE" -name node_modules -prune -o -name '.*' -prune -o -name "$FRAGMENT_DIR" -prune -o -type d -print |
  while IFS= read -r dir; do
    reldir="${dir#"$BUNDLE"}"
    reldir="${reldir#/}"

    # このディレクトリ直下の concept と、.md を含むサブディレクトリを収集
    concepts=""
    subdirs=""
    for entry in "$dir"/*; do
      [ -e "$entry" ] || continue
      name="$(basename "$entry")"
      case "$name" in .* | node_modules) continue ;; esac
      if [ -f "$entry" ]; then
        case "$name" in *.md) ;; *) continue ;; esac
        is_reserved "$name" && continue
        is_exempt "$name" && continue
        concepts="$concepts $name"
      elif [ -d "$entry" ]; then
        # 断片置き場は concept のサブディレクトリではないので index への掲載も要らない
        [ "$name" = "$FRAGMENT_DIR" ] && continue
        if find "$entry" -type f -name '*.md' -print -quit | grep -q .; then
          subdirs="$subdirs $name"
        fi
      fi
    done
    [ -z "$concepts$subdirs" ] && continue

    idx="$dir/index.md"
    relidx="${idx#"$BUNDLE"/}"
    if [ ! -f "$idx" ]; then
      error "index-exists" "${reldir:-（root）}" "index.md がない（progressive disclosure が途切れる）"
      continue
    fi

    # index.md 内のリンクをバンドル相対に正規化した一覧を作る
    links="$(extract_links "$idx" | while IFS= read -r t; do
      case "$t" in
      /*) echo "${t#/}" ;;
      ./*)
        t="${t#./}"
        [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t"
        ;;
      *) [ -n "$reldir" ] && echo "$reldir/$t" || echo "$t" ;;
      esac
    done | sed 's|/$||')"

    # index.md の各エントリの「転記された description」を、リンク先を鍵に引けるようにする
    entries="$(index_entries "$idx" | while IFS="$TAB" read -r t d; do
      case "$t" in
      /*) t="${t#/}" ;;
      ./*)
        t="${t#./}"
        [ -n "$reldir" ] && t="$reldir/$t"
        ;;
      *) [ -n "$reldir" ] && t="$reldir/$t" ;;
      esac
      printf '%s\t%s\n' "$t" "$d"
    done)"

    for c in $concepts; do
      target="${reldir:+$reldir/}$c"
      if echo "$links" | grep -qxF "$target"; then
        # index の転記は frontmatter の description の全文でなければならない。
        # ずれたまま放置されると okf-search の結果と index の説明が食い違う。
        # frontmatter 自体が壊れている場合は、そちらを直すのが先なので比較しない
        # （壊れた値を転記先として提示すると誤誘導になる）。警告だけなら比較する。
        if fm_lint "$dir/$c" 1 "$TODAY" "$OKF_VERSION" | grep -q '^E'; then
          continue
        fi
        want="$(fm_value "$dir/$c" description)"
        got="$(printf '%s\n' "$entries" |
          awk -v k="$target" 'BEGIN{FS="\t"} $1==k {print substr($0, index($0,"\t")+1); exit}')"
        if [ -z "$got" ]; then
          error "index-desc" "$relidx" "$c のエントリが \`* [Title]($c) - <description>\` の形になっていない"
        elif [ -n "$want" ] && [ "$got" != "$want" ]; then
          error "index-desc" "$relidx" "$c の転記が frontmatter の description と一致しない
    index      : $got
    frontmatter: $want"
        fi
      else
        error "index-listed" "$relidx" "concept が未掲載: $c"
      fi
    done
    for s in $subdirs; do
      target="${reldir:+$reldir/}$s"
      echo "$links" | grep -qxF "$target" ||
        echo "$links" | grep -qxF "$target/index.md" ||
        error "index-listed" "$relidx" "サブディレクトリが未掲載: $s/"
    done
  done >/tmp/okf-lint-pass2.$$
cat /tmp/okf-lint-pass2.$$

ERRORS=$(cat /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$ | grep -c '^ERROR' || true)
WARNINGS=$(cat /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$ | grep -c '^WARN' || true)
rm -f /tmp/okf-lint-pass1.$$ /tmp/okf-lint-pass2.$$

# ---------- 3) 元帳の同期（log.md / index.md の一覧は生成物）----------
# 検査そのものはビルダーへ委譲する。ここで frontmatter を読み直す実装を持つと、
# 「lint は通るのに生成すると差分が出る」という食い違いが必ずいつか生まれる
# （同じ規約を 2 箇所で実装することになるため）。ビルダーが正本。
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
for pair in "log-sync:okf-log-build.sh" "index-sync:okf-index-build.sh"; do
  check="${pair%%:*}"
  builder="${SCRIPT_DIR}/${pair#*:}"
  # ビルダーが無いのは「検査しなくてよい」ではなく「検査できない」。スキップすると
  # 生成物への唯一の番人が消えたまま lint が緑を返す（＝ビルダーを消す/改名する
  # PR が素通りする）ので、エラーにする。
  if [ ! -f "$builder" ]; then
    error "$check" "${BUNDLE}" "ビルダーが無いので検査できない: ${builder}"
    continue
  fi
  if ! bash "$builder" "$BUNDLE" --check; then
    error "$check" "${BUNDLE}" "生成物が材料と同期していない（上の差分を参照）"
  fi
done

# ---------- 4) リンク切れ（lychee に委譲）----------
if [ "$NO_LINKS" -eq 0 ]; then
  if command -v lychee >/dev/null 2>&1; then
    # --offline: ネットワークに出ない（外部URLは対象外、ファイルリンクのみ検査）
    # --root-dir: /path/file.md 形式のバンドルルート相対リンクを解決
    if ! lychee --offline --no-progress --root-dir "$(cd "$BUNDLE" && pwd)" "$BUNDLE"; then
      if [ "$STRICT" -eq 1 ]; then
        echo "ERROR [link] リンク切れを検出（--strict のためエラー扱い）"
        ERRORS=$((ERRORS + 1))
      else
        echo "WARN  [link] リンク切れあり（未執筆 concept なら許容可 / SPEC §6.1。--strict でエラー化）"
        WARNINGS=$((WARNINGS + 1))
      fi
    fi
  else
    echo "note: lychee 未インストールのためリンク検査をスキップ (brew install lychee / --no-links で明示スキップ)"
  fi
fi

echo ""
echo "okf-lint: ${ERRORS} error(s), ${WARNINGS} warning(s)$([ "$STRICT" -eq 1 ] && echo ' (--strict)')"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
