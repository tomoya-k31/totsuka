//! The Event Gateway conformance suite, run from totsuka's side (#656).
//!
//! Every case under `contracts/slack-event-gateway/cases/` pairs a raw Slack
//! delivery with the Pub/Sub records it must turn into. An Event Gateway is
//! conformant when it reproduces them; this file proves the *reference*
//! projection in [`task_source_slack::gateway_contract`] does, so the fixtures
//! and the consumer that reads them cannot drift apart.
//!
//! # Why this is a test suite and not one sample record
//!
//! Decision 4 of ADR-0072 narrowed what gets published, which made the
//! gateway's filter a **gate**: a message it drops has no record at all and is
//! invisible to totsuka forever. A single golden record would pin the *shape*
//! and let the *judgement* drift — and judgement is where the damage is, since
//! a false negative is a mention that silently disappears.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use task_source_slack::gateway_contract::{
    Endpoint, GatewayRecord, Projection, Registration, Topic, project,
};

/// The registered operator every case is judged against.
const CONFORMANCE_USER_ID: &str = "U_ME";
/// The fixed clock the fixtures were written against. A gateway under test
/// injects this instead of reading the wall clock; see the suite's README.
const CONFORMANCE_NOW: &str = "2026-09-13T00:00:00Z";

/// Keys that must never appear in a published record, whatever the kind.
///
/// The schema has no field for any of them, so this is not really about serde
/// — it is a tripwire for the day someone adds "just the text, it is easier".
/// The behavioural half of the promise (do not hold, log or forward the body)
/// cannot be checked from here and belongs to the gateway's own tests.
const BODY_BEARING_KEYS: [&str; 8] = [
    "text",
    "body",
    "message",
    "blocks",
    "attachments",
    "files",
    "elements",
    "payload",
];

/// Find `contracts/slack-event-gateway` by walking up from this crate.
///
/// A hardcoded `../../` would keep compiling after either side moved and
/// simply find nothing — and a suite that loads zero cases **passes**. Failing
/// loudly here is the only way that mistake stays visible.
fn contracts_dir() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join("contracts/slack-event-gateway");
        if candidate.join("cases").is_dir() {
            return candidate;
        }
        dir = dir.parent().unwrap_or_else(|| {
            panic!(
                "no `contracts/slack-event-gateway/cases` above {}",
                env!("CARGO_MANIFEST_DIR")
            )
        });
    }
}

struct Case {
    name: String,
    body: Value,
}

fn load_cases() -> Vec<Case> {
    let dir = contracts_dir().join("cases");
    let mut cases: Vec<Case> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            Case {
                name: path
                    .file_stem()
                    .expect("fixture file name")
                    .to_string_lossy()
                    .into_owned(),
                body: serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display())),
            }
        })
        .collect();
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(
        cases.len() >= 24,
        "only {} conformance cases found — the suite was probably not loaded from where it lives",
        cases.len()
    );
    cases
}

fn endpoint_of(case: &Case) -> Endpoint {
    match case
        .body
        .pointer("/delivery/endpoint")
        .and_then(Value::as_str)
    {
        Some("events") => Endpoint::Events,
        Some("interactivity") => Endpoint::Interactivity,
        other => panic!("{}: unknown delivery.endpoint {other:?}", case.name),
    }
}

fn topic_of(case: &Case, expectation: &Value) -> Topic {
    match expectation.get("topic").and_then(Value::as_str) {
        Some("events") => Topic::Events,
        Some("block_actions") => Topic::BlockActions,
        other => panic!("{}: unknown expect[].topic {other:?}", case.name),
    }
}

fn expectations(case: &Case) -> &Vec<Value> {
    case.body
        .get("expect")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{}: `expect` must be an array", case.name))
}

/// Every case documents the property it defends. An undocumented case is one
/// nobody can decide to delete later.
#[test]
fn every_case_says_what_it_is_for() {
    for case in load_cases() {
        let why = case
            .body
            .get("why")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{}: missing `why`", case.name));
        assert!(
            why.chars().count() >= 20,
            "{}: `why` is too short to be a reason",
            case.name
        );
    }
}

/// The whole point: the reference projection reproduces every fixture.
#[test]
fn the_reference_projection_reproduces_every_case() {
    for case in load_cases() {
        let payload = case
            .body
            .pointer("/delivery/payload")
            .unwrap_or_else(|| panic!("{}: missing delivery.payload", case.name));
        let produced = project(
            endpoint_of(&case),
            payload,
            Registration {
                user_id: CONFORMANCE_USER_ID,
            },
            CONFORMANCE_NOW,
        );

        if let Some(challenge) = case.body.get("expect_challenge").and_then(Value::as_str) {
            // The fixture itself has to agree that nothing is published —
            // otherwise a case could carry both a challenge and records, and
            // this branch would pass while asserting the opposite.
            assert!(
                expectations(&case).is_empty(),
                "{}: a url_verification case must expect no records",
                case.name
            );
            assert_eq!(
                produced,
                Projection::Challenge(challenge.to_string()),
                "{}: the url_verification challenge must be echoed and nothing published",
                case.name
            );
            continue;
        }

        let Projection::Publish(published) = produced else {
            panic!(
                "{}: produced a challenge, but the case expects records",
                case.name
            );
        };
        let expected = expectations(&case);
        assert_eq!(
            published.len(),
            expected.len(),
            "{}: published {} record(s), the case expects {}",
            case.name,
            published.len(),
            expected.len()
        );
        for (got, want) in published.iter().zip(expected) {
            assert_eq!(
                got.topic,
                topic_of(&case, want),
                "{}: published to the wrong topic",
                case.name
            );
            let want_record = want
                .get("record")
                .unwrap_or_else(|| panic!("{}: expect[].record is missing", case.name));
            assert_eq!(
                &serde_json::to_value(&got.record).unwrap(),
                want_record,
                "{}: the projected record diverged from the fixture",
                case.name
            );
        }
    }
}

/// Each committed record still parses, and re-serializes byte-for-byte. This
/// is what turns a field rename into a failing test rather than a gateway that
/// quietly publishes something nobody reads.
#[test]
fn committed_records_round_trip_through_the_schema() {
    let mut seen = 0;
    for case in load_cases() {
        for expectation in expectations(&case) {
            let raw = expectation.get("record").expect("expect[].record");
            let parsed = GatewayRecord::from_value(raw)
                .unwrap_or_else(|e| panic!("{}: record no longer parses: {e}", case.name));
            assert_eq!(
                &serde_json::to_value(&parsed).unwrap(),
                raw,
                "{}: re-serialized record diverged from the committed wire",
                case.name
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "no records in the suite at all");
}

/// The delivery-identity rules, pinned per case rather than restated in prose.
#[test]
fn delivery_identity_matches_the_committed_key() {
    for case in load_cases() {
        for expectation in expectations(&case) {
            let raw = expectation.get("record").expect("expect[].record");
            let record = GatewayRecord::from_value(raw).expect("record parses");
            let want = expectation
                .get("identity")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{}: expect[].identity is missing", case.name));
            assert_eq!(record.delivery_id(), want, "{}", case.name);
            // A message's key is the identity minus the prefix — the same
            // string `Mention::message_key()` builds, so gateway-sourced and
            // Socket Mode-sourced deliveries collide in the same dedup set.
            if let Some(key) = record.message_key() {
                assert_eq!(want, format!("message:{key}"), "{}", case.name);
            }
        }
    }
}

/// No record carries the body, or anything that could hide one.
#[test]
fn no_record_carries_the_message_body() {
    for case in load_cases() {
        for expectation in expectations(&case) {
            let raw = expectation.get("record").expect("expect[].record");
            let keys: BTreeSet<&str> = raw
                .as_object()
                .unwrap_or_else(|| panic!("{}: record is not an object", case.name))
                .keys()
                .map(String::as_str)
                .collect();
            for forbidden in BODY_BEARING_KEYS {
                assert!(
                    !keys.contains(forbidden),
                    "{}: record carries `{forbidden}` — the schema must not be able to move a body",
                    case.name
                );
            }
        }
    }
}

/// The flattened press has to be readable by the code that already handles
/// Slack's own payload, or every approval button goes dead.
#[test]
fn press_records_rebuild_into_a_payload_the_pipeline_reads() {
    let mut checked = 0;
    for case in load_cases() {
        for expectation in expectations(&case) {
            let record =
                GatewayRecord::from_value(expectation.get("record").expect("record")).unwrap();
            let Some(payload) = record.block_actions_payload() else {
                continue;
            };
            assert!(
                payload
                    .pointer("/actions/0/action_id")
                    .is_some_and(Value::is_string),
                "{}: rebuilt payload has no actions[0].action_id",
                case.name
            );
            assert!(
                payload
                    .pointer("/actions/0/value")
                    .is_some_and(Value::is_string),
                "{}: rebuilt payload has no actions[0].value",
                case.name
            );
            assert!(
                payload.get("response_url").is_some_and(Value::is_string),
                "{}: rebuilt payload has no response_url",
                case.name
            );
            assert!(
                payload
                    .pointer("/container/channel_id")
                    .is_some_and(Value::is_string),
                "{}: rebuilt payload has no container.channel_id",
                case.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "the suite lost its block_actions cases");
}

/// The suite must keep covering the boundary shapes. Naming them here means
/// deleting one is a failing test, not a quiet reduction in coverage.
///
/// The list is **not** all one kind, and saying otherwise in a change whose
/// subject is that asymmetry would be careless. Most of these pin the false
/// negative — a mention that silently vanishes, the only fatal direction. A
/// few pin the false positive instead (`message-mention-prefix-not-match`,
/// the broadcast trio, the file reaction): harmless to the consumer, but they
/// put records nobody will ever read on a topic for seven days, per user.
#[test]
fn the_false_negative_boundary_cases_are_still_present() {
    let names: BTreeSet<String> = load_cases().into_iter().map(|case| case.name).collect();
    for required in [
        "message-mention-closed-tag",
        "message-mention-labeled-tag",
        "message-mention-no-surrounding-space",
        "message-mention-prefix-not-match",
        "message-subteam-closed-tag",
        "message-subteam-labeled-tag",
        "message-mention-and-subteam-together",
        "message-broadcast-here",
        "message-broadcast-channel",
        "message-broadcast-everyone",
        "message-without-text-field",
        "message-mention-inside-thread",
        "reaction-added-by-operator",
        "reaction-added-by-someone-else",
        "block-actions-approve-from-container",
        "block-actions-channel-id-fallback",
        "url-verification-challenge",
        // Records must not be able to carry free text, and a coordinate that
        // the consumer refuses outright must not be stored for seven days.
        "message-subteam-tag-with-prose",
        "message-subteam-unterminated",
        "reaction-added-on-a-file",
    ] {
        assert!(
            names.contains(required),
            "conformance case `{required}` is gone — it pins a way a mention can silently vanish"
        );
    }
}
