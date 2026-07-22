#!/usr/bin/env bash
# hook-common.sh — shared helpers for the totsuka Claude Code hooks (#131/#137).
#
# Sourced by on-stop.sh / on-notification.sh / on-session-start.sh /
# on-session-end.sh. Never executed directly.
#
# Design constraints (D-09 fail-open / H-10..H-14):
#   - Callers use `set -uo pipefail` and NEVER `-e`: any internal failure must
#     fall through to `exit 0` so a broken hook can never wedge the agent.
#   - Job-specific values arrive via env, never baked into the file, so the same
#     rendered `--settings` path is reusable across `--resume` (H-03):
#       TOTSUKA_JOB_ID / TOTSUKA_HOOK_ENDPOINT (UDS path) /
#       TOTSUKA_HOOK_TOKEN / TOTSUKA_HOOK_SPOOL_DIR.
#   - stdout is reserved for the Stop-hook block JSON only (H-13); everything
#     diagnostic goes to stderr.

# True when jq or curl is unavailable — callers spool the raw input and exit 0
# rather than attempting a structured POST (H-14).
tools_missing() {
  ! command -v jq >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1
}

# Current UTC timestamp (RFC3339, second precision).
iso_now() {
  date -u +%Y-%m-%dT%H:%M:%SZ
}

# Append one NDJSON line to the spool (E-07/H-11). Best effort: a missing spool
# dir or a write error is swallowed so the hook still exits 0.
spool_line() {
  line="$1"
  dir="${TOTSUKA_HOOK_SPOOL_DIR:-}"
  [ -n "$dir" ] || return 0
  mkdir -p "$dir" 2>/dev/null || return 0
  file="$dir/$(date +%s)-$$.jsonl"
  printf '%s\n' "$line" >>"$file" 2>/dev/null || return 0
}

# POST one JSON payload to the orchestrator over the UDS (Bearer auth,
# --max-time 5, 2 retries). On any failure, spool the payload for later
# recovery. Never returns non-zero (fail-open).
post_event() {
  payload="$1"
  endpoint="${TOTSUKA_HOOK_ENDPOINT:-}"
  token="${TOTSUKA_HOOK_TOKEN:-}"
  if [ -n "$endpoint" ] && command -v curl >/dev/null 2>&1; then
    if curl --unix-socket "$endpoint" \
      --max-time 5 --retry 2 --silent --show-error \
      -o /dev/null \
      -X POST \
      -H "Authorization: Bearer $token" \
      -H "Content-Type: application/json" \
      --data-binary "$payload" \
      "http://localhost/claude-events" 2>/dev/null; then
      return 0
    fi
  fi
  spool_line "$payload"
  return 0
}
