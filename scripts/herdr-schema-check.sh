#!/usr/bin/env bash
# herdr Socket API との互換を、コミット済みの schema スライスに対して機械検査する。
#
#   bash scripts/herdr-schema-check.sh
#
# 2 つの検査:
#
#   drift  再生成した `wire.rs` がコミット済みのものと一致するか
#   compat コミット済みの各版が、下限版から生成した型で読めるか
#
# **なぜ protocol 整数ではなく schema 差分なのか。** `ping` の `protocol` は
# herdr のバイナリ client↔server wire 形式の版で、totsuka が使う NDJSON
# Socket API を追跡していない（実測: 17 → 20 の 3 bump で 22 メソッドは無変化、
# 逆に `custom_status` は 16 → 16 のまま削除された）。使える信号は schema
# そのものの差分だけで、しかも totsuka が使う 22 メソッドに絞ったものである。
#
# **なぜ PR の CI で最新版を取りに行かないのか。** herdr がリリースされた瞬間に
# 無関係な PR が全部赤くなり、ネットワーク障害でも落ちる。PR の CI はコミット済み
# schema とだけ突き合わせ、新版の追随は別レーンに分ける（日次 cron は #520 §2 で
# 入れる。**まだ無い** — 現状の追随は `--fetch` を手で叩くこと）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCHEMA_DIR="$ROOT/plugins/agent-ide-herdr/schemas"
SPEC="$SCHEMA_DIR/methods.json"
WIRE="$ROOT/plugins/agent-ide-herdr/src/wire.rs"

command -v jq >/dev/null 2>&1 || { echo "herdr-schema-check: jq が必要です" >&2; exit 2; }
# 生成器は rustfmt に必須依存している（整形の有無が生成結果を変える）。ここで
# 見ておかないと、rustfmt の無い環境では drift 検査が理由の分からない exit 2 に
# なる — 「検査が落ちた」と「検査が動かなかった」を取り違えさせない。
command -v rustfmt >/dev/null 2>&1 \
  || { echo "herdr-schema-check: rustfmt が必要です (rustup component add rustfmt)" >&2; exit 2; }

# 型の対応表は生成器と共有する。検査は「各プロパティが写る Rust の型が
# 変わっていないか」を見るので、写像そのものが同じでなければ意味が無い。
TYPES_JQ="$(cat "$ROOT/scripts/herdr-types.jq")"

errors=0
fail() { echo "herdr-schema-check: $*" >&2; errors=$((errors + 1)); }

# ---------------------------------------------------------------- drift
# 生成物をその場で作り直して突き合わせる。**元の内容はバイト単位で戻す**
# （`$(cat …)` 経由だと末尾改行が落ちて、検査自身が差分を作ってしまう）。
before="$(mktemp)"; trap 'rm -f "$before"' EXIT
cp "$WIRE" "$before"
# stdout だけを捨てる。**stderr は残す** — 生成が落ちた理由が 1 文字も
# 出ないと、`set -e` のフェイルクローズが「無言の exit」に見える。
bash "$ROOT/scripts/herdr-types-build.sh" >/dev/null
if ! cmp -s "$before" "$WIRE"; then
  fail "wire.rs が生成物と一致しません。\`bash scripts/herdr-types-build.sh\` を流してコミットしてください"
  cp "$before" "$WIRE"
fi

# ---------------------------------------------------------------- compat
# 落とす条件は**向きで違う**。totsuka が読む側（result）と送る側（request）で、
# 互換を壊す変化が逆になるためである。
#
#   result（読む）: メソッド削除 / result タグの削除 / **生成した型に載っている**
#                   プロパティの削除 / `required` から外れる（新しい版が省略しうる
#                   → 下限生成の型が落ちる）/ enum バリアントの削除。
#                   `required` の**追加**は保証が強まるだけなので通す
#   request（送る）: `required` の**追加**（totsuka が送らない params を要求される）/
#                   totsuka が送る enum バリアントの削除 / 生成した型に載っている
#                   params プロパティの削除
#
# **プロパティの検査は「生成した型に載っているもの」が対象で、`reads` に挙げた
# ものだけではない。** 型に載っている以上どれが読まれてもおかしくないので、
# 対象を `reads` に絞ると検査が主張より弱くなる。`reads` は別の検査
# （下限版に対する妥当性、下記）に使う。
#
# なお request 側のプロパティ削除も落とす。生成した型はその名前で送るので、
# 消えたキーは herdr に無視される（request `$defs` に `additionalProperties:
# false` は 1 つも無い）が、**送っているつもりのものが届かなくなる**のは
# 黙って縮退する側なので、知りたい。
read -r -d '' COMPAT_JQ <<'JQ' || true
# 生成される型の一覧を、名前 → schema で作る。`$doc.results[]` の封筒も
# **型である**（`*Envelope` として生成される）ので、ここに入れる。これを
# 落としていると、`pong.version` が消えても検査が通ってしまう。
def typed($doc; $ns):
  ($doc[$ns + "_defs"] // {}) as $defs
  | ($defs
     # `oneOf` の def は tagged union として生成される（`Subscription` は 26
     # バリアント）。トップレベルには `.properties` も `.required` も無く、中身は
     # `.oneOf[]` の下にあるので、**バリアントを 1 つずつ型として展開する**。
     # これを falten せずに `typed()` へ入れても空を返すだけで、生成される
     # Rust enum のフィールドが丸ごと検査の外に出る（封筒と同じクラスの穴）。
     + ($defs | to_entries
        | map(.key as $t | ((.value.oneOf // [])
              | map({ key: ($t + "::" + (.properties.type.const | variant_name)),
                      value: (. | del(.properties.type)) })))
        | flatten | from_entries)
     + (if $ns == "result"
        then ($doc.results // []
              | map({key: ((.properties.type.const | variant_name) + "Envelope"), value: .})
              | from_entries)
        else {} end));

# tagged union の**バリアント名の集合**。`enum` と同じ意味で「消えたら落とす」
# 対象なので、`enums()` と合わせて扱う。
def variants($doc; $ns):
  (($doc[$ns + "_defs"] // {}) | to_entries
   | map(.key as $t | ((.value.oneOf // []) | map($t + "::" + .properties.type.const)))
   | flatten | unique);

def props($doc; $ns):
  (typed($doc; $ns) | to_entries
   | map(.key as $t | ((.value.properties // {}) | keys | map($t + "." + .)))
   | flatten | unique);
def required($doc; $ns):
  (typed($doc; $ns) | to_entries
   | map(.key as $t | ((.value.required // []) | map($t + "." + .)))
   | flatten | unique);
def enums($doc; $ns):
  (typed($doc; $ns) | to_entries
   | map(.key as $t | ((.value.enum // []) | map($t + "." + .)))
   | flatten | unique);

# **プロパティの「中身」も見る。** 名前の集合だけを比べていると、
# `revision` が `uint64` から `string` に化けても検査を通る（生成型は `u64` の
# ままなので、実行時にハードエラーになる）。比べる相手は生成される Rust の
# 型名そのもの — 生成と検査が同じ写像（`herdr-types.jq`）を見ている理由。
def shapes($doc; $ns):
  (typed($doc; $ns) | to_entries
   | map(.key as $t
         | ((.value.properties // {}) | to_entries
            | map(.key as $p
                  | { key: ($t + "." + $p),
                      value: (try (.value | rust_type) catch ("<" + . + ">")) })))
   | flatten | from_entries);

def methods($doc): [$doc.methods[].properties.method.const] | unique;
def results($doc): [$doc.results[].properties.type.const] | unique;

$floor as $f | . as $v
| (shapes($f; "result")) as $fsr | (shapes($v; "result")) as $vsr
| (shapes($f; "request")) as $fsq | (shapes($v; "request")) as $vsq
| [
    (methods($f) - methods($v) | map("メソッドが消えた: " + .)),
    (results($f) - results($v) | map("result のタグが消えた: " + .)),
    (props($f; "result") - props($v; "result")
      | map("生成した型に載っている result のプロパティが消えた: " + .)),
    (props($f; "request") - props($v; "request")
      | map("生成した型に載っている params のプロパティが消えた: " + .)),
    (required($f; "result") - required($v; "result")
      | map("result の required から外れた（新しい版が省略しうる）: " + .)),
    # **下限版に無い型の `required` は数えない。** 新しい任意パラメータが新しい
    # def を `$ref` するのは追加のみのリリースで最もありふれた形で、totsuka は
    # そのキーを一切送らないので何も壊れていない。型名で絞らないと、純粋に
    # 追加だけの herdr 版で CI が赤くなる。
    ([typed($f; "request") | keys[]] as $known
      | (required($v; "request") - required($f; "request"))
      | map(select((. | split(".")[0]) as $t | $known | index($t))
            | "request に required が増えた（totsuka は送っていない）: " + .)),
    (enums($f; "result") - enums($v; "result") | map("読む enum のバリアントが消えた: " + .)),
    (enums($f; "request") - enums($v; "request") | map("送る enum のバリアントが消えた: " + .)),
    (variants($f; "result") - variants($v; "result")
      | map("読む tagged union のバリアントが消えた: " + .)),
    (variants($f; "request") - variants($v; "request")
      | map("送る tagged union のバリアントが消えた: " + .)),
    ($fsr | to_entries | map(select($vsr[.key] != null and $vsr[.key] != .value)
      | "result のプロパティの型が変わった: \(.key) は \(.value) だったが \($vsr[.key])")),
    ($fsq | to_entries | map(select($vsq[.key] != null and $vsq[.key] != .value)
      | "params のプロパティの型が変わった: \(.key) は \(.value) だったが \($vsq[.key])"))
  ] | flatten
JQ

floor="$(jq -r '.floor' "$SPEC")"
floor_file="$SCHEMA_DIR/herdr-$floor.json"
[[ -f "$floor_file" ]] || { echo "herdr-schema-check: 下限版がありません: $floor_file" >&2; exit 2; }

# ---------------------------------------------------------------- reads
# `methods.json` の `reads` が下限版の schema で実在するかを確かめる。**互換の
# 検査ではなく、対応表が嘘をついていないことの検査**である。`reads` は手書きで、
# 手書きの表は放っておくと本文から静かにずれる — その 1 点をここで押さえる。
read -r -d '' READS_JQ <<'JQ' || true
# `$ref` / 配列 / `[T, null]` を剥がして、プロパティを引ける形にする。
def unwrap($defs):
  def once:
    if (. | has("$ref")) then ($defs[.["$ref"] | sub("^#/schemas/[a-z_]+/\\$defs/"; "")] // null)
    elif (.type == "array") then (.items // null)
    elif (. | has("anyOf")) then ((.anyOf | map(select(.type != "null")))[0] // null)
    else . end;
  . as $s | once as $n | if ($n == null) or ($n == $s) then $n else ($n | unwrap($defs)) end;

. as $doc
| ($doc.result_defs) as $defs
| ([$doc.results[] | {key: .properties.type.const, value: .}] | from_entries) as $env
| [ $spec.methods[]
    | select(.result != null)
    | . as $m
    | .reads[]
    | . as $path
    | (reduce ($path | split(".") | .[]) as $p ($env[$m.result];
         if . == null then null else (unwrap($defs) | ((.properties // {})[$p] // null)) end)) as $hit
    | select($hit == null)
    | "\($m.method): reads `\($path)` が下限版の schema に見当たりません" ]
  # `params` も手書きなので、同じく下限版と突き合わせる。`reads` にだけ検査を
  # 付けて `params` に付けないと、同じ手書きの表の片方だけがずれ得る。
  + [ $spec.methods[]
      | . as $m
      | ($doc.methods[] | select(.properties.method.const == $m.method)
         | .properties.params["$ref"] | sub("^#/schemas/[a-z_]+/\\$defs/"; "")) as $actual
      | select($actual != $m.params)
      | "\($m.method): params は `\($m.params)` と書かれていますが、下限版の schema は `\($actual)` です" ]
JQ

reads_out="$(jq -r --argjson spec "$(cat "$SPEC")" "$READS_JQ" "$floor_file")" \
  || { echo "herdr-schema-check: reads の検査自体が失敗しました" >&2; exit 2; }
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  fail "$line"
done < <(jq -r '.[]' <<<"$reads_out")

checked=0
for f in "$SCHEMA_DIR"/herdr-*.json; do
  version="$(jq -r '.sliced_from.herdr_version' "$f")"
  [[ "$f" == "$floor_file" ]] && continue
  checked=$((checked + 1))
  # フェイルクローズ: 検査パイプライン自体の失敗は「違反なし」ではない
  # （`arch-lint.sh` と同じ方針）。パイプの中で jq が落ちると `while read` は
  # 何も読まずに抜け、そのまま 0 error と報告してしまう。
  out="$(jq -r --argjson floor "$(cat "$floor_file")" "$TYPES_JQ"$'\n'"$COMPAT_JQ" "$f")" \
    || { echo "herdr-schema-check: $version の互換検査自体が失敗しました" >&2; exit 2; }
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    fail "herdr $version: $line"
  done < <(jq -r '.[]' <<<"$out")
done

# **対象ゼロは成功ではない。** 下限を上げて古いスライスを消し忘れた、逆に
# 上位版を消した、という経路でここが 0 になる。件数は出力に出るが、CI は
# 人が読まないので機械で押さえる。
if [[ $checked -eq 0 ]]; then
  fail "下限版より上のスライスが 1 本もありません。互換を突き合わせる相手がいません"
fi

if [[ $errors -gt 0 ]]; then
  echo "herdr-schema-check: $errors error(s)" >&2
  exit 1
fi
echo "herdr-schema-check: 0 error(s)（下限 $floor / 上位 $checked 版 / $(jq -r '.methods | length' "$SPEC") メソッドを検査）"
