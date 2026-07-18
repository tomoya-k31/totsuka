#!/usr/bin/env bash
# on-user-prompt-submit.sh — Claude Code UserPromptSubmit hook.
#
# Injects the orchestrator-supplied prompt context (TOTSUKA_PROMPT_CONTEXT:
# the task-source's instructions + the marker self-report convention) as
# `additionalContext` — the model sees it in full, the pane renders nothing.
# Fail-open (D-09): without the env var, or without `jq`, it exits 0 with no
# output — the prompt still submits, only without the injected context for
# that turn (the instructions are lost then; accepted trade-off, the on-stop
# safety net still covers completion). stdout carries EXACTLY one JSON line
# or nothing (H-13) — anything else would corrupt the hook protocol.
set -uo pipefail

# The hook input JSON on stdin is not needed (the context rides the env), but
# drain it so the writing side never sees a broken pipe.
cat >/dev/null

if [ -z "${TOTSUKA_PROMPT_CONTEXT:-}" ]; then
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

jq -cn --arg ctx "$TOTSUKA_PROMPT_CONTEXT" \
  '{hookSpecificOutput: {hookEventName: "UserPromptSubmit", additionalContext: $ctx}}'
exit 0
