#!/usr/bin/env bash
# live-e2e-orca の orca 側の準備・観測。GitHub / Slack の駆動は live-e2e-herdr の
# scripts/（github.sh / slack.sh / report.sh）を共用する — task source の経路は
# エージェントが herdr でも orca でも同じだから。
#
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh preflight          # 前提がそろっているか（読むだけ）
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh use <orca|herdr>   # E2E 設定の agent を切り替える
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh sessions           # totsuka が開いた orca 端末の一覧
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh inspect <task-id>  # そのタスクの端末・worktree を判定
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh snapshot <task-id> # そのタスクの画面
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh exit-agent <task-id>  # エージェントを /exit させる（deadman 試験）
#   bash .claude/skills/live-e2e-orca/scripts/orca.sh cleanup-hints      # 後始末の対象を列挙（消さない）
#
# 前提: リポジトリルートで `source .env` 済み（E2E_HOME / E2E_TOTSUKA_BIN / tt）。
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
# `tt` の定義は herdr 版に 1 つだけある（シェル関数は子プロセスへ継承されない）。
# shellcheck source=../../live-e2e-herdr/scripts/_common.sh
. "$REPO/.claude/skills/live-e2e-herdr/scripts/_common.sh"

ORCA_BIN="${E2E_ORCA_BIN:-orca}"
CONFIG="$E2E_HOME/cfg/totsuka/config.toml"
PLUGIN_BIN="$E2E_HOME/data/totsuka/plugins/orca/orca"
REPO_WEB="$E2E_HOME/repo/${E2E_GH_REPO_WEB:-totsuka-sandbox-web}"
REPO_CLI="$E2E_HOME/repo/${E2E_GH_REPO_CLI:-totsuka-sandbox-cli}"

pass() { printf '  PASS  %s\n' "$*"; }
fail() {
  printf '  FAIL  %s\n' "$*"
  FAILED=1
}
note() { printf '  ----  %s\n' "$*"; }
FAILED=0

# orca の --json envelope から result を取り出す。ok:false は stderr へ code を出して 1。
# **終了コードだけで判定しない** — 拒否も stdout に envelope として出る。
orca_json() {
  local out
  out="$("$ORCA_BIN" "$@" --json 2>/dev/null)" || true
  python3 -c '
import json, sys
try:
    env = json.loads(sys.stdin.read())
except Exception:
    print("orca: not JSON", file=sys.stderr); sys.exit(2)
if env.get("ok"):
    json.dump(env.get("result"), sys.stdout)
else:
    print("orca error: " + str((env.get("error") or {}).get("code")), file=sys.stderr); sys.exit(1)
' <<<"$out"
}

# task show --json から orca のセッション（最新）を取り出す: "<handle>\t<worktree_path>"
task_session() {
  tt task show "$1" --json | python3 -c '
import json, sys
t = json.load(sys.stdin)
s = [s for s in t.get("sessions", []) if s.get("plugin") == "orca"]
if not s:
    print("task %s has no orca session (agent が orca ではない？)" % t.get("id"), file=sys.stderr); sys.exit(1)
print(s[0]["session_id"] + "\t" + (t.get("worktree_path") or ""))
'
}

cmd_preflight() {
  echo "== orca ランタイム"
  if status="$(orca_json status)"; then
    reach="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("runtime",{}).get("reachable"))' <<<"$status")"
    ver="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("runtime",{}).get("appVersion"))' <<<"$status")"
    [ "$reach" = "True" ] && pass "runtime reachable（orca ${ver}）" || fail "runtime に届かない → Orca アプリを起動（orca open）"
  else
    fail "orca status が答えない（orca_bin / インストールを確認）"
  fi

  echo "== サンドボックス repo の orca 登録と external worktree の表示"
  for p in "$REPO_WEB" "$REPO_CLI"; do
    if repo="$(orca_json repo show --repo "path:$p")"; then
      vis="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["repo"].get("externalWorktreeVisibility"))' <<<"$repo")"
      pass "登録済み: $p"
      [ "$vis" = "show" ] && pass "externalWorktreeVisibility = show" ||
        fail "externalWorktreeVisibility = $vis → totsuka の worktree がサイドバーに出ない（Orca の repo 設定で表示にする）"
    else
      fail "未登録: $p → 【承認が要る】orca repo add --path ${p}（Orca のサイドバーにプロジェクトが増える）"
    fi
  done

  echo "== インストール済みプラグイン（tt run が起動するのはこのコピー）"
  if [ -x "$PLUGIN_BIN" ]; then
    installed=$(stat -f %m "$PLUGIN_BIN")
    newest=$(find "$REPO/plugins/agent-ide-orca" \( -name '*.rs' -o -name '*.toml' \) -print0 | xargs -0 stat -f %m | sort -n | tail -1)
    [ "$installed" -ge "$newest" ] && pass "install はソースより新しい" ||
      fail "install がソースより古い → tt plugin install --from-source --yes orca"
  else
    fail "未インストール → tt plugin install --from-source --yes orca"
  fi

  echo "== E2E 設定（${CONFIG}）"
  python3 - "$CONFIG" <<'EOF' || FAILED=1
import sys, tomllib
cfg = tomllib.load(open(sys.argv[1], "rb"))
ok = True
p = cfg.get("plugins", {}).get("orca", {})
if p.get("enabled") and p.get("kind") == "agent_ide":
    print("  PASS  [plugins.orca] enabled")
else:
    print("  FAIL  [plugins.orca] が無効 → orca.sh use orca"); ok = False
agents = {w.get("name"): w.get("agent") for w in cfg.get("workflows", [])}
on_orca = [n for n, a in agents.items() if a == "orca"]
if on_orca:
    print("  PASS  agent = orca のワークフロー: " + ", ".join(on_orca))
else:
    print("  FAIL  agent = orca のワークフローが無い → orca.sh use orca"); ok = False
removed = [k for k in ("agent", "setup", "repo_selector", "plan_prompt_prefix", "poll_interval_ms") if k in cfg.get("orca", {})]
if removed:
    print("  FAIL  [orca] に廃止キー: " + ", ".join(removed) + "（initialize が CONFIG_INVALID になる）"); ok = False
if not cfg.get("hooks", {}).get("auth_token_ref"):
    print("  FAIL  [hooks].auth_token_ref が無い（orca は hook_completion を宣言する）"); ok = False
sys.exit(0 if ok else 1)
EOF
  [ "$FAILED" = 0 ] && echo "preflight: OK" || {
    echo "preflight: 未充足あり"
    exit 1
  }
}

# E2E 設定の [[workflows]].agent を herdr ⇄ orca で書き換える。バックアップを残す。
# 書き換えるのは隔離環境（${E2E_HOME}）の設定だけ。反映には tt run の再起動が要る。
cmd_use() {
  local to="${1:?orca か herdr を指定}"
  case "$to" in orca) from=herdr ;; herdr) from=orca ;; *)
    echo "orca か herdr" >&2
    exit 2
    ;;
  esac
  cp "$CONFIG" "$CONFIG.bak.$(date +%Y%m%d%H%M%S)"
  sed -i '' "s/^agent = \"$from\"/agent = \"$to\"/" "$CONFIG"
  if ! grep -q '^\[plugins\.orca\]' "$CONFIG"; then
    # [plugins.herdr] の直後に置く（ロスターの並びを保つ）。
    python3 - "$CONFIG" <<'EOF'
import sys
p = sys.argv[1]; s = open(p).read()
block = '[plugins.orca]\nenabled = true\nkind = "agent_ide"\n\n'
anchor = '[plugins.mock_agent]'
s = s.replace(anchor, block + anchor, 1) if anchor in s else s + '\n' + block
open(p, "w").write(s)
EOF
  fi
  grep -n '^agent = \|^\[plugins\.orca\]' "$CONFIG"
  echo "==> 反映には tt run の再起動が要る（【手動】人間のターミナルで Ctrl-C → source .env && tt run --watch）"
}

cmd_sessions() {
  orca_json terminal list --limit 500 | python3 -c '
import json, sys
ts = [t for t in json.load(sys.stdin)["terminals"] if (t.get("title") or "").startswith("totsuka ")]
if not ts:
    print("(totsuka の端末は無い)")
for t in ts:
    print("%s  %-22s connected=%s  %s" % (t["handle"], t["title"], t.get("connected"), t.get("worktreePath")))
'
}

# タスクの端末と worktree を、ADR-0081 の契約に照らして判定する。
cmd_inspect() {
  local id="${1:?task id}" handle wt
  IFS=$'\t' read -r handle wt < <(task_session "$id")
  echo "== task $id  handle=$handle"
  echo "   worktree=$wt"
  if term="$(orca_json terminal show --terminal "$handle")"; then
    TERM_JSON="$term" python3 - "$wt" <<'EOF' || FAILED=1
import json, os, sys
wt = sys.argv[1]
t = json.loads(os.environ["TERM_JSON"])["terminal"]
ok = True
def same(a, b):
    return a == b or (os.path.exists(a) and os.path.exists(b) and os.path.realpath(a) == os.path.realpath(b))
title = t.get("title") or ""
print("  %s  タブタイトル = %r（totsuka で始まり、Claude の OSC に上書きされていない）" % ("PASS" if title.startswith("totsuka ") else "FAIL", title)); ok &= title.startswith("totsuka ")
w = t.get("worktreePath") or ""
print("  %s  端末は totsuka の worktree にある（%s）" % ("PASS" if same(w, wt) else "FAIL", w)); ok &= same(w, wt)
print("  ----  connected = %s（稼働中なら true。完了後も対話型エージェントは終了しない）" % t.get("connected"))
sys.exit(0 if ok else 1)
EOF
  else
    fail "terminal show が答えない（タブが閉じられた？ cleanup 済みなら正常）"
  fi
  if [ -n "$wt" ] && wtj="$(orca_json worktree show --worktree "path:$wt")"; then
    python3 -c '
import json, sys
w = json.load(sys.stdin)["worktree"]
n = w.get("displayName") or ""
print("  %s  サイドバーの表示名 = %r（{repo}: {title}）" % ("PASS" if ": " in n else "FAIL", n))
' <<<"$wtj"
  else
    note "worktree show が答えない（worktree 掃除後なら正常）"
  fi
  # orca が 2 本目の worktree を作っていないこと（旧実装の worktree create の再発検知）。
  local extra
  extra="$("$ORCA_BIN" worktree list --repo "path:$REPO_WEB" --json 2>/dev/null | python3 -c '
import json, sys
ws = json.load(sys.stdin).get("result", {}).get("worktrees", [])
print(sum(1 for w in ws if "totsuka-" in (w.get("path") or "").rsplit("/", 1)[-1] and (w.get("creatorProvenance") or {}).get("kind")))
' 2>/dev/null || echo 0)"
  [ "${extra:-0}" = 0 ] && pass "orca 自身が作った totsuka-* worktree は無い" || fail "orca が作った worktree が $extra 本ある（worktree create の再発？）"
  [ "$FAILED" = 0 ] || exit 1
}

cmd_snapshot() {
  local id="${1:?task id}" handle wt
  IFS=$'\t' read -r handle wt < <(task_session "$id")
  orca_json terminal read --terminal "$handle" --screen | python3 -c '
import json, sys
t = json.load(sys.stdin)["terminal"]
print("\n".join(t.get("tail") or []) or "(source=%s: 画面を描画できない)" % t.get("source"))
'
}

# エージェントを /exit で終わらせる。exec 起動なので端末ごと終了し、deadman が
# failed を送るはず。**完了済みのタスクに使っても何も起きない**（Orchestrator が無視する）。
cmd_exit_agent() {
  local id="${1:?task id}" handle wt
  IFS=$'\t' read -r handle wt < <(task_session "$id")
  orca_json terminal send --terminal "$handle" --text "/exit" --enter >/dev/null
  echo "==> /exit を送った。tt task show $id で failed への遷移を確認する"
}

cmd_cleanup_hints() {
  echo "-- totsuka の orca 端末（閉じるなら: orca terminal close --terminal <handle> --tab）"
  cmd_sessions
  echo "-- E2E の worktree（\$E2E_HOME/wt。消すのは Orchestrator の cleanup か tt doctor に任せる）"
  find "$E2E_HOME/wt" -mindepth 2 -maxdepth 2 -type d 2>/dev/null || true
}

case "${1:-}" in
preflight) cmd_preflight ;;
use)
  shift
  cmd_use "$@"
  ;;
sessions) cmd_sessions ;;
inspect)
  shift
  cmd_inspect "$@"
  ;;
snapshot)
  shift
  cmd_snapshot "$@"
  ;;
exit-agent)
  shift
  cmd_exit_agent "$@"
  ;;
cleanup-hints) cmd_cleanup_hints ;;
*)
  sed -n '2,14p' "$0"
  exit 2
  ;;
esac
