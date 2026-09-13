//! Raw Slack delivery → the coordinates that go on a topic.
//!
//! **This is the only place the message body is looked at, and all it does is
//! compare against constant strings.** The body is never stored, never
//! forwarded anywhere but the comparison, and never logged. The record shape
//! has no field that could carry it — but that alone guarantees nothing, since
//! a process can hold a string in memory or print it; the guarantee is this
//! module plus the tests that pin it (ADR-0072 decision 4).
//!
//! # The filter is a gate
//!
//! Decision 4 narrowed publishing to things that could concern the operator.
//! That makes a dropped message **invisible to totsuka forever**, so the two
//! error directions are not symmetric:
//!
//! - publishing something that turns out not to be a mention costs one wasted
//!   `fetch_message` on the consumer, which discards it;
//! - *not* publishing a real mention makes it disappear silently.
//!
//! Every judgement below is written for the second one. The conformance suite
//! in `contracts/slack-event-gateway/` is the check.

use serde::Serialize;
use serde_json::Value;

/// Schema version. A consumer that does not know this refuses the record
/// rather than guessing at the fields it recognises.
pub const SCHEMA_VERSION: u32 = 1;

/// The opening of a user-group mention.
const SUBTEAM_OPEN: &str = "<!subteam^";

/// Longest id [`extract_subteam_ids`] accepts. Slack's are around nine
/// characters; the bound stops a crafted tag from smuggling a long string into
/// a record that is supposed to carry no free text.
pub const SUBTEAM_ID_MAX: usize = 32;

/// What a record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A channel message naming the operator, directly or through a group.
    Message,
    /// A `reaction_added` the operator themselves put on a message.
    Reaction,
    /// A Block Kit button press, flattened out of `actions[0]`.
    BlockActions,
}

/// Boolean verdicts reached by comparing against constant strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Flags {
    /// The text contained a mention tag matching the registered user id
    /// exactly. Always `false` on non-message kinds.
    pub mentions_me: bool,
}

/// One Pub/Sub message. **No body field, and there must never be one.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    /// Schema version.
    pub v: u32,
    /// Which of the three shapes this is.
    pub kind: RecordKind,
    /// Conversation the event happened in.
    pub channel: String,
    /// Slack timestamp of the message this record is about.
    pub ts: String,
    /// Enclosing thread, when there is one.
    pub thread_ts: Option<String>,
    /// Who caused the event.
    pub user: String,
    /// Verdicts from constant-string comparison.
    pub flags: Flags,
    /// User-group ids found in the text, first-seen order, de-duplicated.
    pub subteam_ids: Vec<String>,
    /// When this gateway accepted the delivery (RFC 3339).
    pub received_at: String,

    /// `reaction` only: the emoji name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction: Option<String>,
    /// `reaction` only: who wrote the reacted-to message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_user: Option<String>,
    /// `block_actions` only: `actions[0].action_id`, flattened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// `block_actions` only: `actions[0].value`, flattened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// `block_actions` only: where to answer the press.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_url: Option<String>,
    /// `block_actions` only: `container.channel_id`, falling back to
    /// `channel.id` — the two-path derivation the consumer already performs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_channel: Option<String>,
    /// `block_actions` only: `actions[0].action_ts`, part of the delivery
    /// identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_ts: Option<String>,
}

/// Which Request URL a delivery arrived on.
///
/// **Slack has two, and they are separate settings.** Event Subscriptions
/// carries messages and reactions; Interactivity & Shortcuts carries button
/// presses as a form-encoded `payload`. Socket Mode delivered both down one
/// WebSocket, so wiring up only the first is an easy mistake — and its symptom
/// is that every approval button goes dead while mentions keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// Event Subscriptions.
    Events,
    /// Interactivity & Shortcuts.
    Interactivity,
}

/// Which topic a record goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// Messages and reactions.
    Events,
    /// Button presses.
    BlockActions,
}

/// One record and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// Destination.
    pub topic: Topic,
    /// The message body.
    pub record: Record,
}

/// What a delivery turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// A `url_verification` handshake: echo the string, publish nothing.
    Challenge(String),
    /// Everything else. An empty vector is the common case.
    Publish(Vec<Published>),
}

/// Raw Slack delivery → what gets published.
///
/// `received_at` is passed in rather than read from a clock so the result is a
/// pure function of its inputs, which is what lets the conformance fixtures
/// pin it.
pub fn project(
    endpoint: Endpoint,
    payload: &Value,
    operator_user_id: &str,
    received_at: &str,
) -> Projection {
    match endpoint {
        Endpoint::Interactivity => Projection::Publish(
            project_press(payload, received_at)
                .map(|record| Published {
                    topic: Topic::BlockActions,
                    record,
                })
                .into_iter()
                .collect(),
        ),
        Endpoint::Events => project_event(payload, operator_user_id, received_at),
    }
}

fn project_event(payload: &Value, operator: &str, received_at: &str) -> Projection {
    match payload.get("type").and_then(Value::as_str) {
        Some("url_verification") => {
            return Projection::Challenge(
                payload
                    .get("challenge")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        Some("event_callback") => {}
        // An event type this build does not know is dropped, not guessed at.
        _ => return Projection::Publish(Vec::new()),
    }
    let Some(event) = payload.get("event") else {
        return Projection::Publish(Vec::new());
    };
    let record = match event.get("type").and_then(Value::as_str) {
        Some("message") => project_message(event, operator, received_at),
        Some("reaction_added") => project_reaction(event, operator, received_at),
        _ => None,
    };
    Projection::Publish(
        record
            .map(|record| Published {
                topic: Topic::Events,
                record,
            })
            .into_iter()
            .collect(),
    )
}

fn project_message(event: &Value, operator: &str, received_at: &str) -> Option<Record> {
    let field = |name: &str| event.get(name).and_then(Value::as_str);

    // Edits, deletions, system posts, bot posts.
    if event.get("subtype").is_some() || event.get("bot_id").is_some() {
        return None;
    }
    let user = field("user")?;
    let channel = field("channel")?;
    let ts = field("ts")?;
    // The operator's own posts, including their approved auto-replies — the
    // consumer would loop on those.
    if user == operator {
        return None;
    }
    // A message can arrive without `text` (a bare file share). Absent is
    // empty, never a panic: this process exists to not miss deliveries.
    let text = field("text").unwrap_or_default();
    let mentions_me = mentions_user(text, operator);
    let subteam_ids = extract_subteam_ids(text);
    // Named neither directly nor through a group. Broadcasts land here too:
    // neither predicate matches `<!here>` and friends, which is decision 8's
    // exclusion.
    if !mentions_me && subteam_ids.is_empty() {
        return None;
    }
    Some(Record {
        v: SCHEMA_VERSION,
        kind: RecordKind::Message,
        channel: channel.to_string(),
        ts: ts.to_string(),
        thread_ts: field("thread_ts").map(str::to_string),
        user: user.to_string(),
        flags: Flags { mentions_me },
        subteam_ids,
        received_at: received_at.to_string(),
        reaction: None,
        item_user: None,
        action_id: None,
        value: None,
        response_url: None,
        container_channel: None,
        action_ts: None,
    })
}

/// Only the operator's own reactions, and only on messages.
///
/// The consumer refuses anyone else's outright, and refuses `file` /
/// `file_comment` items outright, so publishing either would store records it
/// is guaranteed to discard — for seven days, per operator.
fn project_reaction(event: &Value, operator: &str, received_at: &str) -> Option<Record> {
    let user = event.get("user").and_then(Value::as_str)?;
    if user != operator {
        return None;
    }
    let reaction = event.get("reaction").and_then(Value::as_str)?;
    if event.pointer("/item/type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    // The body is not in a reaction event; `item` is the whole coordinate.
    let channel = event.pointer("/item/channel").and_then(Value::as_str)?;
    let ts = event.pointer("/item/ts").and_then(Value::as_str)?;
    Some(Record {
        v: SCHEMA_VERSION,
        kind: RecordKind::Reaction,
        channel: channel.to_string(),
        ts: ts.to_string(),
        thread_ts: None,
        user: user.to_string(),
        flags: Flags { mentions_me: false },
        subteam_ids: Vec::new(),
        received_at: received_at.to_string(),
        reaction: Some(reaction.to_string()),
        item_user: event
            .get("item_user")
            .and_then(Value::as_str)
            .map(str::to_string),
        action_id: None,
        value: None,
        response_url: None,
        container_channel: None,
        action_ts: None,
    })
}

/// Flatten `actions[0]` and derive the channel down both paths the consumer
/// reads: `container.channel_id`, falling back to `channel.id`.
///
/// **Freezing only the first path breaks every press whose payload carries
/// just the second.** The consumer's fallback is not dead code.
fn project_press(payload: &Value, received_at: &str) -> Option<Record> {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return None;
    }
    let action = payload.pointer("/actions/0")?;
    let channel = payload
        .pointer("/container/channel_id")
        .or_else(|| payload.pointer("/channel/id"))
        .and_then(Value::as_str)?;
    Some(Record {
        v: SCHEMA_VERSION,
        kind: RecordKind::BlockActions,
        channel: channel.to_string(),
        ts: payload
            .pointer("/container/message_ts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        thread_ts: None,
        user: payload
            .pointer("/user/id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        flags: Flags { mentions_me: false },
        subteam_ids: Vec::new(),
        received_at: received_at.to_string(),
        reaction: None,
        item_user: None,
        // Required: the delivery identity is built from these, so a record
        // without them cannot be de-duplicated by the consumer.
        action_id: Some(action.get("action_id").and_then(Value::as_str)?.to_string()),
        value: action
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string),
        response_url: payload
            .get("response_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        container_channel: Some(channel.to_string()),
        action_ts: Some(action.get("action_ts").and_then(Value::as_str)?.to_string()),
    })
}

/// Whether `text` names `user_id` directly.
///
/// Both `<@U123>` and `<@U123|label>` count. A plain substring test, not a
/// word-boundary one — `cc<@U_ME>よろしく` is a mention. The trailing `>` / `|`
/// is what keeps `<@U_MEX>` from matching `U_ME`.
pub fn mentions_user(text: &str, user_id: &str) -> bool {
    text.contains(&format!("<@{user_id}>")) || text.contains(&format!("<@{user_id}|"))
}

/// Every user-group id named in `text`, first-seen order, de-duplicated.
///
/// A tag counts only when it actually closes with `>` and the id is
/// alphanumeric and at most [`SUBTEAM_ID_MAX`] long. Taking everything up to
/// the next `>` would put `<!subteam^S0ABC and here is the secret>` on a topic
/// verbatim — free text, in a record whose premise is that it carries none.
///
/// The validation deliberately stops there. Requiring a leading `S`, or an
/// uppercase alphabet, would be a guess about Slack's id format, and guessing
/// wrong loses a mention.
pub fn extract_subteam_ids(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(SUBTEAM_OPEN) {
        rest = &rest[at + SUBTEAM_OPEN.len()..];
        let Some(end) = rest.find(['>', '|']) else {
            break;
        };
        let id = &rest[..end];
        let tail = &rest[end..];
        // `>` right after the id, or after a `|label` that still closes before
        // the next tag starts. Scanning for any `>` anywhere would let an
        // unterminated tag borrow the `>` of something later in the message.
        let closes = tail.starts_with('>')
            || tail
                .find(['>', '<'])
                .is_some_and(|at| tail.as_bytes()[at] == b'>');
        let plausible = !id.is_empty()
            && id.len() <= SUBTEAM_ID_MAX
            && id.chars().all(|c| c.is_ascii_alphanumeric());
        if closes && plausible && !found.iter().any(|seen| seen == id) {
            found.push(id.to_string());
        }
        rest = tail;
    }
    found
}

/// The stable key for "the same happening".
///
/// Both Slack's redelivery and Pub/Sub's delivery are at-least-once, so each
/// `kind` needs one. Without it each implementation falls to one side or the
/// other: duplicate work, or a dropped event.
pub fn delivery_id(record: &Record) -> String {
    match record.kind {
        RecordKind::Message => format!("message:{}:{}", record.channel, record.ts),
        RecordKind::Reaction => format!(
            "reaction:{}:{}:{}:{}",
            record.channel,
            record.ts,
            record.user,
            record.reaction.as_deref().unwrap_or_default()
        ),
        RecordKind::BlockActions => format!(
            "block_actions:{}:{}:{}",
            record
                .container_channel
                .as_deref()
                .unwrap_or(&record.channel),
            record.action_ts.as_deref().unwrap_or_default(),
            record.action_id.as_deref().unwrap_or_default()
        ),
    }
}

/// The JSON that goes on the topic.
pub fn encode(record: &Record) -> Value {
    serde_json::to_value(record).expect("a Record always serializes")
}

/// Decode the `payload=` field of an `application/x-www-form-urlencoded`
/// interactivity delivery.
///
/// Slack sends button presses this way, not as JSON. Reading them as JSON
/// yields a parse error on every press, which looks like "the buttons are
/// broken" rather than "the body was decoded wrong".
pub fn decode_interactivity_payload(body: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(body).ok()?;
    for pair in text.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key != "payload" {
            continue;
        }
        // `+` is a space in form encoding, which percent-decoding alone does
        // not handle — and a JSON body is full of them.
        let plus_decoded = value.replace('+', " ");
        let decoded = percent_encoding::percent_decode_str(&plus_decoded)
            .decode_utf8()
            .ok()?;
        return serde_json::from_str(&decoded).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_tags_reject_a_longer_id() {
        assert!(mentions_user("<@U_ME> hi", "U_ME"));
        assert!(mentions_user("<@U_ME|tomoya> hi", "U_ME"));
        assert!(mentions_user("cc<@U_ME>よろしく", "U_ME"));
        assert!(!mentions_user("<@U_MEX> hi", "U_ME"));
        assert!(!mentions_user("<@U_OTHER> hi", "U_ME"));
    }

    #[test]
    fn broadcasts_match_neither_predicate() {
        for tag in ["<!here>", "<!channel>", "<!everyone>"] {
            assert!(!mentions_user(tag, "U_ME"));
            assert!(extract_subteam_ids(tag).is_empty());
        }
    }

    #[test]
    fn a_malformed_subteam_tag_cannot_smuggle_text() {
        assert_eq!(
            extract_subteam_ids("<!subteam^S0A> x <!subteam^S0B|@b> y <!subteam^S0A>"),
            vec!["S0A".to_string(), "S0B".to_string()]
        );
        assert!(extract_subteam_ids("<!subteam^S0ABC and here is the secret> hi").is_empty());
        assert!(extract_subteam_ids("<!subteam^S0A").is_empty());
        assert!(extract_subteam_ids("<!subteam^S0A|@team").is_empty());
        assert!(extract_subteam_ids("<!subteam^S0A|@team <@U_ME> done").is_empty());
        let long = "X".repeat(SUBTEAM_ID_MAX + 1);
        assert!(extract_subteam_ids(&format!("<!subteam^{long}>")).is_empty());
    }

    #[test]
    fn form_encoded_presses_decode() {
        let payload = r#"{"type":"block_actions","user":{"id":"U_ME"}}"#;
        let encoded: String =
            percent_encoding::utf8_percent_encode(payload, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let body = format!("payload={encoded}");
        let decoded = decode_interactivity_payload(body.as_bytes()).expect("decodes");
        assert_eq!(
            decoded.get("type").and_then(Value::as_str),
            Some("block_actions")
        );
        // `+` means space, not a literal plus.
        let spaced =
            decode_interactivity_payload(br#"payload=%7B%22a%22%3A%22x+y%22%7D"#).expect("decodes");
        assert_eq!(spaced.get("a").and_then(Value::as_str), Some("x y"));
        assert!(decode_interactivity_payload(b"other=1").is_none());
    }

    /// The record type has nowhere to put a body. This is not the guarantee —
    /// a process can hold a string in memory — but a field appearing here
    /// would make the guarantee unarguable to enforce.
    #[test]
    fn the_record_shape_carries_no_body() {
        let record = Record {
            v: 1,
            kind: RecordKind::Message,
            channel: "C1".into(),
            ts: "1.0".into(),
            thread_ts: None,
            user: "U_SENDER".into(),
            flags: Flags { mentions_me: true },
            subteam_ids: Vec::new(),
            received_at: "2026-09-13T00:00:00Z".into(),
            reaction: None,
            item_user: None,
            action_id: None,
            value: None,
            response_url: None,
            container_channel: None,
            action_ts: None,
        };
        let json = encode(&record);
        let keys: Vec<&str> = json
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        for forbidden in ["text", "body", "message", "blocks", "attachments", "files"] {
            assert!(!keys.contains(&forbidden), "`{forbidden}` must not exist");
        }
    }
}
