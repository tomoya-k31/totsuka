//! The request handler: route, verify, project, publish, answer.
//!
//! # The order is the design
//!
//! 1. **Route by path.** `/slack/e/<opaque-token>` picks the operator, and
//!    therefore which key verifies the request. Reading the identity out of
//!    the body instead would mean choosing the key by trusting unverified
//!    input (ADR-0072 decision 6).
//! 2. **Verify the signature and the timestamp**, over the raw bytes.
//! 3. **Project**, comparing the body against constant strings and nothing
//!    else.
//! 4. **Publish**, and only then answer 200. A 200 before the publish loses
//!    the event for good.
//!
//! Steps 1-2 are the entire gate. Slack cannot be an IAM principal and
//! publishes no stable source-IP list, so there is no layer in front of this
//! container — the path, the signature and the window are what stand between
//! it and the open internet (decision 11).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::Value;

use crate::project::{Endpoint, Projection, Topic, decode_interactivity_payload, encode, project};
use crate::publish::{PUBLISH_BUDGET, Publisher};
use crate::registry::Registry;
use crate::signature::{self, SignatureError};

/// Path prefix every delivery arrives on.
const PATH_PREFIX: &str = "/slack/e/";

/// Largest body accepted.
///
/// Slack's own limit is well under this. The cap exists so an unauthenticated
/// caller — which every caller is, until the signature is checked — cannot
/// make the process allocate without bound.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Everything one request needs.
pub struct Gateway<P: Publisher> {
    /// Who may post, and where their records go.
    pub registry: Registry,
    /// Where records are published.
    pub publisher: P,
}

/// Handle one request.
///
/// Returns the response to send. **It never contains anything derived from the
/// request body** — an error message that echoed the body would defeat the
/// point of not storing it.
pub async fn handle<P: Publisher, B: Body>(
    gateway: Arc<Gateway<P>>,
    request: Request<B>,
    now: SystemTime,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST {
        return text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }
    let path = request.uri().path().to_string();
    let Some(path_token) = path.strip_prefix(PATH_PREFIX).filter(|t| !t.is_empty()) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    // Read the headers before the body is consumed.
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let signature = header("x-slack-signature");
    let timestamp = header("x-slack-request-timestamp");
    let content_type = header("content-type").unwrap_or_default();

    let Some(registration) = gateway.registry.lookup(path_token) else {
        // Deliberately the same answer an unroutable path gets. Telling an
        // unauthenticated caller "that token exists but the signature was
        // wrong" turns the path into something worth brute-forcing.
        tracing::info!("refused a delivery on an unregistered path");
        return text(StatusCode::NOT_FOUND, "not found");
    };

    let body = match read_body(request).await {
        Ok(body) => body,
        Err(status) => return text(status, "bad request"),
    };

    if let Err(e) = signature::verify(
        &registration.signing_secret,
        signature.as_deref(),
        timestamp.as_deref(),
        &body,
        now,
    ) {
        // The reason is logged; the response says nothing. Which check failed
        // is information an attacker can use to iterate.
        match e {
            SignatureError::StaleTimestamp => {
                tracing::info!(user = %registration.slack_user_id, "refused a stale delivery")
            }
            _ => tracing::info!(
                user = %registration.slack_user_id,
                "refused an unsigned or wrongly signed delivery"
            ),
        }
        return text(StatusCode::UNAUTHORIZED, "unauthorized");
    }

    // Interactivity arrives form-encoded; events arrive as JSON. Reading a
    // press as JSON fails on every button, which looks like broken buttons
    // rather than a decoding mistake.
    let (endpoint, payload) = if content_type.starts_with("application/x-www-form-urlencoded") {
        match decode_interactivity_payload(&body) {
            Some(payload) => (Endpoint::Interactivity, payload),
            None => return text(StatusCode::BAD_REQUEST, "bad request"),
        }
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(payload) => (Endpoint::Events, payload),
            Err(_) => return text(StatusCode::BAD_REQUEST, "bad request"),
        }
    };
    // The body is not needed past this point, and nothing below may reach it.
    drop(body);

    let received_at = rfc3339(now);
    match project(
        endpoint,
        &payload,
        &registration.slack_user_id,
        &received_at,
    ) {
        Projection::Challenge(challenge) => {
            tracing::info!(
                user = %registration.slack_user_id,
                "answered a Request URL verification"
            );
            text(StatusCode::OK, &challenge)
        }
        Projection::Publish(published) => {
            for item in &published {
                let topic = match item.topic {
                    Topic::Events => &registration.topic,
                    Topic::BlockActions => &registration.block_actions_topic,
                };
                let result = tokio::time::timeout(
                    PUBLISH_BUDGET,
                    gateway.publisher.publish(topic, &encode(&item.record)),
                )
                .await;
                match result {
                    Ok(Ok(())) => {}
                    // Answering 200 here would lose the event permanently:
                    // Slack treats a 200 as delivered and never retries. A 5xx
                    // asks for a redelivery, which is the only way this event
                    // survives — at the cost of counting as a failed attempt.
                    Ok(Err(e)) => {
                        tracing::error!(
                            user = %registration.slack_user_id,
                            error = %e,
                            "could not publish; answering 500 so Slack redelivers"
                        );
                        return text(StatusCode::INTERNAL_SERVER_ERROR, "publish failed");
                    }
                    Err(_) => {
                        tracing::error!(
                            user = %registration.slack_user_id,
                            "publish exceeded the acknowledgement budget; answering 500"
                        );
                        return text(StatusCode::INTERNAL_SERVER_ERROR, "publish timed out");
                    }
                }
            }
            tracing::info!(
                user = %registration.slack_user_id,
                published = published.len(),
                "accepted a delivery"
            );
            text(StatusCode::OK, "")
        }
    }
}

/// Read the body, refusing anything over [`MAX_BODY_BYTES`].
async fn read_body<B: Body>(request: Request<B>) -> Result<Bytes, StatusCode> {
    if let Some(declared) = request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        && declared > MAX_BODY_BYTES
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let collected = request
        .into_body()
        .collect()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .to_bytes();
    if collected.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(collected)
}

fn text(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("a static response always builds")
}

/// `SystemTime` as an RFC 3339 UTC timestamp, which is the `received_at`
/// format the contract fixes.
///
/// Hand-rolled, like the base64: the alternative is a date-time crate in a
/// container whose dependency list is itself part of what the operator is
/// being asked to trust.
pub fn rfc3339(time: SystemTime) -> String {
    let total = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let days = total.div_euclid(86_400);
    let seconds = total.rem_euclid(86_400);
    // Civil date from days since the epoch (Howard Hinnant's
    // `civil_from_days`). Written out rather than approximated because a leap
    // -year slip would move `received_at` by a day once every four years and
    // never be noticed.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_matches_known_instants() {
        let at = |s: u64| rfc3339(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(1_789_257_600), "2026-09-13T00:00:00Z");
        assert_eq!(at(1_757_640_000), "2025-09-12T01:20:00Z");
        // 2024 is a leap year, 2023 is not, 2000 was and 1900 was not — the
        // three rules, each on the day they matter.
        assert_eq!(at(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_677_628_800), "2023-03-01T00:00:00Z");
    }
}
