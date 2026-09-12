//! The gate itself (#659, ADR-0072 decision 11).
//!
//! Slack cannot be an IAM principal and publishes no stable source-IP list, so
//! there is no layer in front of this container. Three things stand between it
//! and the open internet — an unguessable path, the signature, and the
//! timestamp window — and each is checked here rather than merely asserted in
//! a comment.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode};
use serde_json::{Value, json};

use slack_event_gateway::http::{Gateway, handle};
use slack_event_gateway::publish::{PublishError, Publisher};
use slack_event_gateway::registry::Registry;
use slack_event_gateway::signature::sign;

const SECRET: &str = "8f742231b10e8888abcd99yyyzzz85a5";
const TOKEN: &str = "tok-conformance";
const NOW_SECS: u64 = 1_757_640_000;

/// Records what was published, and can be told to fail.
#[derive(Clone, Default)]
struct FakePublisher {
    published: Arc<Mutex<Vec<(String, Value)>>>,
    fail: bool,
}

impl Publisher for FakePublisher {
    async fn publish(&self, topic: &str, record: &Value) -> Result<(), PublishError> {
        if self.fail {
            return Err(PublishError::Rejected("permission denied".into()));
        }
        self.published
            .lock()
            .unwrap()
            .push((topic.to_string(), record.clone()));
        Ok(())
    }
}

fn registry() -> Registry {
    Registry::parse(&format!(
        r#"{{"users":[{{"path_token":"{TOKEN}","slack_user_id":"U_ME",
             "signing_secret":"{SECRET}",
             "topic":"projects/p/topics/events",
             "block_actions_topic":"projects/p/topics/presses"}}]}}"#
    ))
    .expect("the table parses")
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

struct Delivery {
    path: String,
    body: String,
    timestamp: String,
    signature: Option<String>,
    content_type: &'static str,
}

impl Delivery {
    fn events(body: Value) -> Self {
        let body = body.to_string();
        let timestamp = NOW_SECS.to_string();
        Self {
            path: format!("/slack/e/{TOKEN}"),
            signature: Some(sign(SECRET, &timestamp, body.as_bytes())),
            body,
            timestamp,
            content_type: "application/json",
        }
    }

    fn resign(mut self) -> Self {
        self.signature = Some(sign(SECRET, &self.timestamp, self.body.as_bytes()));
        self
    }

    fn request(&self) -> Request<Full<Bytes>> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(&self.path)
            .header("content-type", self.content_type)
            .header("x-slack-request-timestamp", &self.timestamp);
        if let Some(signature) = &self.signature {
            builder = builder.header("x-slack-signature", signature);
        }
        builder
            .body(Full::new(Bytes::from(self.body.clone())))
            .expect("a valid request")
    }
}

async fn send(
    publisher: FakePublisher,
    delivery: &Delivery,
) -> (StatusCode, String, FakePublisher) {
    let gateway = Arc::new(Gateway {
        registry: registry(),
        publisher: publisher.clone(),
    });
    let response = handle(gateway, delivery.request(), now()).await;
    let status = response.status();
    let body = String::from_utf8_lossy(
        &response
            .into_body()
            .collect()
            .await
            .expect("a complete body")
            .to_bytes(),
    )
    .into_owned();
    (status, body, publisher)
}

fn mention_payload() -> Value {
    json!({
        "type": "event_callback",
        "event": {
            "type": "message",
            "channel": "C0LOBBY",
            "user": "U_SENDER",
            "text": "<@U_ME> このリポジトリの CI を見てほしい",
            "ts": "1757640000.000100"
        }
    })
}

#[tokio::test]
async fn a_signed_mention_is_published_and_answered() {
    let delivery = Delivery::events(mention_payload());
    let (status, _, publisher) = send(FakePublisher::default(), &delivery).await;
    assert_eq!(status, StatusCode::OK);

    let published = publisher.published.lock().unwrap().clone();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, "projects/p/topics/events");
    assert_eq!(
        published[0].1.get("channel").and_then(Value::as_str),
        Some("C0LOBBY")
    );
}

/// The path is a credential. An unregistered one must not be distinguishable
/// from a registered one with a bad signature, or it becomes worth guessing.
#[tokio::test]
async fn an_unknown_path_is_refused_the_same_way_a_bad_signature_is() {
    let mut unknown = Delivery::events(mention_payload());
    unknown.path = "/slack/e/not-a-real-token".into();
    let (unknown_status, _, publisher) = send(FakePublisher::default(), &unknown).await;
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    assert!(publisher.published.lock().unwrap().is_empty());

    // …and the prefix alone is not a path.
    let mut bare = Delivery::events(mention_payload());
    bare.path = "/slack/e/".into();
    assert_eq!(
        send(FakePublisher::default(), &bare).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_unsigned_or_wrongly_signed_delivery_is_refused() {
    let mut unsigned = Delivery::events(mention_payload());
    unsigned.signature = None;
    assert_eq!(
        send(FakePublisher::default(), &unsigned).await.0,
        StatusCode::UNAUTHORIZED
    );

    // A signature for a *different* body: exactly what a replay with edited
    // content looks like.
    let mut tampered = Delivery::events(mention_payload());
    tampered.body = json!({ "type": "event_callback", "event": {
        "type": "message", "channel": "C0LOBBY", "user": "U_SENDER",
        "text": "<@U_ME> something else", "ts": "1757640000.000100"
    }})
    .to_string();
    let (status, _, publisher) = send(FakePublisher::default(), &tampered).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(publisher.published.lock().unwrap().is_empty());
}

/// The window is what stops a captured request from working forever, so a
/// correctly signed delivery outside it must still be refused.
#[tokio::test]
async fn a_correctly_signed_but_stale_delivery_is_refused() {
    let mut stale = Delivery::events(mention_payload());
    stale.timestamp = (NOW_SECS - 6 * 60).to_string();
    let stale = stale.resign();
    let (status, _, publisher) = send(FakePublisher::default(), &stale).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(publisher.published.lock().unwrap().is_empty());

    // Just inside the window still works, so the check is a window and not a
    // blanket refusal of anything not stamped this second.
    let mut fresh = Delivery::events(mention_payload());
    fresh.timestamp = (NOW_SECS - 4 * 60).to_string();
    assert_eq!(
        send(FakePublisher::default(), &fresh.resign()).await.0,
        StatusCode::OK
    );
}

/// A 200 before the publish succeeds loses the event permanently: Slack
/// treats it as delivered and never sends it again.
#[tokio::test]
async fn a_failed_publish_does_not_answer_200() {
    let failing = FakePublisher {
        fail: true,
        ..FakePublisher::default()
    };
    let (status, _, _) = send(failing, &Delivery::events(mention_payload())).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "only a 5xx makes Slack redeliver"
    );
}

/// Not a mention: nothing is published, and the answer is still 200 — a
/// non-200 would count as a failed delivery and push the app toward having
/// its subscription switched off.
#[tokio::test]
async fn a_message_naming_nobody_is_accepted_and_not_published() {
    let payload = json!({
        "type": "event_callback",
        "event": {
            "type": "message", "channel": "C0LOBBY", "user": "U_SENDER",
            "text": "今日のランチどこにします", "ts": "1757640000.000100"
        }
    });
    let (status, _, publisher) = send(FakePublisher::default(), &Delivery::events(payload)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(publisher.published.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_url_verification_challenge_is_echoed() {
    let challenge = "3eZbrw1aB1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0Uv";
    let payload = json!({ "type": "url_verification", "challenge": challenge });
    let (status, body, publisher) =
        send(FakePublisher::default(), &Delivery::events(payload)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, challenge);
    assert!(
        publisher.published.lock().unwrap().is_empty(),
        "registering a Request URL must not put a record on a topic"
    );
}

/// A press arrives form-encoded on the other Request URL, and has to reach the
/// press topic. Wiring up only Event Subscriptions leaves mentions working and
/// every approval button dead.
#[tokio::test]
async fn a_form_encoded_press_reaches_the_press_topic() {
    let payload = json!({
        "type": "block_actions",
        "user": { "id": "U_ME" },
        "container": { "type": "message", "channel_id": "D0SELFDM", "message_ts": "1757640100.000200" },
        "channel": { "id": "D0SELFDM" },
        "response_url": "https://hooks.slack.com/actions/T/1/abc",
        "actions": [{ "action_id": "approve_reply", "value": "{\"d\":\"draft-1\"}", "action_ts": "1757640200.111111" }]
    });
    let encoded = percent_encoding::utf8_percent_encode(
        &payload.to_string(),
        percent_encoding::NON_ALPHANUMERIC,
    )
    .to_string();
    let body = format!("payload={encoded}");
    let timestamp = NOW_SECS.to_string();
    let delivery = Delivery {
        path: format!("/slack/e/{TOKEN}"),
        signature: Some(sign(SECRET, &timestamp, body.as_bytes())),
        body,
        timestamp,
        content_type: "application/x-www-form-urlencoded",
    };

    let (status, _, publisher) = send(FakePublisher::default(), &delivery).await;
    assert_eq!(status, StatusCode::OK);
    let published = publisher.published.lock().unwrap().clone();
    assert_eq!(published.len(), 1);
    assert_eq!(
        published[0].0, "projects/p/topics/presses",
        "presses go to their own topic, whose retention is measured in minutes"
    );
    assert_eq!(
        published[0].1.get("action_id").and_then(Value::as_str),
        Some("approve_reply")
    );
}

/// Nothing derived from the body may reach the response. An error that echoed
/// the request would undo the point of not storing it.
#[tokio::test]
async fn no_response_body_echoes_the_request() {
    let secret_text = "<@U_ME> the password is hunter2";
    let payload = json!({
        "type": "event_callback",
        "event": {
            "type": "message", "channel": "C0LOBBY", "user": "U_SENDER",
            "text": secret_text, "ts": "1757640000.000100"
        }
    });
    for (publisher, delivery) in [
        (FakePublisher::default(), Delivery::events(payload.clone())),
        (
            FakePublisher {
                fail: true,
                ..FakePublisher::default()
            },
            Delivery::events(payload.clone()),
        ),
    ] {
        let (_, body, _) = send(publisher, &delivery).await;
        assert!(
            !body.contains("hunter2") && !body.contains("U_SENDER"),
            "the response echoed the request: {body}"
        );
    }

    // The published record must not carry it either — the projection's own
    // tests pin the shape, this pins it on the real path.
    let (_, _, publisher) = send(FakePublisher::default(), &Delivery::events(payload)).await;
    let published = publisher.published.lock().unwrap().clone();
    let serialized = published[0].1.to_string();
    assert!(
        !serialized.contains("hunter2"),
        "the published record carried the body: {serialized}"
    );
}

#[tokio::test]
async fn a_get_is_refused() {
    let gateway = Arc::new(Gateway {
        registry: registry(),
        publisher: FakePublisher::default(),
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/slack/e/{TOKEN}"))
        .body(Full::new(Bytes::new()))
        .expect("a valid request");
    assert_eq!(
        handle(gateway, request, now()).await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}
