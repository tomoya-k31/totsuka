#!/usr/bin/env bash
# on-ask-user-question.sh — Claude Code PreToolUse hook, matcher
# `AskUserQuestion` (#487). Rendered only into the settings of workflows whose
# profile confirms with a human (design / implement).
#
# Fires when the agent opens an AskUserQuestion dialog. The turn does not end
# while the dialog waits, so no Stop — and therefore no NEEDS_INPUT — can reach
# the orchestrator from that path (ADR-0038 D6). This hook is what parks the
# task instead: it POSTs a QuestionPending event and the engine moves the task
# to waiting_input (slot released, operator notified with the question text).
#
# stdout stays EMPTY on every path: a PreToolUse hook's stdout JSON is a
# permission decision, and printing one would allow/deny the tool call instead
# of relaying it to the human. Fail-open (no -e).
set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=hook-common.sh
. "$DIR/hook-common.sh"

input="$(cat)"

# No TOTSUKA_JOB_ID means a personal session, not an orchestrator pane.
[ -n "${TOTSUKA_JOB_ID:-}" ] || exit 0

if tools_missing; then
  spool_line "$input"
  exit 0
fi

session_id="$(printf '%s' "$input" | jq -r '.session_id // ""')"

# Compact question summary — this becomes the waiting_input notification body
# the operator reads, so it carries the question text, truncated.
message="$(printf '%s' "$input" |
  jq -r '[.tool_input.questions[]?.question // empty] | join(" / ")' |
  cut -c1-500)"
[ -n "$message" ] || message="agent asked a question (AskUserQuestion)"

# Idempotency-key component (hook_events dedup): must be DISTINCT per question
# and stable across curl retries / spool re-sends — with an empty prompt_id
# (the on-notification.sh shape) the session's second question would collapse
# into a Duplicate and be dropped. Prefer the per-call tool_use_id when the
# hook input carries one; otherwise hash the tool_input (cksum is POSIX).
prompt_id="$(printf '%s' "$input" | jq -r '.tool_use_id // .prompt_id // ""')"
if [ -z "$prompt_id" ]; then
  prompt_id="q-$(printf '%s' "$input" | jq -cS '.tool_input // {}' | cksum | awk '{print $1}')"
fi

payload="$(jq -cn \
  --arg job_id "${TOTSUKA_JOB_ID:-}" \
  --arg session_id "$session_id" \
  --arg prompt_id "$prompt_id" \
  --arg ts "$(iso_now)" \
  --arg message "$message" \
  '{job_id: $job_id, session_id: $session_id, prompt_id: $prompt_id, hook_event_name: "QuestionPending", ts: $ts, message: $message}')"

post_event "$payload"
exit 0
