//! The Discord Gateway: the only transport that carries `MESSAGE_CREATE`.
//!
//! Discord has no Socket-Mode-style alternative and no HTTP push for message
//! events — its webhook-events mechanism covers app-lifecycle events only —
//! so a live WebSocket is mandatory, and everything about staying connected
//! is this module's problem.
//!
//! # The two things that make this different from a plain reconnect loop
//!
//! **RESUME.** After a drop, replaying `session_id` + the last sequence
//! number gets every missed event back, in order. That window is the cheap
//! path and is tried first; only when it fails does the expensive one (a
//! fresh `IDENTIFY` plus a REST backfill) run.
//!
//! **Permanent close codes.** Discord answers a disallowed intent by closing
//! the socket, not by failing the handshake. Reconnecting on 4014 would loop
//! forever against an un-ticked checkbox and read, in the logs, exactly like
//! a flaky network — so [`close_code_is_permanent`] stops the loop instead.

use std::time::Duration;

use serde_json::{Value, json};

/// Gateway intents this plugin identifies with.
///
/// `GUILD_MESSAGES` (1 << 9) delivers `MESSAGE_CREATE` in server channels;
/// `MESSAGE_CONTENT` (1 << 15) is what makes `content` non-empty, and is
/// **privileged** — below 10,000 users it is a toggle in the Developer
/// Portal, above that it needs review. Nothing else is requested: the watch
/// reads channel messages and nothing else, and asking for more would widen
/// what this process sees for no feature.
pub const INTENTS: u64 = (1 << 9) | (1 << 15);

/// Gateway opcodes, named so the match below reads as the protocol does.
pub mod op {
    /// An event dispatch (`d` carries the payload, `t` the event name).
    pub const DISPATCH: u64 = 0;
    /// Keep-alive.
    pub const HEARTBEAT: u64 = 1;
    /// Our opening handshake.
    pub const IDENTIFY: u64 = 2;
    /// Replay a dropped session.
    pub const RESUME: u64 = 6;
    /// Discord asking us to reconnect (resumable).
    pub const RECONNECT: u64 = 7;
    /// Our session is gone; `d` says whether a resume may be retried.
    pub const INVALID_SESSION: u64 = 9;
    /// The opening frame, carrying `heartbeat_interval`.
    pub const HELLO: u64 = 10;
    /// Acknowledgement of our heartbeat.
    pub const HEARTBEAT_ACK: u64 = 11;
}

/// What a session needs to resume: handed out by `READY`, spent by `RESUME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeState {
    /// The URL to resume against — **not** the one used to connect first.
    pub resume_gateway_url: String,
    /// The session to replay.
    pub session_id: String,
    /// The last sequence number seen.
    pub seq: u64,
}

/// The `IDENTIFY` payload.
pub fn identify(token: &str) -> Value {
    json!({
        "op": op::IDENTIFY,
        "d": {
            "token": token,
            "intents": INTENTS,
            "properties": { "os": "linux", "browser": "totsuka", "device": "totsuka" },
        }
    })
}

/// The `RESUME` payload.
pub fn resume(token: &str, state: &ResumeState) -> Value {
    json!({
        "op": op::RESUME,
        "d": { "token": token, "session_id": state.session_id, "seq": state.seq }
    })
}

/// The heartbeat payload for the last sequence number seen (`null` before
/// the first dispatch).
pub fn heartbeat(seq: Option<u64>) -> Value {
    json!({ "op": op::HEARTBEAT, "d": seq })
}

/// Whether a close code means reconnecting can never help.
///
/// The listed codes are configuration or version problems: retrying them
/// burns the socket forever and hides the real cause. Everything else —
/// including no close code at all — is treated as recoverable, which is the
/// safe default for a transport that drops for ordinary reasons.
pub fn close_code_is_permanent(code: u16) -> bool {
    matches!(code, 4004 | 4010 | 4011 | 4012 | 4013 | 4014)
}

/// Whether an `INVALID_SESSION` frame permits another resume attempt.
///
/// `d` is a bare boolean; anything else means "do not resume", which is the
/// conservative reading — a wrong `true` would spend an attempt replaying a
/// session Discord has already discarded.
pub fn invalid_session_is_resumable(frame: &Value) -> bool {
    frame.get("d").and_then(Value::as_bool).unwrap_or(false)
}

/// How the plugin should proceed after one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do (a heartbeat ack, an event we do not read).
    Idle,
    /// Start heartbeating at this interval and send the opening handshake.
    Hello {
        /// Discord's requested interval.
        interval: Duration,
    },
    /// The session is live; remember how to resume it.
    Ready(ResumeState),
    /// A resumed session caught up.
    Resumed,
    /// A message was posted. The payload is the raw `d` object.
    Message(Value),
    /// Reconnect, resuming if we still hold a session.
    Reconnect {
        /// Whether a resume may be attempted.
        resumable: bool,
    },
}

/// Classify one Gateway frame, updating `seq` as dispatches arrive.
///
/// The sequence number is tracked here rather than by the caller because it
/// must advance on **every** dispatch, not just the ones this plugin acts on
/// — a resume replaying from a stale `seq` would re-deliver everything since.
pub fn step(frame: &Value, seq: &mut Option<u64>) -> Step {
    if let Some(n) = frame.get("s").and_then(Value::as_u64) {
        *seq = Some(n);
    }
    match frame.get("op").and_then(Value::as_u64) {
        Some(op::HELLO) => {
            let millis = frame
                .get("d")
                .and_then(|d| d.get("heartbeat_interval"))
                .and_then(Value::as_u64)
                .unwrap_or(41_250);
            Step::Hello {
                interval: Duration::from_millis(millis),
            }
        }
        Some(op::DISPATCH) => match frame.get("t").and_then(Value::as_str) {
            Some("READY") => {
                let d = frame.get("d");
                let url = d
                    .and_then(|d| d.get("resume_gateway_url"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let session_id = d
                    .and_then(|d| d.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Step::Ready(ResumeState {
                    resume_gateway_url: url,
                    session_id,
                    seq: seq.unwrap_or(0),
                })
            }
            Some("RESUMED") => Step::Resumed,
            Some("MESSAGE_CREATE") => match frame.get("d") {
                Some(d) => Step::Message(d.clone()),
                None => Step::Idle,
            },
            _ => Step::Idle,
        },
        // Discord asking politely: our session survives, so resume.
        Some(op::RECONNECT) => Step::Reconnect { resumable: true },
        Some(op::INVALID_SESSION) => Step::Reconnect {
            resumable: invalid_session_is_resumable(frame),
        },
        // A heartbeat request from Discord's side, and its ack, need no
        // decision from the caller — the heartbeat task answers both.
        Some(op::HEARTBEAT) | Some(op::HEARTBEAT_ACK) => Step::Idle,
        _ => Step::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_intent_bitfield_is_exactly_the_two_we_need() {
        assert_eq!(INTENTS, 512 | 32_768);
        // Naming them here so a widened bitfield has to be a deliberate edit:
        // every extra intent widens what this process receives.
        assert_eq!(INTENTS, 33_280);
    }

    #[test]
    fn identify_carries_the_token_and_the_intents() {
        let payload = identify("tok");
        assert_eq!(payload["op"], op::IDENTIFY);
        assert_eq!(payload["d"]["token"], "tok");
        assert_eq!(payload["d"]["intents"], INTENTS);
    }

    #[test]
    fn resume_replays_the_session_from_the_last_sequence() {
        let state = ResumeState {
            resume_gateway_url: "wss://resume.test".into(),
            session_id: "S1".into(),
            seq: 42,
        };
        let payload = resume("tok", &state);
        assert_eq!(payload["op"], op::RESUME);
        assert_eq!(payload["d"]["session_id"], "S1");
        assert_eq!(payload["d"]["seq"], 42);
    }

    #[test]
    fn heartbeat_sends_null_before_the_first_dispatch() {
        assert_eq!(heartbeat(None)["d"], Value::Null);
        assert_eq!(heartbeat(Some(7))["d"], 7);
    }

    /// The codes that must stop the loop, and the ones that must not. Getting
    /// this backwards either loops forever on a checkbox or gives up on a
    /// dropped socket.
    #[test]
    fn only_the_unfixable_close_codes_are_permanent() {
        for code in [4004, 4010, 4011, 4012, 4013, 4014] {
            assert!(close_code_is_permanent(code), "{code} must stop the loop");
        }
        for code in [1000, 1001, 1006, 4000, 4001, 4007, 4008, 4009] {
            assert!(!close_code_is_permanent(code), "{code} must reconnect");
        }
    }

    #[test]
    fn an_invalid_session_only_resumes_when_discord_says_so() {
        assert!(invalid_session_is_resumable(&json!({ "op": 9, "d": true })));
        assert!(!invalid_session_is_resumable(
            &json!({ "op": 9, "d": false })
        ));
        // A shape we did not expect must not be read as permission.
        assert!(!invalid_session_is_resumable(&json!({ "op": 9 })));
        assert!(!invalid_session_is_resumable(
            &json!({ "op": 9, "d": "yes" })
        ));
    }

    #[test]
    fn hello_carries_the_interval_and_falls_back_when_it_does_not() {
        let Step::Hello { interval } = step(
            &json!({ "op": 10, "d": { "heartbeat_interval": 1000 } }),
            &mut None,
        ) else {
            panic!("expected Hello");
        };
        assert_eq!(interval, Duration::from_millis(1000));

        let Step::Hello { interval } = step(&json!({ "op": 10, "d": {} }), &mut None) else {
            panic!("expected Hello");
        };
        assert!(interval > Duration::ZERO, "never a zero-interval spin");
    }

    #[test]
    fn ready_captures_the_resume_url_which_is_not_the_connect_url() {
        let mut seq = None;
        let frame = json!({
            "op": 0, "s": 1, "t": "READY",
            "d": { "resume_gateway_url": "wss://resume.test", "session_id": "S1" }
        });
        let Step::Ready(state) = step(&frame, &mut seq) else {
            panic!("expected Ready");
        };
        assert_eq!(state.resume_gateway_url, "wss://resume.test");
        assert_eq!(state.session_id, "S1");
        assert_eq!(state.seq, 1);
    }

    /// The sequence number must advance on **every** dispatch, including the
    /// ones this plugin ignores — otherwise a resume replays from too far
    /// back and re-delivers everything since.
    #[test]
    fn the_sequence_advances_on_events_we_do_not_read() {
        let mut seq = None;
        assert_eq!(
            step(&json!({ "op": 0, "s": 5, "t": "TYPING_START" }), &mut seq),
            Step::Idle
        );
        assert_eq!(seq, Some(5));
        assert_eq!(
            step(
                &json!({ "op": 0, "s": 6, "t": "PRESENCE_UPDATE" }),
                &mut seq
            ),
            Step::Idle
        );
        assert_eq!(seq, Some(6));
    }

    #[test]
    fn a_message_create_hands_over_its_payload() {
        let mut seq = None;
        let frame = json!({
            "op": 0, "s": 2, "t": "MESSAGE_CREATE",
            "d": { "id": "M1", "channel_id": "C1", "content": "hi", "type": 0 }
        });
        let Step::Message(payload) = step(&frame, &mut seq) else {
            panic!("expected Message");
        };
        assert_eq!(payload["id"], "M1");
        assert_eq!(seq, Some(2));
    }

    #[test]
    fn reconnect_and_invalid_session_map_to_the_right_resumability() {
        let mut seq = None;
        assert_eq!(
            step(&json!({ "op": 7 }), &mut seq),
            Step::Reconnect { resumable: true }
        );
        assert_eq!(
            step(&json!({ "op": 9, "d": true }), &mut seq),
            Step::Reconnect { resumable: true }
        );
        assert_eq!(
            step(&json!({ "op": 9, "d": false }), &mut seq),
            Step::Reconnect { resumable: false }
        );
    }
}
