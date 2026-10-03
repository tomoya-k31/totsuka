#!/usr/bin/env bash
# on-post-tool-use.sh — Claude Code PostToolUse hook.
#
# A tool finished, so whatever permission prompt preceded it has been answered:
# the orchestrator clears the task's "awaiting approval" mark on any signal
# other than a Notification, and nothing else arrives between an approval and
# the end of the turn. Sent as a bare liveness signal (an unknown event name
# normalizes to a heartbeat, which also refreshes the R-10 timeout anchor);
# repeats share one idempotency key and are dropped after that. Fail-open
# (no -e); stdout empty — a PostToolUse hook's output is fed back to the model.
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hook-common.sh
. "$DIR/hook-common.sh"

input="$(cat)"

[ -n "${TOTSUKA_JOB_ID:-}" ] || exit 0

if tools_missing; then
  spool_line "$input"
  exit 0
fi

session_id="$(printf '%s' "$input" | jq -r '.session_id // ""')"

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg ts "$(iso_now)" \
  '{job_id: $job_id, session_id: $session_id, hook_event_name: "PostToolUse", ts: $ts}')"

post_event "$payload"
exit 0
