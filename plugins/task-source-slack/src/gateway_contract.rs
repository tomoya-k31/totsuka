//! The Event Gateway contract (#656, [ADR-0072](../../../ai-docs/decisions/adr-0072-slack-event-gateway.md)
//! decisions 4 and 7): the Pub/Sub record shape, the delivery-identity rules,
//! and the two text predicates the gateway's pre-filter is built from.
//!
//! `event_source = "gateway"` moves Slack's delivery onto an HTTP Request URL
//! answered by a gateway **outside this workspace** — possibly a fork, run by
//! the operator (decision 9). totsuka cannot trust that code, so the only
//! thing the two sides share is this module's shape plus the fixtures under
//! `contracts/slack-event-gateway/`.
//!
//! # Why the predicates live here and not only in the fixtures
//!
//! Decision 4 narrowed what gets published, which turned the gateway's filter
//! into a **gate**: a message it drops has no record and is invisible to
//! totsuka forever. The two error directions are not symmetric —
//!
//! - a false positive costs one wasted `fetch_message`, which [`crate::mention`]
//!   then discards;
//! - a false negative makes a mention **disappear silently**.
//!
//! So [`MentionTags`] is not a second copy of the filter's row 4 — it *is*
//! row 4, and [`crate::mention::MentionFilter`] calls it. A conformance
//! fixture can only prove that two implementations agreed on the cases someone
//! thought to write down; sharing the code removes the chance to disagree on
//! the ones nobody did.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The schema version this build speaks. A record carrying anything else is
/// refused rather than guessed at (ADR-0055: lenient about unknown *fields*,
/// strict about an unknown *version*).
pub const SCHEMA_VERSION: u32 = 1;

/// Broadcast tags, listed so the exclusion is greppable and testable.
///
/// Decision 8 leaves these out of scope: they are not a name, they are "to
/// whoever is here", and turning them into tasks would make noise dominant.
/// Nothing in this module matches them — the point of the constant is that a
/// test can say so out loud.
pub const BROADCAST_TAGS: [&str; 3] = ["<!here>", "<!channel>", "<!everyone>"];

/// The opening of a user-group mention. Slack writes the id after it, closed
/// by `>` or followed by `|<label>>`.
const SUBTEAM_OPEN: &str = "<!subteam^";

/// What a record is about. The wire spelling is the `kind` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A channel message that named the operator, directly or through a group.
    Message,
    /// A `reaction_added` the operator themselves put on a message.
    Reaction,
    /// A Block Kit button press, flattened out of `actions[0]`.
    BlockActions,
}

/// Boolean verdicts the gateway reached by comparing against constant
/// strings. Deliberately a struct and not a bare bool: decision 8 says a
/// changed policy may add a flag under the same `v: 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    /// The text contained a mention tag matching the registered user id
    /// **exactly**. Always `false` on non-message kinds.
    pub mentions_me: bool,
}

/// One Pub/Sub message.
///
/// **There is no body field, and there must never be one.** The record carries
/// coordinates plus the verdicts of constant-string comparisons; totsuka
/// re-fetches the text from Slack with its own token. Note that this shape
/// does not *prevent* a replacement gateway from leaking — it can hold the
/// request in memory, log it, or post it elsewhere. "Does not store, forward
/// or log the body" is a behaviour #659 defends with tests, independently of
/// the schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayRecord {
    /// Schema version; see [`SCHEMA_VERSION`].
    pub v: u32,
    /// Which of the three record shapes this is.
    pub kind: RecordKind,
    /// Conversation the event happened in. For [`RecordKind::BlockActions`]
    /// this equals [`container_channel`](Self::container_channel).
    pub channel: String,
    /// Slack timestamp of the message this record is about. For a reaction,
    /// the reacted-to message (`item.ts`); for a press, the message holding
    /// the button (`container.message_ts`).
    pub ts: String,
    /// Enclosing thread, when there is one.
    pub thread_ts: Option<String>,
    /// Who caused the event: the sender, the reacting user, or the presser.
    pub user: String,
    /// Verdicts from constant-string comparison.
    pub flags: Flags,
    /// User-group ids found in the text, first-seen order, de-duplicated.
    /// **Membership is not resolved here** — the gateway cannot know it
    /// without a second source of truth that would go stale (decision 8).
    #[serde(default)]
    pub subteam_ids: Vec<String>,
    /// When the gateway accepted the delivery (RFC 3339). Diagnostic only:
    /// the ingest window is computed from Slack's `ts`, so a gateway with a
    /// skewed clock cannot change what totsuka files.
    pub received_at: String,

    // ---- `kind`-specific fields. Decision 7 closes each list with "only". --
    /// [`RecordKind::Reaction`]: the emoji name, without colons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<String>,
    /// [`RecordKind::Reaction`]: who wrote the reacted-to message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_user: Option<String>,
    /// [`RecordKind::BlockActions`]: `actions[0].action_id`, flattened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// [`RecordKind::BlockActions`]: `actions[0].value`, flattened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// [`RecordKind::BlockActions`]: where to answer the press. Lives ~30
    /// minutes, which is why that topic's retention must exceed it
    /// (decision 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_url: Option<String>,
    /// [`RecordKind::BlockActions`]: `container.channel_id`, falling back to
    /// `channel.id` — the two-path derivation `approval.rs::press_channel`
    /// already performs. Pinning only the first path breaks presses whose
    /// payload carries just the second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_channel: Option<String>,
    /// [`RecordKind::BlockActions`]: `actions[0].action_ts`. Part of the
    /// delivery identity, which is why it is in the enumeration — a contract
    /// whose identity key needs a field it forbids contradicts itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_ts: Option<String>,
}

/// Why a record was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// Not JSON, or not the right JSON types.
    #[error("gateway record is malformed: {0}")]
    Malformed(String),
    /// A `v` this build does not know. Refused rather than interpreted: the
    /// fields it does recognise may well mean something else now.
    #[error(
        "gateway record has schema version {found}, but this build speaks {SCHEMA_VERSION} — \
         update totsuka, or roll the gateway back"
    )]
    UnsupportedVersion {
        /// The version the record claimed.
        found: u64,
    },
    /// A field the `kind` requires is missing, or one it forbids is present.
    #[error("gateway record of kind {kind:?} {problem}")]
    KindMismatch {
        /// The `kind` that was declared.
        kind: RecordKind,
        /// What was wrong with the fields around it.
        problem: String,
    },
}

impl GatewayRecord {
    /// Parse one Pub/Sub message body.
    ///
    /// Unknown fields are ignored (a newer gateway may add some); an unknown
    /// `v` is refused.
    pub fn parse(raw: &str) -> Result<Self, ContractError> {
        let value: Value =
            serde_json::from_str(raw).map_err(|e| ContractError::Malformed(e.to_string()))?;
        Self::from_value(&value)
    }

    /// [`parse`](Self::parse) for a body that is already a [`Value`].
    pub fn from_value(value: &Value) -> Result<Self, ContractError> {
        // The version gate runs *before* deserialization, so a v2 record that
        // renamed a field reports "unsupported version" rather than a serde
        // error about the rename.
        let v = value
            .get("v")
            .and_then(Value::as_u64)
            .ok_or_else(|| ContractError::Malformed("missing or non-integer `v`".into()))?;
        if v != u64::from(SCHEMA_VERSION) {
            return Err(ContractError::UnsupportedVersion { found: v });
        }
        let record: Self = serde_json::from_value(value.clone())
            .map_err(|e| ContractError::Malformed(e.to_string()))?;
        record.check_kind_fields()?;
        Ok(record)
    }

    /// Decision 7 closes each `kind`'s extra fields with "only". Enforce both
    /// halves: what the kind needs must be there, and what belongs to another
    /// kind must not.
    fn check_kind_fields(&self) -> Result<(), ContractError> {
        // Two different questions, and conflating them was a bug: which
        // fields a kind may *carry* (decision 7 closes each list with "only")
        // and which it must *have*. Only the second list can reject a record,
        // and it holds exactly the fields `delivery_id` needs — demanding more
        // would throw away deliveries over a field the consumer already treats
        // as optional, which is the error direction that loses events.
        let reaction_fields = [
            ("reaction", self.reaction.is_some()),
            ("item_user", self.item_user.is_some()),
        ];
        let reaction_required = ["reaction"];
        let press_fields = [
            ("action_id", self.action_id.is_some()),
            ("value", self.value.is_some()),
            ("response_url", self.response_url.is_some()),
            ("container_channel", self.container_channel.is_some()),
            ("action_ts", self.action_ts.is_some()),
        ];
        let press_required = ["action_id", "container_channel", "action_ts"];
        let refuse = |problem: String| {
            Err(ContractError::KindMismatch {
                kind: self.kind,
                problem,
            })
        };
        let missing = |fields: &[(&str, bool)], required: &[&str]| -> Vec<String> {
            fields
                .iter()
                .filter(|(name, present)| !present && required.contains(name))
                .map(|(name, _)| (*name).to_string())
                .collect()
        };
        let present = |fields: &[(&str, bool)]| -> Vec<String> {
            fields
                .iter()
                .filter(|(_, present)| *present)
                .map(|(name, _)| (*name).to_string())
                .collect()
        };

        match self.kind {
            RecordKind::Message => {
                let extra: Vec<String> = present(&reaction_fields)
                    .into_iter()
                    .chain(present(&press_fields))
                    .collect();
                if !extra.is_empty() {
                    return refuse(format!(
                        "carries fields of another kind: {}",
                        extra.join(", ")
                    ));
                }
            }
            RecordKind::Reaction => {
                let absent = missing(&reaction_fields, &reaction_required);
                if !absent.is_empty() {
                    return refuse(format!("is missing {}", absent.join(", ")));
                }
                let extra = present(&press_fields);
                if !extra.is_empty() {
                    return refuse(format!(
                        "carries fields of another kind: {}",
                        extra.join(", ")
                    ));
                }
            }
            RecordKind::BlockActions => {
                let absent = missing(&press_fields, &press_required);
                if !absent.is_empty() {
                    return refuse(format!("is missing {}", absent.join(", ")));
                }
                let extra = present(&reaction_fields);
                if !extra.is_empty() {
                    return refuse(format!(
                        "carries fields of another kind: {}",
                        extra.join(", ")
                    ));
                }
                // `channel` and `container_channel` hold the same value here.
                // Carrying both is redundant, but dropping `channel` would
                // make the base shape depend on `kind`; the redundancy is
                // kept and turned into a checked invariant instead.
                if self.container_channel.as_deref() != Some(self.channel.as_str()) {
                    return refuse(format!(
                        "has channel `{}` but container_channel {:?}; they name the same \
                         conversation and must agree",
                        self.channel, self.container_channel
                    ));
                }
            }
        }
        Ok(())
    }

    /// The stable key for "the same happening", per decision 7.
    ///
    /// Both Slack's redelivery and Pub/Sub's delivery are at-least-once, so
    /// without an agreed key each implementation falls to one side or the
    /// other: duplicate work, or a dropped event.
    pub fn delivery_id(&self) -> String {
        match self.kind {
            RecordKind::Message => format!("message:{}:{}", self.channel, self.ts),
            RecordKind::Reaction => format!(
                "reaction:{}:{}:{}:{}",
                self.channel,
                self.ts,
                self.user,
                self.reaction.as_deref().unwrap_or_default()
            ),
            RecordKind::BlockActions => format!(
                "block_actions:{}:{}:{}",
                self.container_channel.as_deref().unwrap_or(&self.channel),
                self.action_ts.as_deref().unwrap_or_default(),
                self.action_id.as_deref().unwrap_or_default()
            ),
        }
    }

    /// The key the existing dedup uses, for records that have one.
    ///
    /// Byte-identical to [`crate::mention::Mention::message_key`], so a
    /// gateway-sourced message and a Socket Mode one collide in the same LRU.
    pub fn message_key(&self) -> Option<String> {
        (self.kind == RecordKind::Message).then(|| format!("{}:{}", self.channel, self.ts))
    }

    /// Whether totsuka has to re-fetch the text before it can judge.
    ///
    /// **Both conditions are load-bearing.** A record naming only a user group
    /// leaves `mentions_me` false — membership is resolved in totsuka, not at
    /// the edge — so fetching on `mentions_me` alone would let every group
    /// mention pass through untouched.
    pub fn wants_body(&self) -> bool {
        self.kind == RecordKind::Message && (self.flags.mentions_me || !self.subteam_ids.is_empty())
    }

    /// Rebuild the `block_actions` payload the pipeline expects.
    ///
    /// Only the fields the consumer actually reads are emitted, which is what
    /// makes the projection lossless in the direction that matters:
    /// `pipeline::handle_block_actions` reads `actions[0].action_id`,
    /// `actions[0].value` and `response_url`; `approval::press_channel` reads
    /// `container.channel_id` then `channel.id`. The flattened record is
    /// re-nested here rather than at the call site so there is one place to
    /// change when the consumer starts reading something new.
    pub fn block_actions_payload(&self) -> Option<Value> {
        if self.kind != RecordKind::BlockActions {
            return None;
        }
        let channel = self.container_channel.as_deref().unwrap_or(&self.channel);
        Some(json!({
            "type": "block_actions",
            "user": { "id": self.user },
            "container": { "type": "message", "channel_id": channel, "message_ts": self.ts },
            "channel": { "id": channel },
            "response_url": self.response_url,
            "actions": [{
                "type": "button",
                "action_id": self.action_id,
                "value": self.value,
                "action_ts": self.action_ts,
            }],
        }))
    }
}

/// The two spellings of one user's mention tag, precomputed.
///
/// Slack writes either `<@U123>` or `<@U123|label>`; matching on the id alone
/// would make `<@U_MEX>` a mention of `U_ME`.
#[derive(Debug, Clone)]
pub struct MentionTags {
    closed: String,
    labeled: String,
}

impl MentionTags {
    /// Tags for `user_id`.
    pub fn new(user_id: &str) -> Self {
        Self {
            closed: format!("<@{user_id}>"),
            labeled: format!("<@{user_id}|"),
        }
    }

    /// Whether `text` names this user directly.
    ///
    /// A plain substring test, not a word-boundary one: `cc<@U_ME>よろしく` is
    /// a mention. The trailing `>` / `|` is what keeps `<@U_MEX>` out.
    pub fn matches(&self, text: &str) -> bool {
        text.contains(&self.closed) || text.contains(&self.labeled)
    }
}

/// Longest id [`extract_subteam_ids`] will accept. Slack's are around nine
/// characters; the bound exists so a crafted tag cannot smuggle a long string
/// into a record that is supposed to carry no free text.
///
/// Part of the contract rather than an implementation detail: a gateway that
/// accepts longer ids publishes records this build would not.
pub const SUBTEAM_ID_MAX: usize = 32;

/// Every user-group id named in `text`, first-seen order, de-duplicated.
///
/// Both `<!subteam^S123>` and `<!subteam^S123|@team>` yield `S123`. A tag is
/// only accepted when it actually closes with `>` and the id is alphanumeric
/// and at most [`SUBTEAM_ID_MAX`] long.
///
/// **Why validate at all, when this is only a pre-filter?** Because
/// `subteam_ids` travels in a record whose whole premise is that it carries no
/// message text. Taking everything up to the next `>` would put
/// `<!subteam^S0ABC and here is the secret>` on a Pub/Sub topic verbatim.
///
/// **Why not validate harder** — no leading `S`, no case rule? Those would be
/// guesses about Slack's id format, and guessing wrong here fails in the one
/// direction that loses a mention (decision 4). Rejecting whitespace and
/// punctuation is enough to stop prose while staying agnostic about the
/// alphabet.
///
/// Membership is not consulted: an id for a group the operator does not belong
/// to still lands in the record, because the gateway has no way to know, and
/// guessing wrong drops the mention silently (decision 8).
pub fn extract_subteam_ids(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(SUBTEAM_OPEN) {
        rest = &rest[at + SUBTEAM_OPEN.len()..];
        let Some(end) = rest.find(['>', '|']) else {
            // `<!subteam^` with nothing closing it: not a mention, and there
            // is nothing further to scan.
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
        rest = &rest[end..];
    }
    found
}

/// Which Request URL a delivery arrived on.
///
/// **Slack has two, and they are separate settings.** Event Subscriptions
/// carries messages and reactions; Interactivity & Shortcuts carries button
/// presses, as form-encoded `payload`. Socket Mode delivered both down one
/// WebSocket, so moving to HTTP and wiring up only the first is an easy
/// mistake — and its symptom is that every approval button goes dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    /// Event Subscriptions.
    Events,
    /// Interactivity & Shortcuts.
    Interactivity,
}

/// Which topic a record is published to. Presses go to their own, because its
/// retention has to clear `response_url`'s ~30-minute life while the other's
/// is measured in days (decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Messages and reactions.
    Events,
    /// Button presses.
    BlockActions,
}

/// One record and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// Destination topic.
    pub topic: Topic,
    /// The message body.
    pub record: GatewayRecord,
}

/// What a delivery turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// A `url_verification` handshake: echo the string, publish nothing.
    Challenge(String),
    /// Everything else. **An empty vector is the common case** — most traffic
    /// in a busy workspace is not about the operator.
    Publish(Vec<Published>),
}

/// The one registered operator a delivery is being judged against.
///
/// Routing is by opaque path, so by the time a payload gets here the gateway
/// already knows whose it is and has verified the signature with that user's
/// key (decision 6). Reading the identity out of the body instead would mean
/// choosing the key by trusting unverified input.
#[derive(Debug, Clone, Copy)]
pub struct Registration<'a> {
    /// The operator's Slack user id.
    pub user_id: &'a str,
}

/// **The reference projection**: raw Slack delivery → what gets published.
///
/// Decision 7 makes totsuka's side authoritative, and this is that side. A
/// gateway is conformant when it reproduces this function's output for every
/// case under `contracts/slack-event-gateway/`; totsuka's own tests run it
/// against those same cases, so the fixtures cannot drift away from the
/// consumer that depends on them.
///
/// `received_at` is passed in rather than read from a clock so the result is
/// a pure function of its inputs — which is what lets the fixtures pin it.
pub fn project(
    endpoint: Endpoint,
    payload: &Value,
    registration: Registration<'_>,
    received_at: &str,
) -> Projection {
    match endpoint {
        Endpoint::Interactivity => Projection::Publish(
            project_press(payload, registration, received_at)
                .map(|record| Published {
                    topic: Topic::BlockActions,
                    record,
                })
                .into_iter()
                .collect(),
        ),
        Endpoint::Events => project_event(payload, registration, received_at),
    }
}

fn project_event(payload: &Value, registration: Registration<'_>, received_at: &str) -> Projection {
    match payload.get("type").and_then(Value::as_str) {
        Some("url_verification") => {
            let challenge = payload
                .get("challenge")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Projection::Challenge(challenge.to_string());
        }
        Some("event_callback") => {}
        // An event type this build does not know is dropped, not guessed at.
        _ => return Projection::Publish(Vec::new()),
    }
    let Some(event) = payload.get("event") else {
        return Projection::Publish(Vec::new());
    };
    let record = match event.get("type").and_then(Value::as_str) {
        Some("message") => project_message(event, registration, received_at),
        Some("reaction_added") => project_reaction(event, registration, received_at),
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

/// Decision 4's message filter, in the order [`crate::mention`] applies it so
/// the two cannot disagree about *why* something was dropped.
fn project_message(
    event: &Value,
    registration: Registration<'_>,
    received_at: &str,
) -> Option<GatewayRecord> {
    let str_field = |name: &str| event.get(name).and_then(Value::as_str);

    // Edits, deletions, system posts, bot posts.
    if event.get("subtype").is_some() || event.get("bot_id").is_some() {
        return None;
    }
    let user = str_field("user")?;
    let channel = str_field("channel")?;
    let ts = str_field("ts")?;
    // The operator's own posts, including approved auto-replies.
    if user == registration.user_id {
        return None;
    }
    // A message can arrive without `text` (a bare file share, say). Absent is
    // empty, never a panic: this runs in a process whose whole job is to not
    // miss deliveries.
    let text = str_field("text").unwrap_or_default();
    let mentions_me = MentionTags::new(registration.user_id).matches(text);
    let subteam_ids = extract_subteam_ids(text);
    // Named neither directly nor through a group — so not ours. Broadcasts
    // land here too, which is decision 8's exclusion: neither predicate
    // matches `<!here>` and friends.
    if !mentions_me && subteam_ids.is_empty() {
        return None;
    }
    Some(GatewayRecord {
        v: SCHEMA_VERSION,
        kind: RecordKind::Message,
        channel: channel.to_string(),
        ts: ts.to_string(),
        thread_ts: str_field("thread_ts").map(str::to_string),
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

/// Only the operator's own reactions. `reaction.rs` will not act on anyone
/// else's (ADR-0025), so publishing them would store records the consumer is
/// guaranteed to throw away — for seven days, per user.
fn project_reaction(
    event: &Value,
    registration: Registration<'_>,
    received_at: &str,
) -> Option<GatewayRecord> {
    let user = event.get("user").and_then(Value::as_str)?;
    if user != registration.user_id {
        return None;
    }
    let reaction = event.get("reaction").and_then(Value::as_str)?;
    // Only reactions on messages. `reaction.rs::reaction_target` refuses
    // `file` and `file_comment` items outright, so publishing them would store
    // records the consumer is guaranteed to discard — the same objection that
    // rules out other people's reactions, one level down.
    if event.pointer("/item/type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    // The body is not in a reaction event; `item` is the whole coordinate.
    let channel = event.pointer("/item/channel").and_then(Value::as_str)?;
    let ts = event.pointer("/item/ts").and_then(Value::as_str)?;
    Some(GatewayRecord {
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

/// Flatten `actions[0]` and derive the channel down the same two paths
/// `approval::press_channel` reads.
fn project_press(
    payload: &Value,
    _registration: Registration<'_>,
    received_at: &str,
) -> Option<GatewayRecord> {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return None;
    }
    let action = payload.pointer("/actions/0")?;
    let channel = payload
        .pointer("/container/channel_id")
        .or_else(|| payload.pointer("/channel/id"))
        .and_then(Value::as_str)?;
    Some(GatewayRecord {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message_record() -> GatewayRecord {
        GatewayRecord {
            v: 1,
            kind: RecordKind::Message,
            channel: "C0LOBBY".into(),
            ts: "1757640000.000100".into(),
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
        }
    }

    fn press_record() -> GatewayRecord {
        GatewayRecord {
            kind: RecordKind::BlockActions,
            channel: "D0SELFDM".into(),
            ts: "1757640100.000200".into(),
            user: "U_ME".into(),
            flags: Flags { mentions_me: false },
            action_id: Some("approve_reply".into()),
            value: Some(r#"{"d":"draft-1"}"#.into()),
            response_url: Some("https://hooks.slack.com/actions/1".into()),
            container_channel: Some("D0SELFDM".into()),
            action_ts: Some("1757640200.111111".into()),
            ..message_record()
        }
    }

    #[test]
    fn mention_tags_reject_a_longer_id() {
        let tags = MentionTags::new("U_ME");
        assert!(tags.matches("<@U_ME> hi"));
        assert!(tags.matches("<@U_ME|tomoya> hi"));
        assert!(tags.matches("cc<@U_ME>よろしく"));
        assert!(!tags.matches("<@U_MEX> hi"));
        assert!(!tags.matches("<@U_OTHER> hi"));
        assert!(!tags.matches("U_ME"));
    }

    #[test]
    fn broadcast_tags_are_not_mentions_of_anyone() {
        let tags = MentionTags::new("U_ME");
        for tag in BROADCAST_TAGS {
            assert!(!tags.matches(tag), "{tag} must not match a personal tag");
            assert!(
                extract_subteam_ids(tag).is_empty(),
                "{tag} must not yield a group id"
            );
        }
    }

    #[test]
    fn subteam_ids_are_deduped_in_first_seen_order() {
        assert_eq!(
            extract_subteam_ids("<!subteam^S0A> x <!subteam^S0B|@b> y <!subteam^S0A>"),
            vec!["S0A".to_string(), "S0B".to_string()]
        );
        assert!(extract_subteam_ids("no groups here").is_empty());
        assert!(extract_subteam_ids("<!subteam^>").is_empty());
    }

    /// The record is supposed to carry no message text, so a tag that does not
    /// look like a tag must not become an id.
    #[test]
    fn a_malformed_subteam_tag_cannot_smuggle_text() {
        // Everything-up-to-`>` would have made this the "id".
        assert!(extract_subteam_ids("<!subteam^S0ABC and here is the secret> hi").is_empty());
        // No closing `>` at all.
        assert!(extract_subteam_ids("<!subteam^S0A").is_empty());
        assert!(extract_subteam_ids("<!subteam^S0A|@team").is_empty());
        // An unterminated tag must not borrow a later tag's `>`.
        assert!(extract_subteam_ids("<!subteam^S0A|@team <@U_ME> done").is_empty());
        // Longer than any real id.
        let long = "X".repeat(SUBTEAM_ID_MAX + 1);
        assert!(extract_subteam_ids(&format!("<!subteam^{long}>")).is_empty());
        // …but exactly at the bound is still a group mention: the cost of
        // being wrong here is a mention that vanishes.
        let at_bound = "X".repeat(SUBTEAM_ID_MAX);
        assert_eq!(
            extract_subteam_ids(&format!("<!subteam^{at_bound}>")),
            vec![at_bound]
        );
    }

    /// Required and permitted are different questions: demanding a field the
    /// consumer treats as optional would discard real deliveries.
    #[test]
    fn optional_kind_fields_do_not_reject_a_record() {
        let mut anonymous_item = GatewayRecord {
            kind: RecordKind::Reaction,
            reaction: Some("eyes".into()),
            item_user: None,
            ..message_record()
        };
        anonymous_item.flags.mentions_me = false;
        let value = serde_json::to_value(&anonymous_item).unwrap();
        assert_eq!(GatewayRecord::from_value(&value).unwrap(), anonymous_item);

        let valueless_press = GatewayRecord {
            value: None,
            response_url: None,
            ..press_record()
        };
        let value = serde_json::to_value(&valueless_press).unwrap();
        assert_eq!(GatewayRecord::from_value(&value).unwrap(), valueless_press);
    }

    /// Everything the reference projection emits must parse back.
    #[test]
    fn the_projection_never_emits_an_unparseable_record() {
        let deliveries = [
            (
                Endpoint::Events,
                json!({"type": "event_callback", "event": {
                    "type": "reaction_added", "user": "U_ME", "reaction": "eyes",
                    "item": {"type": "message", "channel": "C1", "ts": "1.0"}
                }}),
            ),
            (
                Endpoint::Interactivity,
                json!({"type": "block_actions", "user": {"id": "U_ME"},
                       "container": {"channel_id": "D1", "message_ts": "2.0"},
                       "actions": [{"action_id": "approve_reply", "action_ts": "3.0"}]}),
            ),
        ];
        for (endpoint, payload) in deliveries {
            let Projection::Publish(published) = project(
                endpoint,
                &payload,
                Registration { user_id: "U_ME" },
                "2026-09-13T00:00:00Z",
            ) else {
                panic!("expected records");
            };
            for item in published {
                let value = serde_json::to_value(&item.record).unwrap();
                GatewayRecord::from_value(&value)
                    .expect("a projected record must survive its own schema check");
            }
        }
    }

    /// `reaction.rs` refuses non-message items, so storing them would keep
    /// records the consumer is guaranteed to throw away.
    #[test]
    fn reactions_on_non_message_items_are_not_published() {
        for item_type in ["file", "file_comment"] {
            let payload = json!({"type": "event_callback", "event": {
                "type": "reaction_added", "user": "U_ME", "reaction": "eyes",
                "item": {"type": item_type, "file": "F1"}
            }});
            assert_eq!(
                project(
                    Endpoint::Events,
                    &payload,
                    Registration { user_id: "U_ME" },
                    "2026-09-13T00:00:00Z"
                ),
                Projection::Publish(Vec::new()),
                "{item_type} must not be published"
            );
        }
    }

    #[test]
    fn unknown_version_is_refused_not_guessed() {
        let mut value = serde_json::to_value(message_record()).unwrap();
        value["v"] = json!(2);
        assert_eq!(
            GatewayRecord::from_value(&value),
            Err(ContractError::UnsupportedVersion { found: 2 })
        );
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let mut value = serde_json::to_value(message_record()).unwrap();
        value["field_from_a_newer_gateway"] = json!("whatever");
        assert_eq!(GatewayRecord::from_value(&value).unwrap(), message_record());
    }

    #[test]
    fn a_body_field_cannot_survive_a_round_trip() {
        let mut value = serde_json::to_value(message_record()).unwrap();
        value["text"] = json!("<@U_ME> secret business");
        let parsed = GatewayRecord::from_value(&value).unwrap();
        let back = serde_json::to_value(&parsed).unwrap();
        assert!(
            back.get("text").is_none(),
            "the type must have nowhere to keep a body"
        );
    }

    #[test]
    fn kind_fields_are_closed_in_both_directions() {
        // A message carrying a press field.
        let mut stray = message_record();
        stray.action_id = Some("approve_reply".into());
        let value = serde_json::to_value(&stray).unwrap();
        assert!(matches!(
            GatewayRecord::from_value(&value),
            Err(ContractError::KindMismatch { .. })
        ));

        // A press missing one of its required fields.
        let mut incomplete = press_record();
        incomplete.action_ts = None;
        let value = serde_json::to_value(&incomplete).unwrap();
        assert!(matches!(
            GatewayRecord::from_value(&value),
            Err(ContractError::KindMismatch { .. })
        ));

        // A press whose two channel spellings disagree.
        let mut split = press_record();
        split.container_channel = Some("C0ELSEWHERE".into());
        let value = serde_json::to_value(&split).unwrap();
        assert!(matches!(
            GatewayRecord::from_value(&value),
            Err(ContractError::KindMismatch { .. })
        ));
    }

    #[test]
    fn wants_body_covers_group_only_records() {
        let mut group_only = message_record();
        group_only.flags.mentions_me = false;
        group_only.subteam_ids = vec!["S0A".into()];
        assert!(
            group_only.wants_body(),
            "a subteam-only record must still be fetched, or group mentions vanish"
        );

        let mut neither = message_record();
        neither.flags.mentions_me = false;
        assert!(!neither.wants_body());
        assert!(!press_record().wants_body());
    }

    #[test]
    fn message_key_matches_the_existing_dedup_key() {
        let record = message_record();
        assert_eq!(
            record.message_key().as_deref(),
            Some("C0LOBBY:1757640000.000100")
        );
        assert_eq!(
            record.delivery_id(),
            format!("message:{}", record.message_key().unwrap())
        );
        assert!(press_record().message_key().is_none());
    }

    #[test]
    fn rebuilt_press_payload_is_read_by_both_consumers() {
        let payload = press_record().block_actions_payload().unwrap();
        // `pipeline::handle_block_actions`
        assert_eq!(
            payload
                .pointer("/actions/0/action_id")
                .and_then(Value::as_str),
            Some("approve_reply")
        );
        assert_eq!(
            payload.pointer("/actions/0/value").and_then(Value::as_str),
            Some(r#"{"d":"draft-1"}"#)
        );
        assert_eq!(
            payload.get("response_url").and_then(Value::as_str),
            Some("https://hooks.slack.com/actions/1")
        );
        // `approval::press_channel`, both of its paths.
        assert_eq!(crate::approval::press_channel(&payload), Some("D0SELFDM"));
        assert_eq!(
            payload.pointer("/channel/id").and_then(Value::as_str),
            Some("D0SELFDM")
        );
        assert!(message_record().block_actions_payload().is_none());
    }
}
