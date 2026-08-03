#!/usr/bin/env bash
# サンドボックス repo 2 つ・ProjectsV2・seed Issue を作る（冪等: 既存はスキップ）。
set -euo pipefail
: "${E2E_GH_OWNER:?source .env してください}"
OWNER="$E2E_GH_OWNER"
WEB="${E2E_GH_REPO_WEB:-totsuka-sandbox-web}"
CLI="${E2E_GH_REPO_CLI:-totsuka-sandbox-cli}"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

mk_repo() {  # mk_repo <name> <desc>
  gh repo view "$OWNER/$1" >/dev/null 2>&1 && { echo "skip (exists): $1"; return; }
  gh repo create "$OWNER/$1" --private --description "$2" >/dev/null
  echo "created: $1"
}
mk_repo "$WEB" "totsuka 実機検証用サンドボックス（Web アプリ想定）"
mk_repo "$CLI" "totsuka 実機検証用サンドボックス（CLI ツール想定）"

# 中身は「依存ゼロ・数秒で終わる」ことが要件。dispatch のたびに新しい worktree が
# 切られるので、そこでパッケージ解決が走る構成は検証のノイズになる。
seed_repo() {  # seed_repo <name> <pkg> <summary> <not-summary>
  local name="$1" pkg="$2" sum="$3" nots="$4" d="$WORK/$name"
  gh api "repos/$OWNER/$name/branches" --jq '.[].name' 2>/dev/null | grep -q main && {
    echo "skip (seeded): $name"; return; }
  mkdir -p "$d/$pkg" "$d/tests"
  printf '__pycache__/\n*.pyc\n.pytest_cache/\n' > "$d/.gitignore"
  cat > "$d/README.md" <<EOF
# $name

$sum

扱わない領域: $nots

## セットアップ

\`\`\`bash
python3 -m unittest discover -s tests -t .
\`\`\`

規約は [CLAUDE.md](CLAUDE.md) を参照。

> これは totsuka の実機検証用サンドボックスであり、実際のサービスではない。
EOF
  cat > "$d/CLAUDE.md" <<'EOF'
# CLAUDE.md

## ブランチ規約

`<type>/<slug>`（すべて小文字、区切りは `-`）。
`type` は `feat` / `fix` / `docs` / `refactor` / `test` / `chore` のいずれか。

worktree は detached HEAD で渡されるので、**作業前に必ず `git switch -c <branch>` する**。

## コミット

Conventional Commits 1.0.0。1 コミット 1 完結単位。

## テスト

```bash
python3 -m unittest discover -s tests -t .
```

依存パッケージは不要（標準ライブラリの unittest のみ）。緑にならない状態でコミットしない。

## 作業が終わったら

1. `git push -u origin <branch>`
2. `gh pr create --fill`

**push と PR 作成まで行って初めて「完了」である。**

## 方針

totsuka の実機検証用サンドボックス。変更は使い捨てでよいが、上の規約は検証対象なので必ず守る。
EOF
  touch "$d/$pkg/__init__.py" "$d/tests/__init__.py"
  if [ "$pkg" = webapp ]; then
    cat > "$d/$pkg/text.py" <<'EOF'
"""画面表示用の文字列整形ヘルパー。"""


def initials(full_name: str) -> str:
    """氏名からアバター用のイニシャルを作る。"""
    parts = [p for p in full_name.split() if p]
    return "".join(p[0].upper() for p in parts[:2])
EOF
    cat > "$d/tests/test_text.py" <<'EOF'
import unittest

from webapp.text import initials


class InitialsTest(unittest.TestCase):
    def test_takes_first_two_words(self):
        self.assertEqual(initials("Ada Lovelace"), "AL")

    def test_handles_single_word(self):
        self.assertEqual(initials("Cher"), "C")


if __name__ == "__main__":
    unittest.main()
EOF
  else
    cat > "$d/$pkg/summarize.py" <<'EOF'
"""ログ行の集計ロジック。"""

from collections import Counter


def count_levels(lines: list[str]) -> dict[str, int]:
    """`LEVEL message` 形式の行を数える。"""
    counter: Counter[str] = Counter()
    for line in lines:
        head = line.split(maxsplit=1)
        if head:
            counter[head[0]] += 1
    return dict(counter)
EOF
    cat > "$d/tests/test_summarize.py" <<'EOF'
import unittest

from logtool.summarize import count_levels


class CountLevelsTest(unittest.TestCase):
    def test_counts_first_token(self):
        self.assertEqual(count_levels(["INFO ok", "ERROR bad", "INFO fine"]),
                         {"INFO": 2, "ERROR": 1})

    def test_ignores_blank_lines(self):
        self.assertEqual(count_levels(["", "   ", "WARN hmm"]), {"WARN": 1})


if __name__ == "__main__":
    unittest.main()
EOF
  fi
  git -C "$d" init -q -b main
  git -C "$d" remote add origin "https://github.com/$OWNER/$name.git"
  git -C "$d" add -A
  git -C "$d" -c commit.gpgsign=false -c user.name=totsuka-e2e -c user.email=e2e@example.invalid \
      commit -q -m "chore: totsuka 実機検証用サンドボックスの初期化"
  git -C "$d" push -q -u origin main
  echo "seeded: $name"
}
# summary のドメインをはっきり離す。似ていると LLM 分類の confidence が閾値を割り、
# 分類が効いているのか縮退なのか区別できなくなる。
seed_repo "$WEB" webapp \
  "顧客向けの **Web アプリケーション**。画面表示・HTTP API・認証まわりを扱う。" \
  "バッチ処理、ログ集計、コマンドラインツール。"
seed_repo "$CLI" logtool \
  "サーバログを集計する **コマンドラインツール**。端末から実行して結果を標準出力に流す。" \
  "画面、HTTP API、認証、ブラウザ向けの何か。"

# seed Issue は「新規追加型」にする。既存コードの書き換え課題にすると、2 回目の実行で
# 「もう終わっている」状態になり判定がぶれる。
mk_issue() { gh issue list -R "$OWNER/$1" --json title --jq '.[].title' | grep -qF "$2" \
  || gh issue create -R "$OWNER/$1" -t "$2" -b "$3" >/dev/null; }
mk_issue "$WEB" "feat: slugify 関数を追加する" \
'## やること
- `webapp/text.py` に `slugify(title: str) -> str` を追加する
- 英数字以外は `-` に置換し、連続する `-` は 1 つに、前後の `-` は落とす

## 受け入れ条件
- `slugify("Hello, World!") == "hello-world"`
- テストがあり `python3 -m unittest discover -s tests -t .` が緑'
mk_issue "$WEB" "feat: truncate 関数を追加する" \
'## やること
- `webapp/text.py` に `truncate(text: str, limit: int) -> str` を追加する
- `limit` を超えたら末尾を `…` にする（`…` 込みで `limit` 文字）

## 受け入れ条件
- `truncate("abcdef", 4) == "abc…"`
- テストがあり緑'
mk_issue "$WEB" "docs: README に関数一覧を追記する" \
'## やること
`README.md` に公開関数の一覧表を追記する。コードは変更しない。'
mk_issue "$CLI" "feat: count_by_hour 関数を追加する" \
'## やること
- `logtool/summarize.py` に `count_by_hour(lines: list[str]) -> dict[int, int]` を追加する
- 行の形式は `HH:MM LEVEL message`。先頭の `HH` を時として数える

## 受け入れ条件
- `count_by_hour(["09:12 INFO a", "10:00 INFO c"]) == {9: 1, 10: 1}`
- テストがあり緑'
mk_issue "$CLI" "feat: format_table 関数を追加する" \
'## やること
- `logtool/summarize.py` に `format_table(rows: list[tuple[str, int]]) -> str` を追加する
- 左列は最長キーに合わせて左詰め、列間は 2 スペース、末尾に改行を付けない

## 受け入れ条件
- テストがあり緑'

# Project は既存があれば使う
NUM="$(gh project list --owner "$OWNER" --format json \
  | python3 -c 'import json,sys;print(next((p["number"] for p in json.load(sys.stdin)["projects"] if p["title"]=="totsuka e2e"),""))')"
if [ -z "$NUM" ]; then
  gh project create --owner "$OWNER" --title "totsuka e2e" >/dev/null
  NUM="$(gh project list --owner "$OWNER" --format json \
    | python3 -c 'import json,sys;print(next(p["number"] for p in json.load(sys.stdin)["projects"] if p["title"]=="totsuka e2e"))')"
  echo "created project #$NUM"
else
  echo "skip (exists): project #$NUM"
fi

for r in "$WEB" "$CLI"; do
  gh issue list -R "$OWNER/$r" --json url --jq '.[].url' | while read -r u; do
    gh project item-add "$NUM" --owner "$OWNER" --url "$u" >/dev/null 2>&1 || true
  done
done

cat <<MSG

==> Project #$NUM を .env の E2E_GH_PROJECT に設定してください
==> 初期状態は 'bash scripts/github.sh status' で確認できます
    検証を始める前に、cli の 1 件を In Progress にしておくと F-08 の対照になります
MSG
