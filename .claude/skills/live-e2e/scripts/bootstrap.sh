#!/usr/bin/env bash
# $E2E_HOME を作り、プラグインを install し、設定の雛形を置く。
# 既存の設定は上書きしない（値を埋めたものを潰さないため）。
#
#   source .env && bash .claude/skills/live-e2e/scripts/bootstrap.sh
set -euo pipefail
# `tt` はシェル関数なので子プロセスには継承されない。共通定義を読む。
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

: "${E2E_HOME:?source .env してください}"
SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "$SKILL_DIR/../../.." && pwd)"

echo "==> repo=$REPO"
echo "==> E2E_HOME=$E2E_HOME"

mkdir -p "$E2E_HOME"/{cfg/totsuka,data/totsuka/plugins,state/totsuka,cache,repo,pkg}

echo "==> cargo build --workspace"
(cd "$REPO" && cargo build --workspace >/dev/null)

# バイナリ名 = plugin.toml の name という不変条件があるので、その名前で配置する。
stage() {  # stage <plugin-name> <manifest-path> <built-binary>
  local name="$1" manifest="$2" bin="$3"
  mkdir -p "$E2E_HOME/pkg/$name"
  cp "$manifest" "$E2E_HOME/pkg/$name/plugin.toml"
  ln -f "$bin" "$E2E_HOME/pkg/$name/$name" 2>/dev/null || cp "$bin" "$E2E_HOME/pkg/$name/$name"
}
stage slack  "$REPO/plugins/task-source-slack/plugin.toml"  "$REPO/target/debug/slack"
stage github "$REPO/plugins/task-source-github/plugin.toml" "$REPO/target/debug/github"
stage herdr  "$REPO/plugins/agent-ide-herdr/plugin.toml"    "$REPO/target/debug/herdr"
stage mock_agent "$SKILL_DIR/assets/cfg/mock-agent.plugin.toml" "$REPO/target/debug/mock_plugin"

# 設定は「無ければ置く」。値を埋めたものを毎回潰すと使い物にならない。
place() {  # place <src> <dst>
  if [ -e "$2" ]; then
    echo "    skip (exists): $2"
  else
    cp "$1" "$2"; echo "    placed: $2"
  fi
}
echo "==> 設定の配置"
# 設定は 1 本だけ（#554。プラグイン個別設定も [<name>] としてこの中にある）
place "$SKILL_DIR/assets/cfg/config.toml"          "$E2E_HOME/cfg/totsuka/config.toml"

echo "==> プラグインの install"
for name in slack github herdr mock_agent; do
  tt plugin install "$E2E_HOME/pkg/$name" --yes >/dev/null && echo "    $name"
done

echo "==> サンドボックスのクローン"
for r in "${E2E_GH_REPO_WEB:-}" "${E2E_GH_REPO_CLI:-}"; do
  [ -n "$r" ] || continue
  if [ -d "$E2E_HOME/repo/$r" ]; then
    echo "    skip (exists): $r"
  else
    gh repo clone "${E2E_GH_OWNER}/$r" "$E2E_HOME/repo/$r" -- -q && echo "    cloned: $r"
  fi
done

cat <<'MSG'

==> 次にやること
  1. $E2E_HOME/cfg/totsuka/config.toml の ________ を埋める（[slack] / [github] / [[projects]]）
  2. source .env && tt config validate
  3. source .env && tt doctor      # state-db の fail は run 前なら正常
  4. 人間のターミナルで: source .env && tt run --watch
MSG
