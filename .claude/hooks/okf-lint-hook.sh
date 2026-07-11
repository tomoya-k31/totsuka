#!/usr/bin/env bash
# PostToolUse hook: Edit/Write が docs/ 配下を触った場合のみ okf-lint を実行する。
# 終了コード 2 で stderr が Claude にフィードバックされ、Claude は自動で修正を試みる。
set -u

INPUT=$(cat)

# tool_input.file_path を抽出（jq があれば使う。なければ grep フォールバック）
if command -v jq >/dev/null 2>&1; then
  FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')
else
  FILE_PATH=$(echo "$INPUT" | grep -o '"file_path"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*:[[:space:]]*"\(.*\)"/\1/')
fi

# docs/ 配下でなければ何もしない
case "$FILE_PATH" in
  */docs/*|docs/*) ;;
  *) exit 0 ;;
esac

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT" || exit 0

LINT_OUT=$(bash scripts/okf-lint.sh docs --no-links 2>&1)
STATUS=$?

if [ $STATUS -ne 0 ]; then
  {
    echo "okf-lint がエラーを検出しました。docs/CLAUDE.md のルールに従い修正してください:"
    echo "$LINT_OUT"
  } >&2
  exit 2 # Claude にエラー内容をフィードバックして修正させる
fi
exit 0
