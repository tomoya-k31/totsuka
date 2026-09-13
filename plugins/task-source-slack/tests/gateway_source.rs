//! The Pub/Sub source (#657): queued coordinates become the same
//! `SocketEvent`s Socket Mode produces, and the rules about what is *not*
//! turned into one hold.
//!
//! The Pub/Sub side is a fake, so these run with no network and no GCP
//! project — the same arrangement the Slack transport has had since #104.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use task_source_slack::config::{GatewayConfig, SlackConfig};
use task_source_slack::error::SlackError;
use task_source_slack::gateway::{
    AccessTokens, AdcTokens, GatewayOptions, PubSubTransport, PulledMessage, spawn,
};
use task_source_slack::gateway_contract::{Flags, GatewayRecord, RecordKind};
use task_source_slack::slack_api::SlackApi;
use task_source_slack::socket_mode::SocketEvent;

use common::{Canned, Shared, transport};

const ME: &str = "U_ME";
const CHANNEL: &str = "C0LOBBY";

/// A queue the drain loop reads, plus a record of what it acknowledged.
#[derive(Clone, Default)]
struct FakePubSub {
    /// One entry per `pull` call, in order. An exhausted queue answers empty
    /// forever, which is the shape a real idle subscription has.
    batches: Arc<Mutex<Vec<Vec<PulledMessage>>>>,
    pulls: Arc<AtomicUsize>,
    acked: Arc<Mutex<Vec<String>>>,
}

impl FakePubSub {
    fn with_records(records: &[GatewayRecord]) -> Self {
        let batch = records
            .iter()
            .enumerate()
            .map(|(i, record)| PulledMessage {
                ack_id: format!("ack-{i}"),
                data: serde_json::to_string(record).expect("record serializes"),
            })
            .collect();
        Self {
            batches: Arc::new(Mutex::new(vec![batch])),
            ..Self::default()
        }
    }

    fn acked(&self) -> Vec<String> {
        self.acked.lock().unwrap().clone()
    }

    fn pulls(&self) -> usize {
        self.pulls.load(Ordering::SeqCst)
    }
}

impl PubSubTransport for FakePubSub {
    async fn pull(
        &self,
        _subscription: &str,
        _max_messages: u32,
    ) -> Result<Vec<PulledMessage>, SlackError> {
        self.pulls.fetch_add(1, Ordering::SeqCst);
        let mut batches = self.batches.lock().unwrap();
        if batches.is_empty() {
            return Ok(Vec::new());
        }
        Ok(batches.remove(0))
    }

    async fn ack(&self, _subscription: &str, ack_ids: &[String]) -> Result<(), SlackError> {
        self.acked.lock().unwrap().extend_from_slice(ack_ids);
        Ok(())
    }
}

fn gateway_config() -> SlackConfig {
    serde_json::from_value(json!({
        "user_token": "xoxp-1",
        "target_user_id": ME,
        "event_source": "gateway",
        "gateway": {
            "project": "p",
            "subscription": "events",
            "block_actions_subscription": "presses",
        },
    }))
    .expect("gateway config parses")
}

fn message_record(ts: &str, mentions_me: bool, subteams: &[&str]) -> GatewayRecord {
    GatewayRecord {
        v: 1,
        kind: RecordKind::Message,
        channel: CHANNEL.into(),
        ts: ts.into(),
        thread_ts: None,
        user: "U_SENDER".into(),
        flags: Flags { mentions_me },
        subteam_ids: subteams.iter().map(|s| (*s).to_string()).collect(),
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

fn press_record(action_ts: &str) -> GatewayRecord {
    GatewayRecord {
        kind: RecordKind::BlockActions,
        channel: "D0SELFDM".into(),
        ts: "1757640100.000200".into(),
        user: ME.into(),
        flags: Flags { mentions_me: false },
        action_id: Some("approve_reply".into()),
        value: Some(r#"{"d":"draft-1"}"#.into()),
        response_url: Some("https://hooks.slack.com/actions/1".into()),
        container_channel: Some("D0SELFDM".into()),
        action_ts: Some(action_ts.into()),
        ..message_record("1757640100.000200", false, &[])
    }
}

/// A Slack `ts` `seconds` ago.
fn ts_ago(seconds: u64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("after the epoch");
    format!("{}.000000", now.as_secs().saturating_sub(seconds))
}

fn history_reply(ts: &str, text: &str) -> Canned {
    Canned::Data(json!({
        "ok": true,
        "messages": [{ "user": "U_SENDER", "text": text, "ts": ts }],
    }))
}

/// Run the drain loop until it produces `expected` events or the deadline
/// passes. Returns what arrived, so an assertion can name the shortfall.
async fn collect(
    shared: &Shared,
    config: SlackConfig,
    pubsub: Arc<FakePubSub>,
    expected: usize,
) -> Vec<SocketEvent> {
    let api = Arc::new(SlackApi::new(transport(shared)));
    let gateway = Arc::new(config.gateway.clone().expect("gateway table"));
    let (mut events, handle) = spawn(
        api,
        Arc::new(config),
        gateway,
        pubsub,
        GatewayOptions {
            backoff_base: Duration::from_millis(1),
            backoff_max: Duration::from_millis(5),
            warn_after: 100,
        },
    );
    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while got.len() < expected {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => got.push(event),
            _ => break,
        }
    }
    handle.abort();
    got
}

/// The happy path: a queued mention comes back as the `message` event the
/// pipeline has always consumed, body re-fetched with totsuka's own token.
#[tokio::test]
async fn a_queued_mention_is_rebuilt_into_a_message_event() {
    let shared = Shared::default();
    let ts = ts_ago(60);
    shared.push_for(
        "conversations.history",
        history_reply(&ts, "<@U_ME> このリポジトリを見てほしい"),
    );
    let pubsub = Arc::new(FakePubSub::with_records(&[message_record(&ts, true, &[])]));

    let events = collect(&shared, gateway_config(), Arc::clone(&pubsub), 1).await;
    assert_eq!(events.len(), 1, "expected one rebuilt event");
    let SocketEvent::Message(event) = &events[0] else {
        panic!("expected a message event, got {:?}", events[0]);
    };
    assert_eq!(event.get("type").and_then(Value::as_str), Some("message"));
    assert_eq!(event.get("channel").and_then(Value::as_str), Some(CHANNEL));
    assert_eq!(
        event.get("text").and_then(Value::as_str),
        Some("<@U_ME> このリポジトリを見てほしい"),
        "the body comes from Slack, never from the queue"
    );
    assert_eq!(pubsub.acked(), vec!["ack-0".to_string()]);
}

/// A record naming only a user group still has to be fetched. Its
/// `mentions_me` is false — membership is resolved in totsuka, not at the
/// edge — so fetching on that flag alone drops every group mention silently.
#[tokio::test]
async fn a_subteam_only_record_is_still_fetched() {
    let shared = Shared::default();
    let ts = ts_ago(60);
    shared.push_for(
        "conversations.history",
        history_reply(&ts, "<!subteam^S0ABCDEF> 障害対応お願いします"),
    );
    let pubsub = Arc::new(FakePubSub::with_records(&[message_record(
        &ts,
        false,
        &["S0ABCDEF"],
    )]));

    let events = collect(&shared, gateway_config(), pubsub, 1).await;
    assert_eq!(
        events.len(),
        1,
        "a subteam-only record must be fetched, or group mentions vanish"
    );
    let fetched = shared
        .requests()
        .iter()
        .any(|r| r.method == "conversations.history");
    assert!(fetched, "the body was never re-fetched");
}

/// A record with neither flag is not worth a round trip: the gateway already
/// said it names nobody.
#[tokio::test]
async fn a_record_naming_nobody_costs_no_api_call() {
    let shared = Shared::default();
    let ts = ts_ago(60);
    let pubsub = Arc::new(FakePubSub::with_records(&[message_record(&ts, false, &[])]));

    let events = collect(&shared, gateway_config(), Arc::clone(&pubsub), 1).await;
    assert!(events.is_empty(), "nothing should have been emitted");
    assert!(
        shared.requests().is_empty(),
        "no Slack call should have been made"
    );
    assert_eq!(
        pubsub.acked(),
        vec!["ack-0".to_string()],
        "a dropped record must still be acked, or Pub/Sub redelivers it forever"
    );
}

/// The ingest window is totsuka's policy, applied to Slack's own timestamp.
#[tokio::test]
async fn an_event_older_than_the_window_is_acked_and_dropped() {
    let shared = Shared::default();
    let mut config = gateway_config();
    config.drain_max_age_hours = Some(1);
    let old = ts_ago(4 * 3600);
    let pubsub = Arc::new(FakePubSub::with_records(&[message_record(&old, true, &[])]));

    let events = collect(&shared, config, Arc::clone(&pubsub), 1).await;
    assert!(events.is_empty(), "a four-hour-old event must not be filed");
    assert!(
        shared.requests().is_empty(),
        "the window is applied before paying for a fetch"
    );
    assert_eq!(pubsub.acked(), vec!["ack-0".to_string()]);
}

/// Expiry is judged here, not by the queue's retention — the queue keeps
/// presses *longer* precisely so a short outage does not discard valid ones.
#[tokio::test]
async fn a_press_that_outlived_its_response_url_is_dropped() {
    let shared = Shared::default();
    let pubsub = Arc::new(FakePubSub::with_records(&[
        press_record(&ts_ago(45 * 60)),
        press_record(&ts_ago(60)),
    ]));

    let events = collect(&shared, gateway_config(), Arc::clone(&pubsub), 2).await;
    assert_eq!(events.len(), 1, "only the still-answerable press survives");
    assert!(matches!(events[0], SocketEvent::BlockActions(_)));
    assert_eq!(
        pubsub.acked(),
        vec!["ack-0".to_string(), "ack-1".to_string()],
        "both are acked; only one is acted on"
    );
}

/// Pub/Sub delivers at least once, and so does Slack. The same happening
/// arriving twice must cost one task, not two.
#[tokio::test]
async fn a_redelivered_record_is_handled_once() {
    let shared = Shared::default();
    let ts = ts_ago(60);
    for _ in 0..2 {
        shared.push_for("conversations.history", history_reply(&ts, "<@U_ME> hi"));
    }
    let record = message_record(&ts, true, &[]);
    let pubsub = Arc::new(FakePubSub::with_records(&[record.clone(), record]));

    let events = collect(&shared, gateway_config(), Arc::clone(&pubsub), 2).await;
    assert_eq!(
        events.len(),
        1,
        "the second delivery of `{}` must be dropped",
        message_record(&ts, true, &[]).delivery_id()
    );
    assert_eq!(
        pubsub.acked(),
        vec!["ack-0".to_string(), "ack-1".to_string()]
    );
}

/// A Slack outage is not a verdict on the mention. Acking a record whose
/// rebuild failed transiently would delete the event on the strength of a
/// temporary error, leaving one `warn` line where a task should have been.
#[tokio::test]
async fn a_transient_slack_failure_leaves_the_record_queued() {
    let shared = Shared::default();
    let ts = ts_ago(60);
    // First pull: Slack is unreachable. Second: it answers, from the queue's
    // redelivery of the very same message.
    shared.push_for("conversations.history", Canned::Network);
    shared.push_for("conversations.history", history_reply(&ts, "<@U_ME> hi"));
    let record = message_record(&ts, true, &[]);
    let batch = vec![PulledMessage {
        ack_id: "ack-0".into(),
        data: serde_json::to_string(&record).expect("record serializes"),
    }];
    let pubsub = Arc::new(FakePubSub {
        batches: Arc::new(Mutex::new(vec![batch.clone(), batch])),
        ..FakePubSub::default()
    });

    let events = collect(&shared, gateway_config(), Arc::clone(&pubsub), 1).await;
    assert_eq!(
        events.len(),
        1,
        "the redelivery must produce the event the failed attempt could not"
    );
    assert_eq!(
        pubsub.acked(),
        vec!["ack-0".to_string()],
        "the failed attempt must not have acked; only the successful one does"
    );
}

/// `drain_limit` paces the work; it does not decide what is worth filing.
/// Acking the overflow would turn a small configured limit into silent loss of
/// mentions — the events are inside the age window and *would* be filed.
#[tokio::test]
async fn events_over_the_drain_limit_stay_queued() {
    let shared = Shared::default();
    // Three distinct records, all inside the age window, and a limit of one.
    let stamps: Vec<String> = [60_u64, 120, 180].iter().map(|ago| ts_ago(*ago)).collect();
    for stamp in &stamps {
        shared.push_for("conversations.history", history_reply(stamp, "<@U_ME> hi"));
    }
    let records: Vec<GatewayRecord> = stamps
        .iter()
        .map(|ts| message_record(ts, true, &[]))
        .collect();
    let mut config = gateway_config();
    config.drain_limit = Some(1);
    let pubsub = Arc::new(FakePubSub::with_records(&records));

    let events = collect(&shared, config, Arc::clone(&pubsub), 2).await;
    assert_eq!(events.len(), 1, "the limit paces the pass");
    assert_eq!(
        pubsub.acked(),
        vec!["ack-0".to_string()],
        "only the filed record is acked; the other two must be redelivered, not destroyed"
    );
}

/// `pull` is specified as *may* wait, so an immediately-empty answer is legal
/// — and a loop that does not back off around it spins a core.
#[tokio::test]
async fn an_always_empty_subscription_does_not_busy_loop() {
    let shared = Shared::default();
    let pubsub = Arc::new(FakePubSub::default());
    let api = Arc::new(SlackApi::new(transport(&shared)));
    let config = gateway_config();
    let gateway = Arc::new(config.gateway.clone().expect("gateway table"));
    let (_events, handle) = spawn(
        api,
        Arc::new(config),
        gateway,
        Arc::clone(&pubsub),
        GatewayOptions {
            backoff_base: Duration::from_millis(20),
            backoff_max: Duration::from_millis(40),
            warn_after: 100,
        },
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.abort();

    let pulls = pubsub.pulls();
    // Two subscriptions are drained, so the ceiling is per-loop × 2 plus
    // slack. Without a backoff this would be in the tens of thousands.
    assert!(
        pulls <= 80,
        "an empty subscription was polled {pulls} times in 300ms — the backoff is not effective"
    );
    assert!(pulls >= 2, "each subscription should have been polled");
}

/// A credential or naming mistake has to fail startup, not leave a healthy
/// -looking plugin that never receives anything.
#[tokio::test]
async fn the_startup_probe_reports_an_unreadable_queue() {
    /// A subscription that refuses every pull, the way a missing
    /// `roles/pubsub.subscriber` or a mistyped name does.
    struct Refusing;
    impl PubSubTransport for Refusing {
        async fn pull(&self, _: &str, _: u32) -> Result<Vec<PulledMessage>, SlackError> {
            Err(SlackError::Http {
                status: 403,
                body: "permission denied".into(),
            })
        }
        async fn ack(&self, _: &str, _: &[String]) -> Result<(), SlackError> {
            Ok(())
        }
    }

    let config = gateway_config();
    let gateway = config.gateway.clone().expect("gateway table");
    let error = task_source_slack::gateway::probe(&Refusing, &gateway)
        .await
        .expect_err("a refused pull must fail the probe");
    let text = error.to_string();
    assert!(
        text.contains("projects/p/subscriptions/events"),
        "the message must name the subscription, got {text}"
    );
    assert!(
        text.contains("pubsub.subscriber"),
        "the message must say what to check, got {text}"
    );

    // An empty answer is a pass: an idle subscription is the normal state.
    assert!(
        task_source_slack::gateway::probe(&FakePubSub::default(), &gateway)
            .await
            .is_ok()
    );
}

/// The token is reused until it expires, then fetched again. Being wrong
/// about the lifetime costs one retry — never a stall — so the cache is
/// deliberately conservative rather than clever.
#[test]
fn the_access_token_is_cached_until_it_expires() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fetches);
    let tokens = AdcTokens::with_fetcher(Duration::from_millis(80), move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        Ok(format!("token-{n}"))
    });

    assert_eq!(tokens.token().unwrap(), "token-0");
    assert_eq!(tokens.token().unwrap(), "token-0", "reused inside the TTL");
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        tokens.token().unwrap(),
        "token-1",
        "re-fetched after the TTL"
    );

    // A rejected credential drops the cache immediately rather than waiting
    // the TTL out: the token may have been revoked, not merely expired.
    tokens.invalidate();
    assert_eq!(tokens.token().unwrap(), "token-2");
}

/// `event_source = "socket"` keeps `app_token` mandatory; `gateway` does not.
#[test]
fn the_app_token_requirement_follows_the_source() {
    let config = |source: &str, app_token: Option<&str>| -> Vec<String> {
        let mut raw = json!({
            "user_token": "xoxp-1",
            "target_user_id": ME,
            "event_source": source,
        });
        if let Some(token) = app_token {
            raw["app_token"] = json!(token);
        }
        if source == "gateway" {
            raw["gateway"] = json!({
                "project": "p",
                "subscription": "events",
                "block_actions_subscription": "presses",
            });
        }
        task_source_slack::config::static_config_errors(
            &serde_json::from_value(raw).expect("config parses"),
        )
    };

    let missing = config("socket", None);
    assert!(
        missing.iter().any(|e| e.contains("app_token")),
        "Socket Mode cannot connect without it, got {missing:?}"
    );
    assert!(config("socket", Some("xapp-1")).is_empty());
    assert!(
        config("gateway", None).is_empty(),
        "the gateway opens no WebSocket, so the token has no use"
    );
    // Still checked when present: a wrong value is a mistake either way.
    assert!(
        config("gateway", Some("xoxb-wrong"))
            .iter()
            .any(|e| e.contains("app_token"))
    );
}

/// The queue names are required, and the two subscriptions must differ —
/// one subscription for both kinds would put presses under a retention
/// measured in days and have the two drain loops race for them.
#[test]
fn the_gateway_table_is_checked() {
    let with = |gateway: Value| -> Vec<String> {
        let raw = json!({
            "user_token": "xoxp-1",
            "target_user_id": ME,
            "event_source": "gateway",
            "gateway": gateway,
        });
        task_source_slack::config::static_config_errors(
            &serde_json::from_value(raw).expect("config parses"),
        )
    };

    let same = with(json!({
        "project": "p", "subscription": "one", "block_actions_subscription": "one",
    }));
    assert!(
        same.iter()
            .any(|e| e.contains("block_actions_subscription")),
        "one subscription for both kinds must be refused, got {same:?}"
    );

    let blank = with(json!({
        "project": "  ", "subscription": "events", "block_actions_subscription": "presses",
    }));
    assert!(blank.iter().any(|e| e.contains("project")));

    let missing_table: Vec<String> = task_source_slack::config::static_config_errors(
        &serde_json::from_value(json!({
            "user_token": "xoxp-1",
            "target_user_id": ME,
            "event_source": "gateway",
        }))
        .expect("config parses"),
    );
    assert!(
        missing_table.iter().any(|e| e.contains("[slack.gateway]")),
        "a gateway source with no queue names must not start, got {missing_table:?}"
    );
}

/// The paths the drain loop pulls from.
#[test]
fn subscription_paths_are_fully_qualified() {
    let gateway: GatewayConfig = serde_json::from_value(json!({
        "project": "my-project",
        "subscription": "totsuka-me-events",
        "block_actions_subscription": "totsuka-me-presses",
    }))
    .expect("gateway config parses");
    assert_eq!(
        gateway.events_path(),
        "projects/my-project/subscriptions/totsuka-me-events"
    );
    assert_eq!(
        gateway.block_actions_path(),
        "projects/my-project/subscriptions/totsuka-me-presses"
    );
}
