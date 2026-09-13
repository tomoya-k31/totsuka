//! The Pub/Sub source behind `event_source = "gateway"` (#657,
//! [ADR-0072](../../../ai-docs/decisions/adr-0072-slack-event-gateway.md)
//! decisions 2-5).
//!
//! Slack delivers to an Event Gateway over HTTP; the gateway projects each
//! delivery to coordinates (never the body — see [`crate::gateway_contract`])
//! and publishes them. This module drains those queues and hands the pipeline
//! the **same [`SocketEvent`]s Socket Mode produces**, so everything
//! downstream is untouched.
//!
//! # Three things that look like details and are not
//!
//! **`pull` is not a subscription.** The REST `pull` method is specified as
//! *may* wait for a message to arrive — "the system may wait (for a bounded
//! amount of time)". *May*. An implementation that assumes it always blocks
//! becomes a busy loop the moment the server answers immediately, so every
//! empty response feeds a backoff here.
//!
//! **Delivery is at-least-once on both legs.** Slack redelivers, and so does
//! Pub/Sub. Records are de-duplicated by
//! [`GatewayRecord::delivery_id`](crate::gateway_contract::GatewayRecord::delivery_id),
//! which is why that key is part of the frozen contract rather than something
//! each side invents.
//!
//! **There is no fallback to Socket Mode.** The two are mutually exclusive in
//! the Slack app itself, so "fall back when the gateway is unreachable" is not
//! something this process could do even if it wanted to — the switch is a
//! manifest change. Unreachable means back off and warn.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::{DEFAULT_DRAIN_LIMIT, DEFAULT_DRAIN_MAX_AGE_HOURS, GatewayConfig, SlackConfig};
use crate::error::SlackError;
use crate::gateway_contract::{GatewayRecord, RecordKind};
use crate::slack_api::SlackApi;
use crate::socket_mode::SocketEvent;
use crate::transport::{SlackTransport, capped_backoff};

/// How long a `response_url` stays usable. A press older than this cannot be
/// answered, so processing it would spend a `fetch` and a draft update to
/// produce a write Slack will reject.
///
/// **This is not the queue's retention.** The topic keeps presses *longer*
/// (~35 minutes) on purpose: retention exists so a ten-minute outage does not
/// discard still-valid presses, and this check exists so the ones that did
/// expire are not worked on. Same number, opposite jobs.
const RESPONSE_URL_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Bound on the delivery-id set. Old entries fall out FIFO; a redelivery
/// arriving after this many newer records is caught by the orchestrator's
/// idempotent ingest instead — the same arrangement `mention.rs` relies on.
const SEEN_CAP: usize = 1024;

/// How long an ADC access token is reused before being fetched again.
///
/// `gcloud auth application-default print-access-token` prints **only the
/// token** — its documented default lifetime is 3600s, but the expiry is not
/// in the output, so this is a deliberately conservative fraction rather than
/// a value read from anywhere. A token rejected before then is discarded and
/// re-fetched, so being wrong here costs one retry, not a stall.
const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(50 * 60);

/// One message as Pub/Sub handed it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledMessage {
    /// Opaque handle used to acknowledge this delivery.
    pub ack_id: String,
    /// The message body: one [`GatewayRecord`] as JSON.
    pub data: String,
}

/// The Pub/Sub side, behind a seam so the drain loop is testable without a
/// network — the same arrangement [`SlackTransport`] provides for Slack.
pub trait PubSubTransport: Send + Sync {
    /// Ask `subscription` for up to `max_messages`. An empty vector is a
    /// normal answer, not an error.
    fn pull(
        &self,
        subscription: &str,
        max_messages: u32,
    ) -> impl Future<Output = Result<Vec<PulledMessage>, SlackError>> + Send;

    /// Acknowledge deliveries so they are not sent again.
    fn ack(
        &self,
        subscription: &str,
        ack_ids: &[String],
    ) -> impl Future<Output = Result<(), SlackError>> + Send;
}

/// Supplies the bearer token Pub/Sub is called with.
pub trait AccessTokens: Send + Sync {
    /// A currently-valid access token.
    fn token(&self) -> Result<String, SlackError>;
    /// Forget the cached token, so the next [`token`](Self::token) fetches a
    /// fresh one. Called when Pub/Sub rejects the credential.
    fn invalidate(&self);
}

/// Application Default Credentials, via the `gcloud` CLI.
///
/// Shelling out rather than linking a GCP SDK is the same trade
/// [ADR-0006](../../../ai-docs/decisions/adr-0006-onepassword-secret-backend.md)
/// made for 1Password: the credential already lives in a tool the operator has
/// configured, and reading it this way adds **no dependency** — which matters
/// because the alternative pulls a large auth stack into a plugin whose entire
/// job is a JSON round trip.
///
/// It also means **no service-account key is distributed** (decision 6): each
/// operator pulls with their own Google identity, and IAM grants them their own
/// subscription and nothing else.
pub struct AdcTokens {
    cached: Mutex<Option<(String, Instant)>>,
    ttl: Duration,
    /// How a fresh token is obtained. Injectable so the caching rules can be
    /// tested without a Google Cloud CLI, a network, or a real credential —
    /// none of which belong in a unit test.
    #[allow(clippy::type_complexity)]
    fetch: Box<dyn Fn() -> Result<String, SlackError> + Send + Sync>,
}

impl Default for AdcTokens {
    fn default() -> Self {
        Self {
            cached: Mutex::new(None),
            ttl: ACCESS_TOKEN_TTL,
            fetch: Box::new(gcloud_access_token),
        }
    }
}

impl AdcTokens {
    /// A token source with a different lifetime and fetcher. Tests only.
    pub fn with_fetcher(
        ttl: Duration,
        fetch: impl Fn() -> Result<String, SlackError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            cached: Mutex::new(None),
            ttl,
            fetch: Box::new(fetch),
        }
    }
}

impl AccessTokens for AdcTokens {
    fn token(&self) -> Result<String, SlackError> {
        if let Some((token, fetched)) = self.cached.lock().unwrap().as_ref()
            && fetched.elapsed() < self.ttl
        {
            return Ok(token.clone());
        }
        let token = (self.fetch)()?;
        *self.cached.lock().unwrap() = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn invalidate(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

/// One `gcloud auth application-default print-access-token` run.
fn gcloud_access_token() -> Result<String, SlackError> {
    let output = std::process::Command::new("gcloud")
        .args(["auth", "application-default", "print-access-token"])
        .output()
        .map_err(|e| {
            SlackError::InvalidRequest(format!(
                "could not run `gcloud auth application-default print-access-token` ({e}) → \
                 install the Google Cloud CLI and run `gcloud auth application-default \
                 login`, or switch to `event_source = \"socket\"`"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlackError::InvalidRequest(format!(
            "`gcloud auth application-default print-access-token` failed: {} → run `gcloud \
             auth application-default login`",
            stderr.trim()
        )));
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(SlackError::InvalidRequest(
            "`gcloud auth application-default print-access-token` printed nothing → run \
             `gcloud auth application-default login`"
                .into(),
        ));
    }
    Ok(token)
}

/// The production Pub/Sub transport: reqwest against the REST API.
pub struct ReqwestPubSub<A: AccessTokens> {
    client: reqwest::Client,
    base_url: String,
    tokens: A,
}

/// Ceiling on one Pub/Sub request.
///
/// **Without it the drain loop can park forever.** A `pull` whose connection
/// never answers is not an error reqwest reports, so the backoff and the
/// "queue is unreachable" warning below are never reached — the process looks
/// healthy and receives nothing, which is the exact failure mode this whole
/// design exists to remove. The Slack transport has had a bounded timeout for
/// the same reason since #104.
///
/// Generous, because an unset `returnImmediately` lets the server hold the
/// request open while it waits for a message.
const PUBSUB_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

impl<A: AccessTokens> ReqwestPubSub<A> {
    /// A transport against `base_url`, authenticating with `tokens`.
    pub fn new(base_url: &str, tokens: A) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(PUBSUB_REQUEST_TIMEOUT)
                .build()
                // Only fails if the TLS backend cannot be initialised, which
                // is the same condition that would fail `Client::new()`.
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens,
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, SlackError> {
        let url = format!("{}/v1/{path}", self.base_url);
        let mut refreshed = false;
        loop {
            let token = self.tokens.token()?;
            let response = self
                .client
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .map_err(|e| SlackError::Transport(e.to_string()))?;
            let status = response.status();
            // A rejected credential is worth exactly one retry: the cached
            // token may simply have outlived the conservative TTL above.
            // Retrying forever would turn "this identity has no access" into a
            // silent loop.
            if (status.as_u16() == 401 || status.as_u16() == 403) && !refreshed {
                self.tokens.invalidate();
                refreshed = true;
                continue;
            }
            let text = response
                .text()
                .await
                .map_err(|e| SlackError::Transport(e.to_string()))?;
            if !status.is_success() {
                return Err(SlackError::Http {
                    status: status.as_u16(),
                    body: text.chars().take(400).collect(),
                });
            }
            // An empty `pull` response body is `{}`, which serde reads fine;
            // a truly empty body is not JSON, so treat it as the same thing.
            if text.trim().is_empty() {
                return Ok(json!({}));
            }
            return serde_json::from_str(&text)
                .map_err(|e| SlackError::InvalidResponse(e.to_string()));
        }
    }
}

impl<A: AccessTokens> PubSubTransport for ReqwestPubSub<A> {
    async fn pull(
        &self,
        subscription: &str,
        max_messages: u32,
    ) -> Result<Vec<PulledMessage>, SlackError> {
        // `returnImmediately` is deliberately not set: unset lets the server
        // hold the request open when nothing is queued, which is the closest
        // this API gets to a push. It is documented as "may wait", though, so
        // the caller must not rely on it (see the module docs).
        let response = self
            .post(
                &format!("{subscription}:pull"),
                json!({ "maxMessages": max_messages }),
            )
            .await?;
        let Some(received) = response.get("receivedMessages").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        received
            .iter()
            .map(|entry| {
                let ack_id = entry
                    .get("ackId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SlackError::InvalidResponse("pull entry has no ackId".into()))?;
                let encoded = entry
                    .pointer("/message/data")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Ok(PulledMessage {
                    ack_id: ack_id.to_string(),
                    data: decode_base64(encoded)?,
                })
            })
            .collect()
    }

    async fn ack(&self, subscription: &str, ack_ids: &[String]) -> Result<(), SlackError> {
        if ack_ids.is_empty() {
            return Ok(());
        }
        self.post(
            &format!("{subscription}:acknowledge"),
            json!({ "ackIds": ack_ids }),
        )
        .await
        .map(|_| ())
    }
}

/// Decode Pub/Sub's base64 message payload.
///
/// Hand-rolled because the plugin has no base64 dependency and this is the
/// only place it would be used — adding a crate to the dependency graph of
/// every build for 20 lines used on one optional path is the worse trade.
/// Accepts both the standard and URL-safe alphabets; Pub/Sub emits standard,
/// but a hand-published test message may not.
fn decode_base64(encoded: &str) -> Result<String, SlackError> {
    let mut bits: u32 = 0;
    let mut width = 0;
    let mut out: Vec<u8> = Vec::with_capacity(encoded.len() * 3 / 4);
    for byte in encoded.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' => continue,
            other => {
                return Err(SlackError::InvalidResponse(format!(
                    "message data is not base64 (byte {other:#04x})"
                )));
            }
        };
        bits = (bits << 6) | u32::from(value);
        width += 6;
        if width >= 8 {
            width -= 8;
            out.push(((bits >> width) & 0xFF) as u8);
        }
    }
    String::from_utf8(out).map_err(|e| SlackError::InvalidResponse(e.to_string()))
}

/// Tuning knobs for the drain loop. [`Default`] is production; tests shrink
/// the delays.
#[derive(Debug, Clone)]
pub struct GatewayOptions {
    /// First delay after an empty pull; doubles per consecutive empty answer.
    pub backoff_base: Duration,
    /// Ceiling for that delay.
    pub backoff_max: Duration,
    /// Consecutive failures after which each further one is logged at `warn`
    /// rather than `info` — a persistent outage, not a blip.
    pub warn_after: u32,
}

impl Default for GatewayOptions {
    fn default() -> Self {
        Self {
            // Short, because an empty answer usually means `pull` returned
            // straight away rather than that nothing is happening.
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(20),
            warn_after: 5,
        }
    }
}

/// The window and volume a drain pass is allowed, resolved from config.
#[derive(Debug, Clone, Copy)]
struct DrainWindow {
    max_age: Duration,
    limit: u32,
}

impl DrainWindow {
    fn from_config(config: &SlackConfig) -> Self {
        Self {
            max_age: Duration::from_secs(
                config
                    .drain_max_age_hours
                    .unwrap_or(DEFAULT_DRAIN_MAX_AGE_HOURS)
                    .saturating_mul(3600),
            ),
            limit: config.drain_limit.unwrap_or(DEFAULT_DRAIN_LIMIT),
        }
    }

    /// Whether a record is recent enough to file.
    fn admits(&self, record: &GatewayRecord, now: SystemTime) -> bool {
        let Some(happened) = event_time(record) else {
            // An unparseable timestamp is not evidence of age. Dropping it
            // would be a false negative, which is the direction that loses a
            // mention; the pipeline's own filters still apply afterwards.
            return true;
        };
        now.duration_since(happened)
            .map(|age| age <= self.max_age)
            .unwrap_or(true)
    }
}

/// When the thing this record describes actually happened.
///
/// **Not `ts` for every kind, and the difference is not cosmetic.** `ts` is the
/// message a record is *about*, which for a reaction or a button press can be
/// arbitrarily old — reacting to a month-old note is a first-class way to open
/// a task, and a draft's buttons outlive the message they sit under. Judging
/// those by `ts` would discard them for the age of something nobody was
/// asking about.
///
/// | `kind` | clock |
/// |---|---|
/// | `message` | `ts` — Slack's, and the post *is* the event |
/// | `block_actions` | `action_ts` — Slack's, stamped at the press |
/// | `reaction` | `received_at` — the **gateway's** |
///
/// The reaction row is the compromise: Slack's `reaction_added` carries an
/// `event_ts`, but the frozen record does not (ADR-0072 decision 7 closes the
/// kind's extra fields at `reaction` / `item_user`), so the nearest available
/// stamp is when the gateway took delivery. It is within milliseconds of the
/// real thing unless the gateway's clock is wrong — and a gateway whose clock
/// is wrong can then widen or narrow this one window. That is a smaller
/// exposure than it sounds: the window only decides what to file after an
/// absence, and `reaction.rs` re-checks everything else.
fn event_time(record: &GatewayRecord) -> Option<SystemTime> {
    match record.kind {
        RecordKind::Message => slack_ts_to_system_time(&record.ts),
        RecordKind::BlockActions => record
            .action_ts
            .as_deref()
            .and_then(slack_ts_to_system_time),
        RecordKind::Reaction => rfc3339_to_system_time(&record.received_at),
    }
}

/// An RFC 3339 timestamp as a [`SystemTime`].
///
/// Hand-rolled for the same reason the base64 decoder is: this is the only
/// date the plugin parses. `None` for anything it cannot read, which
/// [`DrainWindow::admits`] treats as "no evidence of age" — the safe
/// direction, but it is also why the accepted shape has to be wide enough.
///
/// **Offsets are handled, not just `Z`.** This gateway always writes `Z`, but
/// the record may come from a replacement one (ADR-0072 decision 9), and a
/// perfectly valid `2026-09-13T09:00:00+09:00` returning `None` would let a
/// stale reaction slip past the age window after a long outage.
fn rfc3339_to_system_time(text: &str) -> Option<SystemTime> {
    let (date, rest) = text.split_once('T')?;
    // Split the offset off before parsing the clock. `+`/`-` cannot appear in
    // the time itself, and the search starts past the hour so a leading sign
    // (which RFC 3339 does not allow here anyway) cannot be mistaken for one.
    let (time, offset_secs) = match rest
        .char_indices()
        .find(|(i, c)| *i > 0 && (*c == '+' || *c == '-'))
    {
        Some((at, sign)) => {
            let (clock, offset) = rest.split_at(at);
            // `+09:00` and `+0900` are both legal. Splitting the second form
            // on the absent colon would read the whole `0900` as hours — a
            // 37-day offset, which a test caught.
            let body = &offset[1..];
            let (hours, minutes) = match body.split_once(':') {
                Some(pair) => pair,
                None if body.len() >= 4 => body.split_at(2),
                None => (body, "0"),
            };
            let magnitude = hours.parse::<i64>().ok()? * 3_600 + minutes.parse::<i64>().ok()? * 60;
            // A `+09:00` stamp is *earlier* in UTC than the same digits are,
            // so the offset is subtracted.
            (clock, if sign == '+' { -magnitude } else { magnitude })
        }
        None => (rest.trim_end_matches('Z'), 0),
    };
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since the epoch, by the civil-from-days algorithm (Howard Hinnant's
    // `days_from_civil`). Correct for every proleptic Gregorian date, which
    // matters more here than brevity: a leap-year slip would move the ingest
    // window by a day once every four years and never be noticed.
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second + offset_secs;
    u64::try_from(seconds)
        .ok()
        .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))
}

/// Slack's `"<seconds>.<microseconds>"` as a [`SystemTime`].
fn slack_ts_to_system_time(ts: &str) -> Option<SystemTime> {
    let (secs, micros) = ts.split_once('.').unwrap_or((ts, "0"));
    let secs: u64 = secs.parse().ok()?;
    let micros: u32 = format!("{micros:0<6}")[..6].parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_micros(micros.into()))
}

/// Bounded set of delivery ids already handled.
///
/// Split into a check and a record for the same reason `mention.rs` is: a
/// delivery whose handling failed transiently has **not** been handled, and
/// remembering it would make the redelivery — the thing that was supposed to
/// save it — a no-op.
#[derive(Default)]
struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// Whether `id` was already handled, without recording it.
    fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Record `id` as handled.
    fn remember(&mut self, id: String) {
        if !self.ids.insert(id.clone()) {
            return;
        }
        self.order.push_back(id);
        if self.order.len() > SEEN_CAP
            && let Some(evicted) = self.order.pop_front()
        {
            self.ids.remove(&evicted);
        }
    }
}

/// Turn a record into the event the pipeline already knows how to handle.
///
/// `Ok(None)` means "nothing to do with this one" — filtered out, expired, or
/// a message Slack can no longer find. None of those is an error: one
/// unreachable message must not take the source down.
async fn to_socket_event<T: SlackTransport>(
    api: &SlackApi<T>,
    record: &GatewayRecord,
    now: SystemTime,
) -> Result<Option<SocketEvent>, SlackError> {
    match record.kind {
        RecordKind::Message => {
            // The gateway's flags decide *what to fetch*; `mention.rs` decides
            // what it means. Both conditions are inside `wants_body`, and the
            // subteam half is load-bearing: a record naming only a group
            // leaves `mentions_me` false.
            if !record.wants_body() {
                return Ok(None);
            }
            let Some(message) = api.fetch_message(&record.channel, &record.ts).await? else {
                tracing::info!(
                    channel = %record.channel,
                    ts = %record.ts,
                    "the queued message is no longer reachable; dropping it"
                );
                return Ok(None);
            };
            // Rebuild the `message` event shape Socket Mode delivers, so the
            // filter table downstream reads exactly the fields it always has.
            let mut event = json!({
                "type": "message",
                "channel": record.channel,
                "ts": message.ts,
                "text": message.text,
            });
            for (key, value) in [
                ("user", message.user),
                ("thread_ts", message.thread_ts),
                ("subtype", message.subtype),
                ("bot_id", message.bot_id),
            ] {
                if let Some(value) = value {
                    event[key] = json!(value);
                }
            }
            Ok(Some(SocketEvent::Message(event)))
        }
        RecordKind::Reaction => Ok(Some(SocketEvent::Reaction(json!({
            "type": "reaction_added",
            "user": record.user,
            "reaction": record.reaction,
            "item": { "type": "message", "channel": record.channel, "ts": record.ts },
            "item_user": record.item_user,
        })))),
        RecordKind::BlockActions => {
            // Expiry is judged here rather than by the queue's retention: the
            // queue's job is not to lose a still-valid press, this check's job
            // is not to work on a dead one.
            if let Some(pressed) = record
                .action_ts
                .as_deref()
                .and_then(slack_ts_to_system_time)
                && now
                    .duration_since(pressed)
                    .is_ok_and(|age| age > RESPONSE_URL_LIFETIME)
            {
                tracing::info!(
                    action_id = record.action_id.as_deref().unwrap_or_default(),
                    "a queued button press outlived its response_url; dropping it"
                );
                return Ok(None);
            }
            Ok(record
                .block_actions_payload()
                .map(SocketEvent::BlockActions))
        }
    }
}

/// One `pull` against each subscription, to fail startup on a credential,
/// permission or naming mistake instead of running silently.
///
/// **This is the gateway's answer to the `apps.connections.open` probe.**
/// `token_guard` opens a Socket Mode connection at `initialize` for one
/// reason: without it a bad App-Level Token surfaces only inside a background
/// loop, so `totsuka doctor` reports the plugin healthy while it can never
/// receive an event. A wrong ADC identity, a missing `roles/pubsub.subscriber`
/// or a typo in a subscription name fails in exactly that shape, so it gets
/// exactly that treatment.
///
/// An empty answer is a **pass** — it means the credential worked and there is
/// nothing queued, which is the normal state of an idle subscription.
/// Anything pulled here is left unacked and redelivered.
pub async fn probe<P: PubSubTransport>(
    pubsub: &P,
    gateway: &GatewayConfig,
) -> Result<(), SlackError> {
    for subscription in [gateway.events_path(), gateway.block_actions_path()] {
        pubsub.pull(&subscription, 1).await.map_err(|e| {
            SlackError::InvalidRequest(format!(
                concat!(
                    "could not read the Event Gateway queue `{}`: {} → check that ",
                    "`[slack.gateway]` names the right project and subscriptions, that ",
                    "`gcloud auth application-default login` has been run as the account ",
                    "holding `roles/pubsub.subscriber` on it, and that the subscription exists",
                ),
                subscription, e
            ))
        })?;
    }
    Ok(())
}

/// Drain both subscriptions until the receiver is dropped, emitting
/// [`SocketEvent`]s the mention pipeline consumes.
///
/// Returns the same `(events, handle)` pair
/// [`socket_mode::spawn`](crate::socket_mode::spawn) does, which is what lets
/// `server.rs` choose between them with nothing downstream changing.
pub fn spawn<T, P>(
    api: Arc<SlackApi<T>>,
    config: Arc<SlackConfig>,
    gateway: Arc<GatewayConfig>,
    pubsub: Arc<P>,
    options: GatewayOptions,
) -> (
    mpsc::UnboundedReceiver<SocketEvent>,
    tokio::task::JoinHandle<()>,
)
where
    T: SlackTransport + 'static,
    P: PubSubTransport + 'static,
{
    // Unbounded for the same reason Socket Mode's is: a slow consumer must
    // never park the loop that is acknowledging deliveries.
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        let events = drain_forever(
            Arc::clone(&api),
            Arc::clone(&config),
            Arc::clone(&pubsub),
            gateway.events_path(),
            tx.clone(),
            options.clone(),
        );
        let presses = drain_forever(
            api,
            config,
            pubsub,
            gateway.block_actions_path(),
            tx,
            options,
        );
        tokio::join!(events, presses);
    });
    (rx, handle)
}

/// One subscription's drain loop.
async fn drain_forever<T, P>(
    api: Arc<SlackApi<T>>,
    config: Arc<SlackConfig>,
    pubsub: Arc<P>,
    subscription: String,
    tx: mpsc::UnboundedSender<SocketEvent>,
    options: GatewayOptions,
) where
    T: SlackTransport,
    P: PubSubTransport,
{
    let window = DrainWindow::from_config(&config);
    let max_messages = config
        .gateway
        .as_ref()
        .map(|g| g.pull_max_messages)
        .unwrap_or(50);
    let mut seen = Seen::default();
    // Two counters, not one. An idle subscription answering empty is normal
    // and an unreachable one is not, so sharing a counter would let a quiet
    // afternoon push the *first* transient error straight past `warn_after`
    // — reporting a persistent outage on a blip, and waiting `backoff_max`
    // before retrying it. `socket_mode.rs` had to learn the same thing about
    // its `hello`-only advisory (#641): measuring a harmless state and a
    // broken one on the same clock hides the broken one.
    let mut failures: u32 = 0;
    let mut empty_polls: u32 = 0;
    loop {
        let pulled = match pubsub.pull(&subscription, max_messages).await {
            Ok(pulled) => pulled,
            Err(e) => {
                failures = failures.saturating_add(1);
                let delay = capped_backoff(options.backoff_base, options.backoff_max, failures - 1);
                if failures >= options.warn_after {
                    tracing::warn!(
                        subscription = %subscription, error = %e, attempt = failures,
                        "the Event Gateway's queue is unreachable; Slack events are queued but \
                         not being collected. There is no automatic fall back to Socket Mode — \
                         the two are mutually exclusive in the Slack app"
                    );
                } else {
                    tracing::info!(
                        subscription = %subscription, error = %e,
                        "pull failed; retrying"
                    );
                }
                tokio::time::sleep(delay).await;
                continue;
            }
        };
        failures = 0;
        if pulled.is_empty() {
            // `pull` is documented as *may* wait, so an immediate empty answer
            // is legal and a tight loop around it would spin a CPU.
            let delay = capped_backoff(
                options.backoff_base,
                options.backoff_max,
                empty_polls.min(8),
            );
            empty_polls = empty_polls.saturating_add(1);
            tokio::time::sleep(delay).await;
            continue;
        }
        empty_polls = 0;

        let now = SystemTime::now();
        let mut filed = 0u32;
        // Acknowledged: everything this pass reached a **decision** about,
        // including the records it deliberately dropped — a record this build
        // decided against will be decided against again, so leaving it queued
        // only means deciding again forever.
        //
        // **Not** acknowledged: a record whose rebuild failed for a reason
        // outside this process. Slack running out of rate limit, or answering
        // 5xx, is not a verdict on the mention; acking it would delete the
        // mention on the strength of a transient error and leave one `warn`
        // line behind. Redelivery is the retry, which is why `seen` is
        // recorded only once the record has actually been dealt with.
        let mut ack_ids = Vec::with_capacity(pulled.len());
        for message in pulled {
            let record = match GatewayRecord::parse(&message.data) {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!(
                        subscription = %subscription, error = %e,
                        "dropping a queued record this build cannot read"
                    );
                    ack_ids.push(message.ack_id);
                    continue;
                }
            };
            let delivery_id = record.delivery_id();
            if seen.contains(&delivery_id) {
                ack_ids.push(message.ack_id);
                continue;
            }
            if !window.admits(&record, now) {
                tracing::info!(
                    channel = %record.channel, ts = %record.ts, kind = ?record.kind,
                    "a queued event is older than `drain_max_age_hours`; dropping it"
                );
                seen.remember(delivery_id);
                ack_ids.push(message.ack_id);
                continue;
            }
            if filed >= window.limit {
                // Over the per-pass budget: **left unacked**, so the next pass
                // picks it up.
                //
                // This used to ack and drop, on the reasoning that the drain
                // window is a policy about what to file. That conflated two
                // different settings. `drain_max_age_hours` *is* such a policy
                // — an event outside it will never be filed, so holding it
                // costs nothing. `drain_limit` is pacing: these events are
                // inside the window and would be filed, just not right now.
                // Acking them turned a small configured limit into silent loss
                // of mentions and button presses.
                //
                // Not remembered in `seen` either, for the same reason: this
                // delivery has not been handled.
                tracing::info!(
                    subscription = %subscription, limit = window.limit,
                    "drain limit reached for this pass; leaving the remainder \
                     queued for the next one"
                );
                continue;
            }
            match to_socket_event(api.as_ref(), &record, now).await {
                Ok(Some(event)) => {
                    filed += 1;
                    seen.remember(delivery_id);
                    ack_ids.push(message.ack_id);
                    if tx.send(event).is_err() {
                        // The pipeline is gone; ack what we have and stop.
                        let _ = pubsub.ack(&subscription, &ack_ids).await;
                        return;
                    }
                }
                // A definitive "nothing to do": filtered out, or a message
                // Slack says does not exist. Settled, so it is acked.
                Ok(None) => {
                    seen.remember(delivery_id);
                    ack_ids.push(message.ack_id);
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %record.channel, ts = %record.ts, error = %e,
                        "could not reach Slack to rebuild a queued event; leaving it queued \
                         for redelivery rather than dropping the event"
                    );
                }
            }
        }
        if let Err(e) = pubsub.ack(&subscription, &ack_ids).await {
            // Not fatal: unacked messages are redelivered, and `seen` drops
            // the repeats. Worth saying out loud because sustained ack
            // failures mean the queue keeps growing.
            tracing::warn!(
                subscription = %subscription, error = %e,
                "could not acknowledge deliveries; they will be redelivered"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_both_alphabets() {
        // `{"v":1}` in the standard alphabet, with padding.
        assert_eq!(decode_base64("eyJ2IjoxfQ==").unwrap(), r#"{"v":1}"#);
        // URL-safe input decodes the same way.
        assert_eq!(decode_base64("eyJ2IjoxfQ").unwrap(), r#"{"v":1}"#);
        assert!(decode_base64("not base64!").is_err());
    }

    #[test]
    fn slack_timestamps_parse_to_the_right_instant() {
        let at = slack_ts_to_system_time("1757640000.000100").expect("parses");
        assert_eq!(
            at.duration_since(SystemTime::UNIX_EPOCH).unwrap(),
            Duration::from_secs(1_757_640_000) + Duration::from_micros(100)
        );
        assert!(slack_ts_to_system_time("not-a-timestamp").is_none());
    }

    fn record(
        kind: RecordKind,
        ts: &str,
        action_ts: Option<&str>,
        received: &str,
    ) -> GatewayRecord {
        GatewayRecord {
            v: 1,
            kind,
            channel: "C1".into(),
            ts: ts.into(),
            thread_ts: None,
            user: "U_ME".into(),
            flags: crate::gateway_contract::Flags { mentions_me: true },
            subteam_ids: Vec::new(),
            received_at: received.into(),
            reaction: (kind == RecordKind::Reaction).then(|| "eyes".to_string()),
            item_user: None,
            action_id: (kind == RecordKind::BlockActions).then(|| "approve_reply".to_string()),
            value: None,
            response_url: None,
            container_channel: (kind == RecordKind::BlockActions).then(|| "C1".to_string()),
            action_ts: action_ts.map(str::to_string),
        }
    }

    #[test]
    fn the_drain_window_keeps_recent_events_and_drops_old_ones() {
        let window = DrainWindow {
            max_age: Duration::from_secs(3600),
            limit: 10,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_757_640_000);
        let message = |ts: &str| record(RecordKind::Message, ts, None, "2026-09-13T00:00:00Z");
        assert!(
            window.admits(&message("1757639000.000000"), now),
            "17 min old"
        );
        assert!(
            !window.admits(&message("1757600000.000000"), now),
            "11 h old"
        );
        // A timestamp from the future is not old; clocks disagree.
        assert!(window.admits(&message("1757650000.000000"), now));
        // An unparseable timestamp must not be treated as expired — that
        // would be the error direction that loses a mention.
        assert!(window.admits(&message("garbage"), now));
    }

    /// A press on an old message, and a reaction on an old message, are both
    /// *recent events*. Judging them by the message's `ts` would discard the
    /// most ordinary way either one is used.
    #[test]
    fn the_window_judges_the_event_not_the_message_it_points_at() {
        let window = DrainWindow {
            max_age: Duration::from_secs(3600),
            limit: 10,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_757_640_000);
        let ancient = "1700000000.000000";

        let press = record(
            RecordKind::BlockActions,
            ancient,
            Some("1757639900.000000"),
            "2026-09-13T00:00:00Z",
        );
        assert!(
            window.admits(&press, now),
            "a button pressed 100 seconds ago on a two-year-old message is not old"
        );

        // `now` is 2025-09-12T01:20:00Z, so this one is a day stale.
        let stale_reaction = record(RecordKind::Reaction, ancient, None, "2025-09-11T00:00:00Z");
        assert!(!window.admits(&stale_reaction, now));
        let fresh_reaction = record(RecordKind::Reaction, ancient, None, "2025-09-12T01:00:00Z");
        assert!(
            window.admits(&fresh_reaction, now),
            "a reaction added minutes ago on an ancient message is not old"
        );
    }

    #[test]
    fn rfc3339_timestamps_parse_including_leap_years() {
        let at = |text: &str| {
            rfc3339_to_system_time(text)
                .expect("parses")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs()
        };
        assert_eq!(at("1970-01-01T00:00:00Z"), 0);
        assert_eq!(at("2026-09-13T00:00:00Z"), 1_789_257_600);
        // 2024 is a leap year; 1900 was not, and 2000 was.
        assert_eq!(
            at("2024-03-01T00:00:00Z") - at("2024-02-28T00:00:00Z"),
            2 * 86_400
        );
        assert_eq!(
            at("2000-03-01T00:00:00Z") - at("2000-02-28T00:00:00Z"),
            2 * 86_400
        );
        assert_eq!(
            at("2023-03-01T00:00:00Z") - at("2023-02-28T00:00:00Z"),
            86_400
        );
        assert!(rfc3339_to_system_time("not a date").is_none());
        assert!(rfc3339_to_system_time("2026-13-01T00:00:00Z").is_none());
    }

    /// A replacement gateway may stamp an offset rather than `Z`. Returning
    /// `None` there would read as "no evidence of age", which lets a stale
    /// reaction through the window after a long outage.
    #[test]
    fn rfc3339_offsets_resolve_to_the_same_instant_as_z() {
        let at = |text: &str| {
            rfc3339_to_system_time(text)
                .unwrap_or_else(|| panic!("`{text}` must parse"))
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs()
        };
        // Same instant, three spellings.
        assert_eq!(at("2026-09-13T09:00:00+09:00"), at("2026-09-13T00:00:00Z"));
        assert_eq!(at("2026-09-12T19:00:00-05:00"), at("2026-09-13T00:00:00Z"));
        // Offsets with no colon, and a half-hour offset.
        assert_eq!(at("2026-09-13T09:00:00+0900"), at("2026-09-13T00:00:00Z"));
        assert_eq!(at("2026-09-13T05:30:00+05:30"), at("2026-09-13T00:00:00Z"));
        // Fractional seconds are still tolerated alongside an offset.
        assert_eq!(
            at("2026-09-13T09:00:00.123+09:00"),
            at("2026-09-13T00:00:00Z")
        );
    }

    #[test]
    fn a_delivery_is_only_handled_once() {
        let mut seen = Seen::default();
        assert!(!seen.contains("message:C1:1.0"));
        seen.remember("message:C1:1.0".into());
        assert!(seen.contains("message:C1:1.0"));
        assert!(!seen.contains("message:C1:2.0"));
        // The set is bounded; the oldest entry falls out, and the
        // orchestrator's idempotent ingest catches a redelivery after that.
        for n in 0..SEEN_CAP {
            seen.remember(format!("message:C1:filler-{n}"));
        }
        assert!(!seen.contains("message:C1:1.0"));
    }
}
