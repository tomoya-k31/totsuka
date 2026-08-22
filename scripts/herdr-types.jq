# herdr API schema → Rust の型対応。**`herdr-types-build.sh` と
# `herdr-schema-check.sh` の両方が読む。** 生成と検査が同じ対応表を見ていないと、
# 「生成できる」と「互換である」が別々の意味になってしまう — 検査は
# 「各プロパティが写る Rust の型が変わっていないか」を見るので、写像そのものが
# 共有されている必要がある。
#
# 教えていない JSON Schema 構文では `die` する（フェイルクローズ）。
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
# **`float` も f64 に写す。** JSON に float32 は無く、`format: "float"` は herdr の
# **内部の**型を述べているだけである。f32 を使うと、設定に書かれた `0.65` が
# 送信時に `0.6499999761581421` になる — 同じ f32 だが同じ文字列ではなく、
# 運用者が目にするのは文字列のほうである（実測: `pane.split` の `ratio`）。
# f64 は f32 の全値を正確に保持するので、読む向きでも失うものは無い。
def num_type($fmt):
  ({"float":"f64","double":"f64"}[$fmt // "double"]) // die("知らない number format `\($fmt)`");

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

# `$ser` は「totsuka が送る側」。送る側は **`Option` のときだけ**キーごと落とす
# （明示的な `null` が herdr にとって「未指定」と同じとは限らないため）。
# **空のコレクションは落とさない** — 下の `field` の注記のとおり、空を送るのと
# 送らないのを同じにすると「空の env を渡した」と「env を一切渡していない」が
# 区別できなくなる。読む側にこの問題は無い。
def field($name; $schema; $required; $ser):
  ($schema | rust_type) as $ty
  | ($name | ident) as $id
  | (if ($ty | startswith("Vec<")) or ($ty | startswith("BTreeMap<")) or ($ty | startswith("Option<"))
     then $ty else "Option<\($ty)>" end) as $opt
  # 送る側で **`Option` のときだけ**キーごと落とす。`None` は「指定していない」で
  # あって値ではないが、**空のコレクションは値である** — 空を送るのと送らないのを
  # 同じにしてしまうと、「空の env を渡した」と「env を一切渡していない」が
  # 区別できなくなる。読む側にこの問題は無いので `default` だけでよい。
  | if $required then "    pub \($id): \($ty),"
    elif $ser and ($opt | startswith("Option<"))
      then "    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n    pub \($id): \($opt),"
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
