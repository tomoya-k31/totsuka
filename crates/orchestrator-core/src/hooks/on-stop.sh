#!/usr/bin/env bash
# on-stop.sh — Stop hook for Claude Code and Codex (#131/#137/#196,
# D-09/D-12/R-02/R-03).
#
# Decides task completion deterministically from the status marker in the final
# assistant message, POSTs the result to the orchestrator, and — when the marker
# is missing — blocks (once) to make the agent re-emit it. Both tools deliver
# the same Stop stdin shape; Codex names the turn key `turn_id` where Claude
# uses `prompt_id`, and has no `background_tasks` (no heartbeat).
#
# set -uo pipefail but NOT -e: every branch must fall through to exit 0 so a
# hook failure can never wedge the agent (fail-open, D-09/H-10).
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hook-common.sh
. "$DIR/hook-common.sh"

input="$(cat)"

# Codex hooks are registered globally (~/.codex/hooks.json) and therefore fire
# for the user's personal sessions too. Only orchestrator panes carry
# TOTSUKA_JOB_ID — without it, do nothing (never block a personal session).
[ -n "${TOTSUKA_JOB_ID:-}" ] || exit 0

# jq/curl missing -> spool the raw input and bail (H-14).
if tools_missing; then
  spool_line "$input"
  exit 0
fi

session_id="$(printf '%s' "$input" | jq -r '.session_id // ""')"
transcript_path="$(printf '%s' "$input" | jq -r '.transcript_path // ""')"
prompt_id="$(printf '%s' "$input" | jq -r '.prompt_id // .turn_id // ""')"
stop_hook_active="$(printf '%s' "$input" | jq -r '.stop_hook_active // false')"
last_msg="$(printf '%s' "$input" | jq -r '.last_assistant_message // ""')"
bg_json="$(printf '%s' "$input" | jq -c '(.background_tasks // [])')"
bg_count="$(printf '%s' "$bg_json" | jq -r 'length')"
ts="$(iso_now)"

# Build a Stop-family payload matching the canonical wire contract
# (docs/apis/agent-events.md): the event kind is carried by `hook_event_name`
# ("Stop"), and a non-empty `background_tasks` array is what the receiver reads
# to treat an intermediate Stop as a heartbeat (D-12). $1 = status, $2 = reason.
stop_payload() {
  jq -cn \
    --arg job_id "${TOTSUKA_JOB_ID:-}" \
    --arg session_id "$session_id" \
    --arg prompt_id "$prompt_id" \
    --arg ts "$ts" \
    --arg status "$1" \
    --arg reason "$2" \
    --arg last "$last_msg" \
    --arg transcript "$transcript_path" \
    --argjson background "$bg_json" \
    '{
      job_id: $job_id,
      session_id: $session_id,
      prompt_id: $prompt_id,
      hook_event_name: "Stop",
      ts: $ts,
      status: $status,
      reason: $reason,
      last_assistant_message: $last,
      transcript_path: $transcript,
      background_tasks: $background
    }'
}

# Non-empty background_tasks -> intermediate Stop: heartbeat only, no completion
# judgement, no block (R-02). The receiver derives Heartbeat from the non-empty
# background_tasks array carried in the payload.
if [ "${bg_count:-0}" -gt 0 ]; then
  post_event "$(stop_payload "" "")"
  exit 0
fi

# Extract the LAST STATUS marker in the final message (D-12). The canonical form
# is `<<STATUS:...>>`, but real agents routinely normalise the doubled angle
# brackets to a single pair (`<STATUS:...>`); accept 1-or-2 brackets on each side
# so a well-formed completion is never missed over a bracket count.
marker="$(printf '%s\n' "$last_msg" | grep -oE '<{1,2}STATUS:[^>]*>{1,2}' | tail -n 1)"

if [ -n "$marker" ]; then
  # Strip up to two leading '<', the STATUS: prefix, and up to two trailing '>',
  # then split "KEYWORD [reason=\"...\"]".
  inner="${marker#<}"
  inner="${inner#<}"
  inner="${inner#STATUS:}"
  inner="${inner%>}"
  inner="${inner%>}"
  status="${inner%% *}"
  reason=""
  case "$inner" in
  *'reason="'*)
    reason="${inner#*reason=\"}"
    reason="${reason%%\"*}"
    ;;
  esac
  post_event "$(stop_payload "$status" "$reason")"
  exit 0
fi

# Marker absent -> always report UNKNOWN so core can count consecutive blanks
# for escalation (D-02/D-03); core never trusts the hook's own count.
post_event "$(stop_payload UNKNOWN "")"

# First blank Stop (stop_hook_active != true) -> block once with the fix (R-03).
# A re-entrant blank Stop (== true) sends UNKNOWN only, never re-blocks (R-02).
if [ "$stop_hook_active" != "true" ]; then
  printf '%s\n' '{"decision":"block","reason":"応答の最終行に <<STATUS:COMPLETED>> / <<STATUS:NEEDS_INPUT reason=\"...\">> / <<STATUS:FAILED reason=\"...\">> のいずれかを付けてください"}'
fi
exit 0
