#!/usr/bin/env bash
# on-session-start.sh — SessionStart hook for Claude Code and Codex
# (#131/#137/#196, E-09).
#
# Establishes the job_id -> native tool session_id correlation, which is what
# lets resume reattach to the right task. Fail-open (no -e); stdout empty.
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
source="$(printf '%s' "$input" | jq -r '.source // ""')"

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg ts "$(iso_now)" \
  --arg source "$source" \
  '{job_id: $job_id, session_id: $session_id, hook_event_name: "SessionStart", ts: $ts, source: $source}')"

post_event "$payload"
exit 0
