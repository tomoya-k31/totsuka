// totsuka-opencode.js — OpenCode completion-detection plugin (#196 Phase 3).
//
// Installed to ~/.config/opencode/plugins/ by `totsuka run` / `totsuka doctor`
// (opencode auto-loads every plugin there). It normalizes OpenCode session
// events onto the same UDS wire contract the Claude/Codex hook scripts use
// (ai-docs/apis/agent-events.md): POST /agent-events with hook_event_name +
// uppercase status parsed from the LAST <<STATUS:...>> marker in the final
// assistant message.
//
// Targets the opencode **v2** plugin API: a default export `{ id, setup }`
// that reads `ctx.event.subscribe()`. v2 refuses the v1 shape (named export
// returning an `event` hook) with "Plugin must export a default definition",
// which is how every opencode task silently stopped reporting completion.
// The event names below were taken from a real v2.0.18 event stream.
//
// Global installation means this runs for the user's personal sessions too —
// TOTSUKA_HOOK_ENDPOINT (set only in orchestrator panes via ToolLaunchSpec
// env) gates everything: without it the plugin subscribes to nothing. The
// plugin runs inside the opencode *server*, so the env only reaches it when
// the pane runs its own server — hence totsuka launches `--standalone`
// (the shared background service was started without it).
//
// Fail-open (D-09): no throw may escape the subscription; a failed POST is
// spooled as one NDJSON line under TOTSUKA_HOOK_SPOOL_DIR (E-07). OpenCode
// cannot block a stop (marker_block = false), so a missing marker posts
// UNKNOWN and escalation is handled by the engine's UNKNOWN streak (D-02).

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

// Text summary of a question form, for the waiting_input notification body
// the operator reads. v2 renders a `question` tool call as a form whose
// fields carry the question text.
function summarizeQuestion(form) {
  try {
    const text = (form?.fields ?? [])
      .map((f) => f?.description || f?.title || "")
      .filter(Boolean)
      .join(" / ")
    if (text) return text.slice(0, 500)
  } catch {}
  return "agent asked a question (question tool)"
}

export default {
  id: "totsuka-opencode",
  setup(ctx) {
    // Personal session (no orchestrator env): subscribe to nothing.
    if (!ENDPOINT || !JOB_ID) return

    // sessionID -> { messageID, texts[] }: the text parts of the latest
    // assistant message, filled from `session.text.ended` (one per ordinal).
    const lastText = new Map()
    const started = new Set()

    async function onTurnEnd(sessionID) {
      const last = lastText.get(sessionID)
      const text = last ? last.texts.join("") : ""
      const marker = parseMarker(text)
      await postEvent({
        job_id: JOB_ID,
        session_id: sessionID,
        // Idempotency-key element: the assistant message id; a per-occurrence
        // fallback keeps distinct text-less stops from collapsing into one row.
        prompt_id: last?.messageID ?? `stop-${Date.now()}`,
        hook_event_name: "Stop",
        ts: isoNow(),
        // Uppercase mirrors on-stop.sh (the receiver compares
        // case-insensitively either way).
        status: marker ? marker.status.toUpperCase() : "UNKNOWN",
        reason: marker ? marker.reason : "",
        last_assistant_message: text,
        background_tasks: [],
      })
    }

    async function onEvent(event) {
      const t = event?.type ?? ""
      const data = event?.data ?? {}
      const sessionID = data.sessionID ?? ""
      if (t === "session.execution.started") {
        // A turn that ends without text must not report the previous turn's
        // message (its marker, and its prompt_id as a duplicate).
        lastText.delete(sessionID)
        // v2 has no session-created event on the stream; the first execution
        // of a session stands in for it.
        if (sessionID && !started.has(sessionID)) {
          started.add(sessionID)
          await postEvent({
            job_id: JOB_ID,
            session_id: sessionID,
            hook_event_name: "SessionStart",
            ts: isoNow(),
            source: "startup",
          })
        }
      } else if (t === "session.text.ended") {
        let cur = lastText.get(sessionID)
        if (!cur || cur.messageID !== data.assistantMessageID) {
          cur = { messageID: data.assistantMessageID, texts: [] }
          lastText.set(sessionID, cur)
        }
        cur.texts[data.ordinal ?? cur.texts.length] = data.text ?? ""
      } else if (t === "session.execution.succeeded") {
        await onTurnEnd(sessionID)
      } else if (t === "session.execution.interrupted") {
        // "shutdown" is opencode exiting (the pane closing), not a turn the
        // agent ended; anything else (an operator abort) is judged like a stop.
        if (data.reason !== "shutdown") await onTurnEnd(sessionID)
      } else if (t === "session.execution.failed") {
        await postEvent({
          job_id: JOB_ID,
          session_id: sessionID,
          // No message context here; a per-occurrence id keeps repeated
          // errors from collapsing into one row via the idempotency key.
          prompt_id: `error-${Date.now()}`,
          hook_event_name: "Stop",
          ts: isoNow(),
          status: "FAILED",
          reason: String(data.error?.message ?? data.error?.type ?? data.error ?? t),
          last_assistant_message: "",
          background_tasks: [],
        })
      } else if (t === "form.created") {
        // The `question` tool blocks the turn on the human (#487), and v2
        // surfaces it as a form. Post QuestionPending so the engine parks the
        // task (waiting_input, slot kept, operator notified), exactly like
        // claude's AskUserQuestion PreToolUse relay. No turn-end event arrives
        // while the form is open, so nothing needs suppressing meanwhile.
        const form = data.form ?? {}
        if (form.metadata?.kind !== "question") return
        await postEvent({
          job_id: JOB_ID,
          session_id: form.sessionID ?? "",
          // Idempotency key: the form id is distinct per question, so a second
          // question is not dropped as a duplicate of the first.
          prompt_id: form.id ?? `q-${Date.now()}`,
          hook_event_name: "QuestionPending",
          ts: isoNow(),
          message: summarizeQuestion(form),
        })
      }
    }

    const controller = new AbortController()
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          try {
            await onEvent(event)
          } catch {}
        }
      } catch {}
    })()
    return () => controller.abort()
  },
}
