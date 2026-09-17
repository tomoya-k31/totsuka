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
# **Rust には struct のフィールドを列挙する手段が無い**。リフレクションが無く、
# 導出マクロを新設しない限り、テストは「このキーの一覧」を手で書き写すことに
# なる —— 写した一覧こそが次にズレるものなので、検査の意味が消える。
# ソースをテキストとして読めば、その一覧は書き写さずに得られる。
#
# 副次的な理由として、キーの過半は plugins/* の config struct が決めるが、
# orchestrator-cli がそれらに張っている依存は github / slack の 2 本だけで、
# しかも dev-dependency である（`ai-docs/architecture/workspace-dependency-rules.md`）。
# 検査のために 7 本ぶん張ると、CLI の dev ビルドに全プラグインが入る。
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

# ---------------------------------------------------------------------------
# 雛形に書いてよい「struct を持たないキー」。
#
# `[[workflows]].trigger` と、プラグインが `[[workflows]]` へフラットに足す
# 追加プロパティは、core が `toml::Table` のまま保持してプラグインへ渡す
# （#554）。解釈するのはプラグイン側のコードで、config struct のフィールドには
# ならないため、どれだけ正しく書いても unknown-key に見える。
#
# `<key>=<理由>` を 1 行で書く。これも理由なしで足さないこと —— タイポを
# 通す穴になる。
# ---------------------------------------------------------------------------
OPAQUE_ALLOWED="
reaction=[[workflows]].trigger。slack が絵文字でワークフローを選ぶ（ADR-0025）
from_bot=[[workflows]].trigger。その絵文字を使ってよい bot 投稿の許可リスト（ADR-0079）
channel=[[workflows]].trigger。チャンネル監視トリガの宣言そのもの（ADR-0068）
channel_name=[[workflows]].trigger。監視対象チャンネルの照合名（ADR-0068）
repo=[[workflows]].trigger。監視トリガが固定するリポジトリ（ADR-0068）
from=[[workflows]].trigger。監視トリガで起動を許す投稿者（ADR-0068）
publish=[[workflows]] の追加プロパティ。slack の承認フロー切り替え（ADR-0057）
"

for tool in awk grep; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "config-template-lint: ${tool} が必要です" >&2
    exit 2
  }
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEMPLATE="$ROOT/crates/orchestrator-cli/templates/config.toml"

[ -f "$TEMPLATE" ] || {
  echo "config-template-lint: 雛形が見つかりません: $TEMPLATE" >&2
  exit 2
}

# 走査対象の config struct。core の schema.rs と、各プラグインの config.rs。
# プラグインはパスのグロブで拾うので、新プラグイン追加時にこのファイルの更新は
# 要らない（arch-lint が plugins/ 配下をパスで判定しているのと同じ方針）。
SOURCES=("$ROOT/crates/orchestrator-core/src/config/schema.rs")
for f in "$ROOT"/plugins/*/src/config.rs; do
  [ -f "$f" ] && SOURCES+=("$f")
done

# ---------- 1) コード側のキー ----------
#
# struct の `pub <ident>:` だけを拾う（`pub fn` は `:` が続かないので落ちる）。
# 直前の #[serde(...)] を見て flatten / skip は除外し、rename は差し替える。
# #[cfg(test)] 以降は読まない（テスト用の struct を本番キーと混ぜない）。
extract_code_keys() {
  # shellcheck disable=SC2086
  awk '
    FNR == 1 { in_test = 0; item = ""; rename = ""; drop = 0; derives = 0; in_derive = 0 }
    in_test { next }
    /^#\[cfg\(test\)\]/ { in_test = 1; next }

    # Deserialize を導出する型だけが TOML のキーを決める。これを見ないと
    # `ConfigError::EnvOverride { var, reason }` のようなエラー enum の
    # フィールドまで設定キーとして数えてしまう。
    #
    # rustfmt が折り返した derive を 1 行しか見ないと、trait が 1 つ増えた
    # 瞬間にその型のフィールドが丸ごと検査から消える（fail-open）。
    # `)]` が来るまで読み続ける。
    /^#\[derive\(/ { derives = ($0 ~ /Deserialize/); in_derive = ($0 !~ /\)\]/); next }
    in_derive {
      if ($0 ~ /Deserialize/) derives = 1
      if ($0 ~ /\)\]/) in_derive = 0
      next
    }

    # struct / enum の本体だけを読む。impl ブロックや自由関数の中の
    # `Foo { bar: 1 }` を誤ってフィールドとして拾わないための境界である。
    /^(pub )?(struct|enum) [A-Za-z0-9_]+/ {
      # 非 pub の型にも derive は付く。ここで消さないと、その derive が
      # 次に来る pub 型へ持ち越され、Deserialize しない型のフィールドが
      # 設定キーとして数えられる。
      item = (derives && /^pub (struct|enum) [A-Za-z0-9_]+.*\{/) ? "on" : ""
      derives = 0; rename = ""; drop = 0; next
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
  ' "${SOURCES[@]}" | sort -u
}

# ---------- 2) 雛形側のキー ----------
#
# 行頭の `#` を 1 つだけ剥がしてから読む。拾うのは
#   - `key = …` で始まる行（同じ行の inline table の内側キーも拾う）
#   - `[a.b]` / `[[a]]` のテーブル見出しの各セグメント
# の 2 つだけ。散文のコメントを誤って拾わないための制限である。
#
# 引数 `assign` を渡すと**代入行のキーだけ**を返す。見出しを外すのは、末尾の
# セグメントがユーザーの決めるインスタンス名になる見出しがあるため
# （`[tools.claude]` / `[notion.dynamic.sprint]` / `[macos.filter.workflows.<name>]`）。
# これらを「どの struct にも無いキー」として報告させないための区別であり、
# 見出しの綴り間違い自体は `totsuka config validate` が別途弾く
# （ロスターに無い名前のトップレベルテーブルは検証エラーになる）。
extract_template_keys() {
  awk -v mode="${1:-all}" '
    { raw = ($0 ~ /lint:raw/) }
    { line = $0; sub(/^[[:space:]]*#[[:space:]]?/, "", line) }
    line ~ /^\[\[?[a-z_][a-z0-9_.-]*\]\]?/ {
      if (mode == "assign") next
      hdr = line
      sub(/^\[+/, "", hdr); sub(/\]+.*$/, "", hdr)
      n = split(hdr, seg, ".")
      for (i = 1; i <= n; i++) if (seg[i] ~ /^[a-z_][a-z0-9_]*$/) print seg[i]
      next
    }
    line ~ /^[a-z_][a-z0-9_]*[[:space:]]*=/ {
      k = line
      sub(/[[:space:]]*=.*$/, "", k)
      print k
      # inline table の中のキーも設定キーである（`cleanup = { retention_days = 3 }`）。
      # 走査を `{` 〜 最後の `}` に閉じ込めるのは、値の後ろに続く散文の
      # 「unset = no -activate」のような字面を拾わないため。
      #
      # `lint:raw` はここだけを止める。**左辺のキー名は数える** ——
      # `filter = { property = … }` の `filter` は totsuka の設定キーであり、
      # 行ごと読み飛ばすと綴り間違いが素通りする（`fillter` と書いても
      # 0 error になる）。止めたいのは右辺、つまり第三者の DSL の語彙だけ。
      if (!raw && match(line, /\{.*\}/)) {
        rest = substr(line, RSTART, RLENGTH)
        while (match(rest, /[a-z_][a-z0-9_]*[[:space:]]*=/)) {
          k = substr(rest, RSTART, RLENGTH)
          sub(/[[:space:]]*=$/, "", k)
          print k
          rest = substr(rest, RSTART + RLENGTH)
        }
      }
    }
  ' "$TEMPLATE" | sort -u
}

CODE_KEYS="$(extract_code_keys)" || {
  echo "config-template-lint: config struct の走査に失敗" >&2
  exit 2
}
TEMPLATE_KEYS="$(extract_template_keys all)" || {
  echo "config-template-lint: 雛形の走査に失敗" >&2
  exit 2
}
TEMPLATE_ASSIGNED="$(extract_template_keys assign)" || {
  echo "config-template-lint: 雛形の走査に失敗" >&2
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

opaque_reason() {
  printf '%s\n' "$OPAQUE_ALLOWED" | awk -F= -v k="$1" '$1 == k { sub(/^[^=]*=/, ""); print; exit }'
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
  [ -n "$(opaque_reason "$key")" ] && continue
  error unknown-key "$TEMPLATE" \
    "雛形のキー '$key' がどの config struct にも無い: 綴りを直すか、削除済みなら雛形からも消すか、プラグインが解釈する無解釈テーブルのキーなら OPAQUE_ALLOWED に理由付きで登録すること"
done <<<"$TEMPLATE_ASSIGNED"

# ---------- 6) 死んだ宣言 ----------
#
# 免除・許可はどちらも「検査の穴」なので、要らなくなったら消えてほしい。
# 消えないと、汎用的な名前（`repo` / `from` / `channel`）の素通し口が
# 残り続け、後から入った本物のタイポをそこで受け止めてしまう。
# arch-lint の declaration-consumed と同じ発想である。
while IFS= read -r entry; do
  key="${entry%%=*}"
  [ -n "$key" ] || continue
  # 生きている免除は「コードにあって雛形に無いキー」——`missing-key` を
  # 抑えているもの。死ぬのはその逆の 2 通りで、どちらも免除が仕事をして
  # いない。
  if ! contains "$CODE_KEYS" "$key"; then
    error dead-declaration "$TEMPLATE" \
      "TEMPLATE_EXEMPT の '$key' はもう config struct に無い（削除か改名）: 免除ごと消すこと"
  elif contains "$TEMPLATE_KEYS" "$key"; then
    error dead-declaration "$TEMPLATE" \
      "TEMPLATE_EXEMPT の '$key' は雛形に載っているので免除が要らない: 免除ごと消すこと"
  fi
done <<<"$TEMPLATE_EXEMPT"

while IFS= read -r entry; do
  key="${entry%%=*}"
  [ -n "$key" ] || continue
  contains "$TEMPLATE_ASSIGNED" "$key" && continue
  error dead-declaration "$TEMPLATE" \
    "OPAQUE_ALLOWED の '$key' を雛形が書いていない: 使うか、宣言ごと消すこと（汎用的な名前の素通し口が残るとタイポを受け止めてしまう）"
done <<<"$OPAQUE_ALLOWED"

# ---------- サマリ ----------
N_CODE="$(printf '%s\n' "$CODE_KEYS" | grep -c . || true)"
N_TMPL="$(printf '%s\n' "$TEMPLATE_KEYS" | grep -c . || true)"
N_SRC="${#SOURCES[@]}"
echo ""
echo "config-template-lint: ${ERRORS} error(s)（config struct ${N_SRC} ファイル / コード側のキー ${N_CODE} 個 / 雛形のキー ${N_TMPL} 個を照合）"
[ "$ERRORS" -eq 0 ] || exit 1
exit 0
