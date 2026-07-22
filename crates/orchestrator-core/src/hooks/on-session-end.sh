#!/usr/bin/env bash
# on-session-end.sh — Claude Code SessionEnd hook (#131/#137).
#
# Reports session teardown and its reason (clear / resume / logout /
# prompt_input_exit / bypass_permissions_disabled / other). Fail-open (no -e);
# stdout empty.
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hook-common.sh
. "$DIR/hook-common.sh"

input="$(cat)"

if tools_missing; then
  spool_line "$input"
  exit 0
fi

session_id="$(printf '%s' "$input" | jq -r '.session_id // ""')"
reason="$(printf '%s' "$input" | jq -r '.reason // ""')"

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg ts "$(iso_now)" \
  --arg reason "$reason" \
  '{job_id: $job_id, session_id: $session_id, hook_event_name: "SessionEnd", ts: $ts, reason: $reason}')"

post_event "$payload"
exit 0
