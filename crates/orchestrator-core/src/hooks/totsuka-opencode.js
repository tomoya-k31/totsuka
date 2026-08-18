// totsuka-opencode.js — OpenCode completion-detection plugin (#196 Phase 3).
//
// Installed to ~/.config/opencode/plugins/ by `totsuka run` / `totsuka doctor`
// (opencode auto-loads every plugin there). It normalizes OpenCode session
// events onto the same UDS wire contract the Claude/Codex hook scripts use
// (ai-docs/apis/agent-events.md): POST /agent-events with hook_event_name +
// uppercase status parsed from the LAST <<STATUS:...>> marker in the final
// assistant message.
//
// Global installation means this runs for the user's personal sessions too —
// TOTSUKA_HOOK_ENDPOINT (set only in orchestrator panes via ToolLaunchSpec
// env) gates everything: without it the plugin registers no hooks at all.
//
// Fail-open (D-09): no throw may escape a hook; a failed POST is spooled as
// one NDJSON line under TOTSUKA_HOOK_SPOOL_DIR (E-07), tool errors are
// swallowed. OpenCode cannot block a stop (marker_block = false), so a
// missing marker posts UNKNOWN and escalation is handled by the engine's
// UNKNOWN streak (D-02).

import { appendFileSync, mkdirSync } from "node:fs"
import { join } from "node:path"

const ENDPOINT = process.env.TOTSUKA_HOOK_ENDPOINT ?? ""
const JOB_ID = process.env.TOTSUKA_JOB_ID ?? ""
const TOKEN = process.env.TOTSUKA_HOOK_TOKEN ?? ""
const SPOOL_DIR = process.env.TOTSUKA_HOOK_SPOOL_DIR ?? ""

function isoNow() {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z")
}

function spool(payload) {
  if (!SPOOL_DIR) return
  try {
    mkdirSync(SPOOL_DIR, { recursive: true })
    const file = join(SPOOL_DIR, `${Math.floor(Date.now() / 1000)}-${process.pid}.jsonl`)
    appendFileSync(file, JSON.stringify(payload) + "\n")
  } catch {}
}

// POST one payload over the UDS (Bun fetch supports `unix`); spool on any
// failure. Never throws.
async function postEvent(payload) {
  try {
    const res = await fetch("http://localhost/agent-events", {
      unix: ENDPOINT,
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
      },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(5000),
    })
    if (!res.ok) spool(payload)
  } catch {
    spool(payload)
  }
}

// Extract the LAST status marker, tolerating single/double angle brackets on
// either side (mirrors on-stop.sh / strip_status_markers, #152).
function parseMarker(text) {
  const matches = [...(text ?? "").matchAll(/<{1,2}STATUS:([^>]*)>{1,2}/g)]
  if (matches.length === 0) return null
  const inner = matches[matches.length - 1][1]
  const status = inner.split(/\s+/, 1)[0]
  const reasonMatch = inner.match(/reason="([^"]*)"/)
  return { status, reason: reasonMatch ? reasonMatch[1] : "" }
}

// Best-effort text summary of a `question` tool call's args, for the
// waiting_input notification body the operator reads. The args shape is
// unverified on real machines (#487 live-verify item), so probe the likely
// spots and fall back to a fixed label.
function summarizeQuestion(args) {
  try {
    const qs = args?.questions ?? args?.question ?? args
    if (typeof qs === "string") return qs.slice(0, 500)
    if (Array.isArray(qs)) {
      const text = qs
        .map((q) => (typeof q === "string" ? q : (q?.question ?? q?.text ?? "")))
        .filter(Boolean)
        .join(" / ")
      if (text) return text.slice(0, 500)
    }
    if (typeof qs?.question === "string") return qs.question.slice(0, 500)
    if (typeof qs?.text === "string") return qs.text.slice(0, 500)
  } catch {}
  return "agent asked a question (question tool)"
}

export const TotsukaOpencode = async ({ client }) => {
  // Personal session (no orchestrator env): register nothing.
  if (!ENDPOINT || !JOB_ID) return {}

  // Sessions with a `question` dialog currently open (#487). While a question
  // is pending the turn has not ended, so an idle event arriving then must
  // NOT be judged for a marker — that would post a spurious UNKNOWN and feed
  // the D-02 escalation streak.
  const pendingQuestions = new Set()

  // The stop decision needs the final assistant message; session.status(idle)
  // and the deprecated session.idle both signal turn end (they may both fire —
  // the receiver's idempotency key makes the duplicate harmless).
  async function onIdle(sessionID) {
    if (pendingQuestions.has(sessionID)) return
    let last = null
    let lastAny = null
    try {
      const res = await client.session.messages({ path: { id: sessionID } })
      const data = res?.data ?? res
      if (Array.isArray(data)) {
        last = [...data].reverse().find((m) => (m.info?.role ?? m.role) === "assistant") ?? null
        lastAny = data.length > 0 ? data[data.length - 1] : null
      }
    } catch {}
    const text = last
      ? (last.parts ?? [])
          .filter((p) => p.type === "text")
          .map((p) => p.text)
          .join("")
      : ""
    // Idempotency-key element: prefer the assistant message id; fall back to
    // any message id so distinct stops rarely share an empty prompt_id (an
    // empty one would collapse same-status stops into one DB row).
    const promptId =
      last?.info?.id ?? last?.id ?? lastAny?.info?.id ?? lastAny?.id ?? ""
    const marker = parseMarker(text)
    await postEvent({
      job_id: JOB_ID,
      session_id: sessionID,
      prompt_id: promptId,
      hook_event_name: "Stop",
      ts: isoNow(),
      // Uppercase mirrors on-stop.sh (the receiver compares case-insensitively
      // either way).
      status: marker ? marker.status.toUpperCase() : "UNKNOWN",
      reason: marker ? marker.reason : "",
      last_assistant_message: text,
      background_tasks: [],
    })
  }

  return {
    // The `question` tool blocks the turn on the human (#487): no idle — and
    // so no marker — can arrive while it waits. Post QuestionPending so the
    // engine parks the task (waiting_input, slot released, operator notified),
    // exactly like claude's AskUserQuestion PreToolUse relay. `callID` is the
    // per-question idempotency key: a second question must not be dropped as
    // a duplicate of the first.
    "tool.execute.before": async (input, output) => {
      try {
        if (input?.tool !== "question") return
        const sessionID = input.sessionID ?? ""
        if (sessionID) pendingQuestions.add(sessionID)
        await postEvent({
          job_id: JOB_ID,
          session_id: sessionID,
          prompt_id: input.callID ?? `q-${sessionID}`,
          hook_event_name: "QuestionPending",
          ts: isoNow(),
          message: summarizeQuestion(output?.args),
        })
      } catch {}
    },
    "tool.execute.after": async (input) => {
      try {
        if (input?.tool === "question") pendingQuestions.delete(input.sessionID)
      } catch {}
    },
    event: async ({ event }) => {
      try {
        const t = event?.type ?? ""
        const props = event?.properties ?? {}
        if (t === "session.created") {
          const sessionID = props.info?.id ?? props.sessionID ?? ""
          if (sessionID) {
            await postEvent({
              job_id: JOB_ID,
              session_id: sessionID,
              hook_event_name: "SessionStart",
              ts: isoNow(),
              source: "startup",
            })
          }
        } else if (t === "session.status") {
          if (props.status?.type === "idle" && props.sessionID) {
            await onIdle(props.sessionID)
          }
        } else if (t === "session.idle") {
          if (props.sessionID) await onIdle(props.sessionID)
        } else if (t === "session.error") {
          const sessionID = props.sessionID ?? props.info?.id ?? ""
          // Fail-open: an errored session must not stay marked as
          // question-pending, or its later idles would be suppressed forever.
          pendingQuestions.delete(sessionID)
          await postEvent({
            job_id: JOB_ID,
            session_id: sessionID,
            // No message context here; a per-occurrence id keeps repeated
            // errors from collapsing into one row via the idempotency key.
            prompt_id: `error-${Date.now()}`,
            hook_event_name: "Stop",
            ts: isoNow(),
            status: "FAILED",
            reason: String(props.error?.name ?? props.error ?? "session.error"),
            last_assistant_message: "",
            background_tasks: [],
          })
        }
      } catch {}
    },
  }
}
