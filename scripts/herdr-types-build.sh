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
# そのものなので定義上読める。新しい版については、未知フィールド無視 +
# `#[serde(other)]` に加えて、**下限版の型で読めること自体を CI の schema 差分が
# 検査する**（削除・プロパティの型の入れ替え・`required` の向き・enum バリアントの
# 削除を落とす。検出しないのは `pattern` / `maxProperties` のような制約の厳格化と、
# schema に出ない振る舞いの変化）。逆向き（最新版から生成）は、新しく `required`
# になったフィールドを古い版が送らずデシリアライズが落ちるので採らない。
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
GEN_JQ="$(cat "$ROOT/scripts/herdr-types.jq")"

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
    "//! 定義上読める。新しい版は未知フィールド無視 + `#[serde(other)]` で読み、",
    "//! **下限版の型で読めること自体を CI の schema 差分が検査する**（削除・",
    "//! プロパティの型の入れ替え・`required` の向き・enum バリアントの削除を",
    "//! 落とす。検出しないのは `pattern` / `maxProperties` のような制約の厳格化と、",
    "//! schema に出ない振る舞いの変化）。",
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
