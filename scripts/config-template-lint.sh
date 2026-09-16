#!/usr/bin/env bash
# config-template-lint.sh — config.toml 雛形の網羅性 Fitness Function（依存: POSIX awk/grep）
#
# `totsuka setup` が書き出す雛形（crates/orchestrator-cli/templates/config.toml）に、
# 設定スキーマの全キーがコメント付きで載っていることを機械検証する。
#
# チェック内容:
#   [E] missing-key : config の struct にあるフィールドが雛形に現れない（足し忘れ）
#   [E] unknown-key : 雛形にあるキーがどの config struct にも無い（タイポ・削除残り）
#
# 使い方: scripts/config-template-lint.sh
# 終了コード: 違反 1 件以上で 1、前提ツール欠如・検査自体の失敗で 2
#
# ---------------------------------------------------------------------------
# なぜ Rust のテストではなく bash なのか
#
# 雛形は orchestrator-cli が持つが、キーの過半は plugins/* の config struct が
# 決める。`scripts/arch-lint.sh` の境界により orchestrator-cli は plugins/* に
# 依存できないので、Rust 側のテストからは plugins/*/src/config.rs が原理的に
# 見えない。クレート境界の外から両方を読める場所は、ここしか無い。
#
# フィールド名 ≒ TOML キー名であることに寄りかかっている。config の struct は
# フィールドに serde(rename) をほぼ使っていない（rename_all は enum の変種名向け）
# ため実用的な精度が出るが、`#[serde(rename = "…")]` は下の awk が解釈する。
#
# フェイルクローズ: 抽出パイプライン自体の失敗は「違反なし」ではなくエラー終了に
# する。素通りしたら Fitness Function の意味がない。
# ---------------------------------------------------------------------------
set -euo pipefail

# ---------------------------------------------------------------------------
# 免除リスト。
#
# 「config struct にはあるが、雛形に載せない」を**意図して**置く場合だけ
# `<key>=<理由>` を 1 行で書く。空なら免除なし。
#
# このリストの存在が検査の主眼である: 載せ忘れと、意図した除外とを区別できる
# ようにするためにある。理由なしで足さないこと。
# ---------------------------------------------------------------------------
TEMPLATE_EXEMPT=""

command -v awk >/dev/null 2>&1 || {
  echo "config-template-lint: awk が必要です" >&2
  exit 2
}

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$ROOT/crates/orchestrator-cli/templates/config.toml"

[ -f "$TEMPLATE" ] || {
  echo "config-template-lint: 雛形が見つかりません: $TEMPLATE" >&2
  exit 2
}

# 走査対象の config struct。core の schema.rs と、各プラグインの config.rs。
# プラグインはパスのグロブで拾うので、新プラグイン追加時にこのファイルの更新は
# 要らない（arch-lint が plugins/ 配下をパスで判定しているのと同じ方針）。
SOURCES="$ROOT/crates/orchestrator-core/src/config/schema.rs"
for f in "$ROOT"/plugins/*/src/config.rs; do
  [ -f "$f" ] && SOURCES="$SOURCES $f"
done

# ---------- 1) コード側のキー ----------
#
# struct の `pub <ident>:` だけを拾う（`pub fn` は `:` が続かないので落ちる）。
# 直前の #[serde(...)] を見て flatten / skip は除外し、rename は差し替える。
# #[cfg(test)] 以降は読まない（テスト用の struct を本番キーと混ぜない）。
extract_code_keys() {
  # shellcheck disable=SC2086
  awk '
    FNR == 1 { in_test = 0; item = ""; rename = ""; drop = 0; derives = 0 }
    in_test { next }
    /^#\[cfg\(test\)\]/ { in_test = 1; next }

    # Deserialize を導出する型だけが TOML のキーを決める。これを見ないと
    # `ConfigError::EnvOverride { var, reason }` のようなエラー enum の
    # フィールドまで設定キーとして数えてしまう。
    /^#\[derive\(/ { derives = ($0 ~ /Deserialize/); next }

    # struct / enum の本体だけを読む。impl ブロックや自由関数の中の
    # `Foo { bar: 1 }` を誤ってフィールドとして拾わないための境界である。
    /^pub (struct|enum) [A-Za-z0-9_]+.*\{/ {
      item = (derives ? "on" : ""); derives = 0; rename = ""; drop = 0; next
    }
    /^\}/ { item = ""; rename = ""; drop = 0; next }
    item == "" { next }

    /^[[:space:]]*#\[serde\(/ {
      if ($0 ~ /flatten/) drop = 1
      if ($0 ~ /skip\)/ || $0 ~ /skip,/) drop = 1
      if (match($0, /rename[[:space:]]*=[[:space:]]*"[^"]+"/)) {
        r = substr($0, RSTART, RLENGTH)
        sub(/^rename[[:space:]]*=[[:space:]]*"/, "", r)
        sub(/"$/, "", r)
        rename = r
      }
      next
    }

    # `pub x: T,` も、enum の struct variant の中の `x: T,` も、どちらも
    # TOML のキーである。前者は struct のフィールド、後者は
    # `{ retention_days = 5 }` のように書かれる。
    /^[[:space:]]+(pub )?[a-z_][a-z0-9_]*:/ {
      name = $1
      if (name == "pub") name = $2
      sub(/:.*$/, "", name)
      if (name ~ /^[a-z_][a-z0-9_]*$/) {
        if (rename != "") name = rename
        if (!drop) print name
      }
      rename = ""; drop = 0
      next
    }

    # 属性でもフィールドでもない行は、直前の属性の効力を打ち切る
    /^[[:space:]]*(\/\/|$)/ { next }
    { rename = ""; drop = 0 }
  ' $SOURCES | sort -u
}

# ---------- 2) 雛形側のキー ----------
#
# 行頭の `#` を 1 つだけ剥がしてから読む。拾うのは
#   - `key = …` で始まる行（同じ行の inline table の内側キーも拾う）
#   - `[a.b]` / `[[a]]` のテーブル見出しの各セグメント
# の 2 つだけ。散文のコメントを誤って拾わないための制限である。
extract_template_keys() {
  awk '
    { line = $0; sub(/^[[:space:]]*#[[:space:]]?/, "", line) }
    line ~ /^\[\[?[a-z_][a-z0-9_.]*\]\]?/ {
      hdr = line
      sub(/^\[+/, "", hdr); sub(/\]+.*$/, "", hdr)
      n = split(hdr, seg, ".")
      for (i = 1; i <= n; i++) if (seg[i] ~ /^[a-z_][a-z0-9_]*$/) print seg[i]
      next
    }
    line ~ /^[a-z_][a-z0-9_]*[[:space:]]*=/ {
      rest = line
      while (match(rest, /[a-z_][a-z0-9_]*[[:space:]]*=/)) {
        k = substr(rest, RSTART, RLENGTH)
        sub(/[[:space:]]*=$/, "", k)
        print k
        rest = substr(rest, RSTART + RLENGTH)
      }
    }
  ' "$TEMPLATE" | sort -u
}

# ---------- 3) プラグイン名 ----------
#
# `[github]` のようなプラグイン設定テーブルの見出しは、どの struct のフィールド
# でもなくプラグインの名前である。plugin.toml から引いて許可する（列挙しない）。
plugin_names() {
  for m in "$ROOT"/plugins/*/plugin.toml; do
    [ -f "$m" ] || continue
    awk -F'"' '/^name[[:space:]]*=/ { print $2; exit }' "$m"
  done | sort -u
}

CODE_KEYS="$(extract_code_keys)" || {
  echo "config-template-lint: config struct の走査に失敗" >&2
  exit 2
}
TEMPLATE_KEYS="$(extract_template_keys)" || {
  echo "config-template-lint: 雛形の走査に失敗" >&2
  exit 2
}
PLUGINS="$(plugin_names)" || {
  echo "config-template-lint: plugin.toml の走査に失敗" >&2
  exit 2
}

[ -n "$CODE_KEYS" ] || {
  echo "config-template-lint: config struct から 1 件もキーを抽出できませんでした（抽出器の破損を疑うこと）" >&2
  exit 2
}

ERRORS=0
error() {
  echo "ERROR [$1] $2: $3"
  ERRORS=$((ERRORS + 1))
}

contains() { printf '%s\n' "$1" | grep -qxF "$2"; }

exempt_reason() {
  printf '%s\n' "$TEMPLATE_EXEMPT" | awk -F= -v k="$1" '$1 == k { sub(/^[^=]*=/, ""); print; exit }'
}

# ---------- 4) 足し忘れ（コード → 雛形）----------
while IFS= read -r key; do
  [ -n "$key" ] || continue
  contains "$TEMPLATE_KEYS" "$key" && continue
  [ -n "$(exempt_reason "$key")" ] && continue
  error missing-key "$TEMPLATE" \
    "config struct のキー '$key' が雛形に載っていない: コメント付きで追記するか、TEMPLATE_EXEMPT に理由付きで登録すること"
done <<<"$CODE_KEYS"

# ---------- 5) タイポ・削除残り（雛形 → コード）----------
while IFS= read -r key; do
  [ -n "$key" ] || continue
  contains "$CODE_KEYS" "$key" && continue
  contains "$PLUGINS" "$key" && continue
  error unknown-key "$TEMPLATE" \
    "雛形のキー '$key' がどの config struct にも無い: 綴りを直すか、削除済みなら雛形からも消すこと"
done <<<"$TEMPLATE_KEYS"

# ---------- サマリ ----------
N_CODE="$(printf '%s\n' "$CODE_KEYS" | grep -c . || true)"
N_TMPL="$(printf '%s\n' "$TEMPLATE_KEYS" | grep -c . || true)"
N_SRC="$(printf '%s\n' $SOURCES | grep -c . || true)"
echo ""
echo "config-template-lint: ${ERRORS} error(s)（config struct ${N_SRC} ファイル / コード側のキー ${N_CODE} 個 / 雛形のキー ${N_TMPL} 個を照合）"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
