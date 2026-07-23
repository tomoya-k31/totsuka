#!/usr/bin/env bash
# on-notification.sh — Claude Code Notification hook, also registered as the
# Codex PermissionRequest hook (#131/#137/#196).
#
# Relays permission prompts / idle / needs-input notifications to the
# orchestrator. Codex has no Notification event; its PermissionRequest (fires
# before an approval prompt) is normalized here to the same wire shape, with a
# message synthesized from the requesting tool. stdout stays EMPTY either way —
# a PermissionRequest hook that printed a decision JSON would auto-approve/deny
# instead of relaying to the human. Fail-open (no -e).
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hook-common.sh
. "$DIR/hook-common.sh"

input="$(cat)"

# Codex registers hooks globally — no TOTSUKA_JOB_ID means a personal session,
# not an orchestrator pane (see on-stop.sh).
[ -n "${TOTSUKA_JOB_ID:-}" ] || exit 0

if tools_missing; then
  spool_line "$input"
  exit 0
fi

session_id="$(printf '%s' "$input" | jq -r '.session_id // ""')"
message="$(printf '%s' "$input" | jq -r 'if .message then .message elif .tool_name then "permission_prompt: \(.tool_name)" else "" end')"

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg ts "$(iso_now)" \
  --arg message "$message" \
  '{job_id: $job_id, session_id: $session_id, hook_event_name: "Notification", ts: $ts, message: $message}')"

post_event "$payload"
exit 0
