#!/usr/bin/env bash
# herdr Socket API の型生成器。
#
#   scripts/herdr-types-build.sh              # 下限版のスライスから wire.rs を生成
#   scripts/herdr-types-build.sh --fetch v0.8.2   # 新しいタグを取得してスライスを追加
#
# 生成の入力は 2 つだけ:
#
#   plugins/agent-ide-herdr/schemas/methods.json     手書きの対応表（下記）
#   plugins/agent-ide-herdr/schemas/herdr-<floor>.json  下限版のスライス済み schema
#
# **なぜ手書きの対応表が要るのか。** herdr の schema は method と result を
# 結び付けていない。`success_response.result` は `type` の const で判別する
# 57 分岐の `oneOf`（`ResponseResult`）で、`request` 側の `method` からは
# 辿れない。したがって「どのメソッドがどの result を返すか」は schema から
# 機械的に取り出せず、`methods.json` が一次情報になる。
#
# **なぜ下限版から生成するのか。** 型は 1 組しか作らない。古い版 = 生成元
# そのものなので定義上読める。新しい版は追加しかしない（それを CI の
# schema 差分が保証する）ので、未知フィールド無視 + `#[serde(other)]` で
# 同じ型が読める。逆向き（最新版から生成）は、新しく `required` になった
# フィールドを古い版が送らずデシリアライズが落ちるので採らない。
#
# **フェイルクローズ。** 教えていない JSON Schema 構文に当たったら、推測せず
# 異常終了する（`arch-lint.sh` と同じ方針）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCHEMA_DIR="$ROOT/plugins/agent-ide-herdr/schemas"
OUT="$ROOT/plugins/agent-ide-herdr/src/wire.rs"
SPEC="$SCHEMA_DIR/methods.json"
UPSTREAM="https://raw.githubusercontent.com/herdrdev/herdr"

command -v jq >/dev/null 2>&1 || { echo "herdr-types: jq が必要です (brew install jq)" >&2; exit 2; }
command -v rustfmt >/dev/null 2>&1 || { echo "herdr-types: rustfmt が必要です (rustup component add rustfmt)" >&2; exit 2; }

# 生成物を整形する edition は、ワークスペースが宣言しているものに従う。
EDITION="$(jq -rn --rawfile t "$ROOT/Cargo.toml" '$t' | sed -n 's/^edition *= *"\([0-9]*\)".*/\1/p' | head -1)"
[[ -n "$EDITION" ]] || { echo "herdr-types: ワークスペースの edition を読めません" >&2; exit 2; }

# ---------------------------------------------------------------- スライス
# フル schema から `methods.json` の 22 メソッドの `$ref` 閉包だけを切り出す。
read -r -d '' SLICE_JQ <<'JQ' || true
def refnames($ns): [.. | objects | select(has("$ref")) | .["$ref"]]
  | map(select(startswith("#/schemas/" + $ns + "/$defs/"))
        | sub("^#/schemas/" + $ns + "/\\$defs/"; ""));

def closure($defs; $ns; $seed):
  def step($seen):
    ($seen | map($defs[.] // empty) | refnames($ns)) as $next
    | ($seen + $next | unique) as $grown
    | if ($grown | length) == ($seen | length) then $seen else step($grown) end;
  step($seed | unique);

. as $full
| ($full.schemas.request."$defs") as $rdefs
| ($full.schemas.success_response."$defs") as $sdefs
| ([$spec.methods[].method]) as $wanted
| ([$full.schemas.request.oneOf[]
     | select(.properties.method.const as $m | $wanted | index($m))]) as $reqv
| (($wanted - [$reqv[].properties.method.const])) as $missing
| if ($missing | length) > 0
  then error("herdr-types: このタグに存在しないメソッド: " + ($missing | join(", ")))
  else . end
| ([$spec.methods[] | select(.result != null) | .result] | unique) as $tags
| ([$full.schemas.success_response."$defs".ResponseResult.oneOf[]
     | select(.properties.type.const as $t | $tags | index($t))]) as $resv
| (($tags - [$resv[].properties.type.const])) as $missing_tags
| if ($missing_tags | length) > 0
  then error("herdr-types: このタグに存在しない result: " + ($missing_tags | join(", ")))
  else . end
| closure($rdefs; "request"; ($reqv | refnames("request"))) as $rc
| closure($sdefs; "success_response"; ($resv | refnames("success_response"))) as $sc
| {
    sliced_from: {
      herdr_version: $version,
      schema_version: $full.schema_version,
      protocol: $full.protocol
    },
    methods: ($reqv | sort_by(.properties.method.const)),
    results: ($resv | sort_by(.properties.type.const)),
    request_defs: ($rc | sort | map({key: ., value: $rdefs[.]}) | from_entries),
    result_defs: ($sc | sort | map({key: ., value: $sdefs[.]}) | from_entries)
  }
JQ

slice() { # <full schema path> <version>
  jq -S --argjson spec "$(cat "$SPEC")" --arg version "$2" "$SLICE_JQ" "$1"
}

# ---------------------------------------------------------------- 生成器
read -r -d '' GEN_JQ <<'JQ' || true
def die($msg): error("herdr-types: " + $msg);

def pascal: split("_") | map(select(length > 0) | (.[0:1] | ascii_upcase) + .[1:]) | join("");
# `pane.exited` も `pane_exited` も PaneExited へ（herdr は区切り文字が混在する）
def variant_name: gsub("[.\\-]"; "_") | pascal;

def rust_keyword: ["as","break","const","continue","crate","dyn","else","enum","extern","false",
  "fn","for","if","impl","in","let","loop","match","mod","move","mut","pub","ref","return",
  "self","Self","static","struct","super","trait","true","type","unsafe","use","where","while",
  "async","await","box","macro","try","yield"];
def ident: . as $n | if (rust_keyword | index($n)) then "r#" + $n else $n end;

# schemars が出す format。`uint` は usize、`int` は isize。
def int_type($fmt):
  ({"uint8":"u8","uint16":"u16","uint32":"u32","uint64":"u64","uint":"u64",
    "int8":"i8","int16":"i16","int32":"i32","int64":"i64","int":"i64"}[$fmt // "int64"])
  // die("知らない integer format `\($fmt)`");
def num_type($fmt):
  ({"float":"f32","double":"f64"}[$fmt // "double"]) // die("知らない number format `\($fmt)`");

# 1 つのプロパティ schema を Rust の型へ。教えていない構文は推測せず落とす。
def rust_type:
  . as $s
  | if ($s | has("$ref")) then ($s["$ref"] | sub("^#/schemas/[a-z_]+/\\$defs/"; ""))
    elif ($s | has("anyOf")) then
      ($s.anyOf) as $a
      | if (($a | length) == 2) and (($a | map(select(.type == "null")) | length) == 1)
        then "Option<" + (($a | map(select(.type != "null")))[0] | rust_type) + ">"
        else die("`[T, null]` ではない anyOf: " + ($a | tojson)) end
    elif ($s | has("oneOf")) then die("インラインの oneOf は非対応（$def にすること）")
    elif ($s | has("allOf")) then die("allOf は非対応")
    elif ($s | has("enum")) then die("インラインの enum は非対応（$def にすること）")
    elif ($s | has("type")) then
      ($s.type) as $t
      | if ($t | type) == "array" then
          (if ($t | index("null")) and (($t | length) == 2)
           then "Option<" + (($s | del(.type)) + {type: (($t - ["null"])[0])} | rust_type) + ">"
           else die("読めない type 配列: " + ($t | tojson)) end)
        elif $t == "string" then "String"
        elif $t == "boolean" then "bool"
        elif $t == "integer" then int_type($s.format)
        elif $t == "number" then num_type($s.format)
        elif $t == "array" then "Vec<" + (($s.items // die("items の無い array")) | rust_type) + ">"
        elif $t == "object" then
          (if (($s.additionalProperties | type) == "object")
           then "BTreeMap<String, " + ($s.additionalProperties | rust_type) + ">"
           elif ($s | has("properties")) then die("インラインの object は非対応（$def にすること）")
           else "BTreeMap<String, serde_json::Value>" end)
        else die("非対応の type `" + ($t | tostring) + "`") end
    else die("type / $ref / anyOf / enum のいずれも無いプロパティ: " + ($s | tojson)) end;

# `$ser` は「totsuka が送る側」。送る側では未設定のフィールドを **キーごと落とす**
# 必要がある（`json!` で組んでいた既存の呼び出しと同じ形にするため。明示的な
# `null` は herdr にとって「未指定」と同じとは限らない）。読む側にその問題は無い。
def field($name; $schema; $required; $ser):
  ($schema | rust_type) as $ty
  | ($name | ident) as $id
  | (if ($ty | startswith("Vec<")) then "Vec::is_empty"
     elif ($ty | startswith("BTreeMap<")) then "BTreeMap::is_empty"
     else "Option::is_none" end) as $skip
  | (if ($ty | startswith("Vec<")) or ($ty | startswith("BTreeMap<")) or ($ty | startswith("Option<"))
     then $ty else "Option<\($ty)>" end) as $opt
  | if $required then "    pub \($id): \($ty),"
    elif $ser then "    #[serde(default, skip_serializing_if = \"\($skip)\")]\n    pub \($id): \($opt),"
    else "    #[serde(default)]\n    pub \($id): \($opt)," end;

def fields($schema; $skip; $ser):
  ($schema.required // []) as $req
  | (($schema.properties // {}) | to_entries
     | map(.key as $k | select((($skip // []) | index($k)) | not))
     | sort_by(.key)
     | map(.key as $k | field($k; .value; (($req | index($k)) != null); $ser))
     | join("\n"));

def gen_struct($name; $schema; $skip; $ser; $derive):
  (fields($schema; $skip; $ser)) as $f
  | if ($f | length) == 0 then "#[derive(\($derive))]\npub struct \($name) {}"
    else "#[derive(\($derive))]\npub struct \($name) {\n" + $f + "\n}" end;

def gen_enum($name; $values; $other; $derive):
  ([ "#[derive(\($derive), Copy, PartialEq, Eq)]", "pub enum \($name) {" ]
   + ($values | map("    #[serde(rename = \"\(.)\")]\n    \(. | variant_name),"))
   + (if $other then [
        "    /// この生成が知らない値。herdr はリリースの合間にバリアントを足す",
        "    /// （実測: 2 ヶ月で `EventKind` に 3 個）ので、読みは 1 個の追加で",
        "    /// 落ちてはならない。追加を報せるのはコミット済み schema の差分で",
        "    /// あって、デシリアライズの失敗ではない。",
        "    #[serde(other)]",
        "    Unrecognized,"
      ] else [] end)
   + [ "}" ]) | join("\n");

def gen_tagged($name; $schema; $ser; $derive):
  ([ "#[derive(\($derive))]", "#[serde(tag = \"type\")]", "pub enum \($name) {" ]
   + ($schema.oneOf | map(
       (.properties.type.const // die("`type` const の無い oneOf バリアント")) as $tag
       | (fields(.; ["type"]; $ser)) as $f
       | "    #[serde(rename = \"\($tag)\")]\n    \($tag | variant_name)"
         + (if ($f | length) == 0 then ","
            else " {\n" + ($f | gsub("(?m)^    "; "        ") | gsub("(?m)^(?<i> +)pub "; "\(.i)")) + "\n    }," end)))
   + [ "}" ]) | join("\n");

def gen_def($name; $schema; $ser; $derive):
  if ($schema | has("enum")) then gen_enum($name; $schema.enum; ($ser | not); $derive)
  elif ($schema | has("oneOf")) then gen_tagged($name; $schema; $ser; $derive)
  elif ($schema.type == "object") then gen_struct($name; $schema; []; $ser; $derive)
  else die("`\($name)` は enum でも tagged union でも object でもない") end;

# 生成名の衝突は黙って壊れるので、ここで落とす。
def check_unique($names; $where):
  ($names | group_by(.) | map(select(length > 1) | .[0])) as $dup
  | if ($dup | length) > 0 then die("\($where) で生成名が衝突: " + ($dup | join(", "))) else $names end;

# `use` はモジュールの中に置く（`pub mod` の外の import はモジュール内から
# 見えない）。BTreeMap / serde_json は使われないこともあるので、生成した本文に
# 実際に現れたものだけを入れる — 使わない import は `warnings = "deny"` で落ちる。
def wrap_mod($title; $derive; $body):
  ([ "pub mod \($title) {" ]
   + ["    use serde::\($derive);"]
   + (if ($body | test("BTreeMap<")) then ["    use std::collections::BTreeMap;"] else [] end)
   + [""] + [$body] + ["}"]) | join("\n");
JQ

# 生成物の本文。request / result は herdr 自身が schema 上で分けている
# 名前空間なので、Rust でもモジュールを分ける（同じ `AgentStatus` が両側に
# あり、送る側には `#[serde(other)]` を付けてはならない — 知らない値を
# 送り返すことになる）。
read -r -d '' EMIT_JQ <<'JQ' || true
(.sliced_from.herdr_version) as $ver
| (.request_defs | keys) as $rnames
| check_unique($rnames; "request") as $_
| (.result_defs | keys) as $enames
| ([.results[] | (.properties.type.const | variant_name) + "Envelope"]) as $envnames
| check_unique($enames + $envnames; "result") as $_
| ([
    "//! herdr Socket API の wire 型。**生成物 — 手で編集しない。**",
    "//!",
    "//! 生成元: `plugins/agent-ide-herdr/schemas/herdr-\($ver).json`",
    "//! （= herdr \($ver) の API schema を `schemas/methods.json` の 22 メソッドへ",
    "//! スライスしたもの）。再生成は `bash scripts/herdr-types-build.sh`。",
    "//!",
    "//! # なぜ下限版から生成するのか",
    "//!",
    "//! 型は 1 組だけで、版ごとの分岐は作らない。古い版は生成元そのものなので",
    "//! 定義上読める。新しい版は追加しかしない（それを CI の schema 差分が",
    "//! 保証する）ので、未知フィールド無視 + `#[serde(other)]` で同じ型が読める。",
    "//!",
    "//! # 実行時は寛容、CI は厳格",
    "//!",
    "//! `deny_unknown_fields` は**付けない**。前方互換はこの結合の無料の利点で、",
    "//! 捨てる理由が無い。result 封筒は `type` タグを**検査しない**のも同じ判断で、",
    "//! タグの改名を報せるのはコミット済み schema の差分（マージ前）であって、",
    "//! 実行時の失敗ではない。",
    "",
    "/// totsuka が herdr へ**送る**型。",
    "///",
    "/// `#[serde(other)]` はここには無い。知らない値を送り返すことになるうえ、",
    "/// 送る側の未知バリアントは「herdr が知らない値を totsuka が作った」という",
    "/// totsuka 自身のバグだからである。未設定の任意フィールドはキーごと落とす",
    "/// （`skip_serializing_if`）— 明示的な `null` が「未指定」と同じ扱いを",
    "/// されるとは限らない。",
    (wrap_mod("request"; "Serialize";
      (.request_defs | to_entries | sort_by(.key)
       | map(gen_def(.key; .value; true; "Debug, Clone, Serialize")
             | gsub("(?m)^(?<l>.)"; "    \(.l)") | gsub("(?m)^    $"; ""))
       | join("\n\n")))),
    "",
    "/// totsuka が herdr から**読む**型。",
    "///",
    "/// 封筒（`*Envelope`）は `result` オブジェクトそのものの形で、`type` タグの",
    "/// フィールドは持たない（上記「実行時は寛容」）。",
    (wrap_mod("result"; "Deserialize";
      ((.result_defs | to_entries | sort_by(.key)
        | map(gen_def(.key; .value; false; "Debug, Clone, Deserialize")))
       + (.results | map(gen_struct((.properties.type.const | variant_name) + "Envelope"; .; ["type"]; false; "Debug, Clone, Deserialize")))
       | map(gsub("(?m)^(?<l>.)"; "    \(.l)") | gsub("(?m)^    $"; ""))
       | join("\n\n")))),
    ""
  ] | join("\n"))
JQ

generate() { # <sliced schema path>
  jq -r "$GEN_JQ"$'\n'"$EMIT_JQ" "$1"
}

# ---------------------------------------------------------------- entry
floor() { jq -r '.floor' "$SPEC"; }

if [[ "${1:-}" == "--fetch" ]]; then
  tag="${2:?usage: $0 --fetch <tag>   例: --fetch v0.8.2}"
  version="${tag#v}"
  tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
  # `herdr api schema --json` はこのファイルを include_str! しているだけなので、
  # herdr のインストールは要らない。
  curl -sfL -o "$tmp" "$UPSTREAM/$tag/docs/next/api/herdr-api.schema.json" \
    || { echo "herdr-types: $tag の schema を取得できません" >&2; exit 1; }
  slice "$tmp" "$version" > "$SCHEMA_DIR/herdr-$version.json"
  echo "herdr-types: $SCHEMA_DIR/herdr-$version.json を書きました" >&2
  exit 0
fi

src="$SCHEMA_DIR/herdr-$(floor).json"
[[ -f "$src" ]] || { echo "herdr-types: 下限版のスライスがありません: $src" >&2; exit 1; }
# 一時ファイルへ書いてから差し替える。`> "$OUT"` で直接書くと、生成が
# フェイルクローズした瞬間に `$OUT` は**切り詰められた後**で、失敗した実行が
# 生成物を破壊する。
tmp_out="$(mktemp)"; trap 'rm -f "$tmp_out"' EXIT
generate "$src" > "$tmp_out"
# **rustfmt は必須で、任意ではない。** 実測で、整形の有無は生成結果を変える
# （1 フィールドの enum バリアントが 1 行に畳まれる）。無ければ生成物が環境で
# 揺れ、drift 検査がその揺れを差分として報告してしまう。
rustfmt --edition "$EDITION" "$tmp_out"
mv "$tmp_out" "$OUT"
echo "herdr-types: $OUT を生成しました（herdr $(floor) のスライスから）" >&2
