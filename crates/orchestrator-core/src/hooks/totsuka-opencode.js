// totsuka-opencode.js — OpenCode completion-detection plugin (#196 Phase 3).
//
// Installed to ~/.config/opencode/plugins/ by `totsuka run` / `totsuka doctor`
// (opencode auto-loads every plugin there). It normalizes OpenCode session
// events onto the same UDS wire contract the Claude/Codex hook scripts use
// (docs/apis/agent-events.md): POST /agent-events with hook_event_name +
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

export const TotsukaOpencode = async ({ client }) => {
  // Personal session (no orchestrator env): register nothing.
  if (!ENDPOINT || !JOB_ID) return {}

  // The stop decision needs the final assistant message; session.status(idle)
  // and the deprecated session.idle both signal turn end (they may both fire —
  // the receiver's idempotency key makes the duplicate harmless).
  async function onIdle(sessionID) {
    let last = null
    try {
      const res = await client.session.messages({ path: { id: sessionID } })
      const data = res?.data ?? res
      if (Array.isArray(data)) {
        last = [...data].reverse().find((m) => (m.info?.role ?? m.role) === "assistant") ?? null
      }
    } catch {}
    const text = last
      ? (last.parts ?? [])
          .filter((p) => p.type === "text")
          .map((p) => p.text)
          .join("")
      : ""
    const promptId = last?.info?.id ?? last?.id ?? ""
    const marker = parseMarker(text)
    await postEvent({
      job_id: JOB_ID,
      session_id: sessionID,
      prompt_id: promptId,
      hook_event_name: "Stop",
      ts: isoNow(),
      status: marker ? marker.status : "UNKNOWN",
      reason: marker ? marker.reason : "",
      last_assistant_message: text,
      background_tasks: [],
    })
  }

  return {
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
          await postEvent({
            job_id: JOB_ID,
            session_id: sessionID,
            prompt_id: "",
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
