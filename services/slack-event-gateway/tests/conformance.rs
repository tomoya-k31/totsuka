//! The Event Gateway conformance suite, run from the gateway's side (#656).
//!
//! The same `contracts/slack-event-gateway/cases/` the plugin's tests read.
//! totsuka cannot see this code — the gateway runs under the operator's own
//! account and may be a fork — so these files are the entire agreement, and
//! both sides prove they satisfy them independently.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use slack_event_gateway::project::{
    Endpoint, Projection, Topic, decode_interactivity_payload, delivery_id, encode, project,
};

/// The registered operator every case is judged against.
const CONFORMANCE_USER_ID: &str = "U_ME";
/// The fixed clock the fixtures were written against. Injected rather than
/// read, which is why `project` takes `received_at` as an argument.
const CONFORMANCE_NOW: &str = "2026-09-13T00:00:00Z";

/// Find `contracts/slack-event-gateway` by walking **up** from this crate.
///
/// A hardcoded `../` would keep compiling after either side moved and simply
/// find nothing — and a suite that loads zero cases **passes**. Failing loudly
/// here is the only way that mistake stays visible.
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

fn expectations(case: &Case) -> &Vec<Value> {
    case.body
        .get("expect")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{}: `expect` must be an array", case.name))
}

/// The whole point: this gateway reproduces every committed case.
#[test]
fn the_gateway_reproduces_every_case() {
    for case in load_cases() {
        let payload = case
            .body
            .pointer("/delivery/payload")
            .unwrap_or_else(|| panic!("{}: missing delivery.payload", case.name));
        let produced = project(
            endpoint_of(&case),
            payload,
            CONFORMANCE_USER_ID,
            CONFORMANCE_NOW,
        );

        if let Some(challenge) = case.body.get("expect_challenge").and_then(Value::as_str) {
            assert!(
                expectations(&case).is_empty(),
                "{}: a url_verification case must expect no records",
                case.name
            );
            assert_eq!(
                produced,
                Projection::Challenge(challenge.to_string()),
                "{}: the challenge must be echoed and nothing published",
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
            let want_topic = match want.get("topic").and_then(Value::as_str) {
                Some("events") => Topic::Events,
                Some("block_actions") => Topic::BlockActions,
                other => panic!("{}: unknown expect[].topic {other:?}", case.name),
            };
            assert_eq!(got.topic, want_topic, "{}: wrong topic", case.name);
            assert_eq!(
                &encode(&got.record),
                want.get("record").expect("expect[].record"),
                "{}: the projected record diverged from the fixture",
                case.name
            );
            assert_eq!(
                delivery_id(&got.record),
                want.get("identity")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{}: expect[].identity", case.name)),
                "{}: wrong delivery identity",
                case.name
            );
        }
    }
}

/// The wire encoding the fixtures describe but do not themselves carry: an
/// interactivity delivery is `payload=` plus the percent-encoded JSON.
///
/// The fixtures hold the **decoded** payload, so this is the one step they
/// cannot pin — and getting it wrong kills every button while mentions carry
/// on working.
#[test]
fn interactivity_cases_survive_their_real_wire_encoding() {
    let mut checked = 0;
    for case in load_cases() {
        if endpoint_of(&case) != Endpoint::Interactivity {
            continue;
        }
        let payload = case
            .body
            .pointer("/delivery/payload")
            .expect("delivery.payload");
        assert_eq!(
            case.body
                .pointer("/delivery/content_type")
                .and_then(Value::as_str),
            Some("application/x-www-form-urlencoded"),
            "{}: an interactivity case must declare the form encoding",
            case.name
        );
        let encoded = percent_encoding::utf8_percent_encode(
            &payload.to_string(),
            percent_encoding::NON_ALPHANUMERIC,
        )
        .to_string();
        let body = format!("payload={encoded}");
        assert_eq!(
            decode_interactivity_payload(body.as_bytes()).as_ref(),
            Some(payload),
            "{}: the form-encoded body did not decode back to the fixture",
            case.name
        );
        checked += 1;
    }
    assert!(checked >= 3, "the suite lost its interactivity cases");
}

/// No record may carry the body, or anything that could hide one.
#[test]
fn no_projected_record_carries_the_message_body() {
    for case in load_cases() {
        let payload = case
            .body
            .pointer("/delivery/payload")
            .expect("delivery.payload");
        let Projection::Publish(published) = project(
            endpoint_of(&case),
            payload,
            CONFORMANCE_USER_ID,
            CONFORMANCE_NOW,
        ) else {
            continue;
        };
        for item in published {
            let json = encode(&item.record);
            let keys: BTreeSet<&str> = json
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .collect();
            for forbidden in [
                "text",
                "body",
                "message",
                "blocks",
                "attachments",
                "files",
                "elements",
                "payload",
            ] {
                assert!(
                    !keys.contains(forbidden),
                    "{}: the record carries `{forbidden}`",
                    case.name
                );
            }
        }
    }
}
