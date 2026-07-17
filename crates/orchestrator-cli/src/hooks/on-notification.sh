#!/usr/bin/env bash
# on-notification.sh — Claude Code Notification hook (#131/#137).
#
# Relays permission prompts / idle / needs-input notifications to the
# orchestrator. Fail-open (no -e); stdout stays empty.
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
message="$(printf '%s' "$input" | jq -r '.message // ""')"

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg ts "$(iso_now)" \
  --arg message "$message" \
  '{job_id: $job_id, session_id: $session_id, hook_event_name: "Notification", ts: $ts, message: $message}')"

post_event "$payload"
exit 0
