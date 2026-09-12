//! Slack request signing: the only thing that separates a real delivery from
//! anyone who guessed the path.
//!
//! Slack signs `v0:{timestamp}:{raw body}` with the app's signing secret and
//! sends the hex digest as `X-Slack-Signature: v0=…`, with the timestamp in
//! `X-Slack-Request-Timestamp`.
//!
//! # Why both halves matter
//!
//! The signature alone proves the body came from Slack; it does **not** prove
//! it is not being replayed. The timestamp window is what makes a captured
//! delivery stop working, so checking one without the other is not a check
//! (ADR-0072 decision 11).
//!
//! The body must be verified **exactly as received**. Parsing it first and
//! re-serializing would verify a different byte string than the one signed,
//! which is how a signature check comes to pass on input nobody signed.

use std::time::{Duration, SystemTime};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Slack's signature version prefix.
const VERSION: &str = "v0";

/// How far a delivery's timestamp may be from local time.
///
/// Five minutes is Slack's own recommendation. It bounds replay of a captured
/// request; widening it widens that window by exactly as much.
pub const TIMESTAMP_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Why a delivery was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    /// A required header was absent or unreadable.
    #[error("missing or malformed `{0}`")]
    MissingHeader(&'static str),
    /// The timestamp is outside [`TIMESTAMP_WINDOW`].
    #[error("the request timestamp is more than 5 minutes from local time")]
    StaleTimestamp,
    /// The digest did not match.
    #[error("the request signature does not match")]
    Mismatch,
}

/// Verify one delivery.
///
/// `body` is the raw request body, byte for byte.
pub fn verify(
    signing_secret: &str,
    signature_header: Option<&str>,
    timestamp_header: Option<&str>,
    body: &[u8],
    now: SystemTime,
) -> Result<(), SignatureError> {
    let timestamp =
        timestamp_header.ok_or(SignatureError::MissingHeader("X-Slack-Request-Timestamp"))?;
    let seconds: u64 = timestamp
        .parse()
        .map_err(|_| SignatureError::MissingHeader("X-Slack-Request-Timestamp"))?;
    // `UNIX_EPOCH + Duration` panics past the platform's representable range,
    // and this runs **before** the HMAC — so a 20-digit timestamp, which
    // parses fine as a `u64`, would take the connection down without the
    // caller ever needing the signing secret. A timestamp that cannot be a
    // time is not within five minutes of now, so it fails the same way a
    // stale one does.
    let sent = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or(SignatureError::StaleTimestamp)?;
    // Distance in either direction: a clock ahead of ours is as suspect as one
    // behind, and `duration_since` on the wrong ordering is an error, not zero.
    let skew = now
        .duration_since(sent)
        .or_else(|_| sent.duration_since(now))
        .unwrap_or(Duration::ZERO);
    if skew > TIMESTAMP_WINDOW {
        return Err(SignatureError::StaleTimestamp);
    }

    let signature = signature_header.ok_or(SignatureError::MissingHeader("X-Slack-Signature"))?;
    let expected = sign(signing_secret, timestamp, body);
    // Constant-time. A `==` here returns as soon as two bytes differ, which
    // turns the digest into something an attacker can walk one byte at a time.
    let matches: bool = expected.as_bytes().ct_eq(signature.as_bytes()).into();
    if matches {
        Ok(())
    } else {
        Err(SignatureError::Mismatch)
    }
}

/// The `v0=…` header value Slack would send for this body.
pub fn sign(signing_secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256>>::new_from_slice(signing_secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(VERSION.as_bytes());
    mac.update(b":");
    mac.update(timestamp.as_bytes());
    mac.update(b":");
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    let mut out = String::with_capacity(VERSION.len() + 1 + digest.len() * 2);
    out.push_str(VERSION);
    out.push('=');
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "8f742231b10e8888abcd99yyyzzz85a5";
    const BODY: &[u8] = br#"{"type":"event_callback"}"#;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_correctly_signed_delivery_is_accepted() {
        let signature = sign(SECRET, "1757640000", BODY);
        assert!(signature.starts_with("v0="));
        assert!(
            verify(
                SECRET,
                Some(&signature),
                Some("1757640000"),
                BODY,
                at(1_757_640_000)
            )
            .is_ok()
        );
    }

    #[test]
    fn a_tampered_body_or_key_is_refused() {
        let signature = sign(SECRET, "1757640000", BODY);
        for (secret, body) in [
            (SECRET, br#"{"type":"event_callback"} "#.as_slice()),
            ("a-different-secret", BODY),
        ] {
            assert_eq!(
                verify(
                    secret,
                    Some(&signature),
                    Some("1757640000"),
                    body,
                    at(1_757_640_000)
                ),
                Err(SignatureError::Mismatch)
            );
        }
    }

    /// The window is what stops a captured request from working forever, so a
    /// valid signature outside it must still be refused.
    #[test]
    fn a_valid_signature_outside_the_window_is_refused() {
        let signature = sign(SECRET, "1757640000", BODY);
        let check = |now: u64| verify(SECRET, Some(&signature), Some("1757640000"), BODY, at(now));
        assert!(check(1_757_640_000 + 299).is_ok(), "inside the window");
        assert_eq!(
            check(1_757_640_000 + 301),
            Err(SignatureError::StaleTimestamp)
        );
        // Also in the other direction: a timestamp from the future is no more
        // trustworthy than one from the past.
        assert_eq!(
            check(1_757_640_000 - 301),
            Err(SignatureError::StaleTimestamp)
        );
    }

    /// A timestamp outside the representable range must be refused, not
    /// panicked on: this check runs before the HMAC, so reaching it needs only
    /// a registered path — never the signing secret. A panic here answers
    /// nothing, and an unanswered delivery counts against the app the same way
    /// a failure does.
    #[test]
    fn an_unrepresentable_timestamp_is_refused_not_panicked_on() {
        let signature = sign(SECRET, "1757640000", BODY);
        for timestamp in ["18446744073709551615", "9223372036854775808"] {
            assert_eq!(
                verify(
                    SECRET,
                    Some(&signature),
                    Some(timestamp),
                    BODY,
                    at(1_757_640_000)
                ),
                Err(SignatureError::StaleTimestamp),
                "`{timestamp}` must be refused"
            );
        }
        // A negative one does not parse as a `u64` at all, which is the other
        // shape of the same input.
        assert_eq!(
            verify(
                SECRET,
                Some(&signature),
                Some("-1"),
                BODY,
                at(1_757_640_000)
            ),
            Err(SignatureError::MissingHeader("X-Slack-Request-Timestamp"))
        );
    }

    #[test]
    fn a_delivery_missing_either_header_is_refused() {
        let signature = sign(SECRET, "1757640000", BODY);
        assert_eq!(
            verify(SECRET, None, Some("1757640000"), BODY, at(1_757_640_000)),
            Err(SignatureError::MissingHeader("X-Slack-Signature"))
        );
        assert_eq!(
            verify(SECRET, Some(&signature), None, BODY, at(1_757_640_000)),
            Err(SignatureError::MissingHeader("X-Slack-Request-Timestamp"))
        );
        assert_eq!(
            verify(
                SECRET,
                Some(&signature),
                Some("not-a-number"),
                BODY,
                at(1_757_640_000)
            ),
            Err(SignatureError::MissingHeader("X-Slack-Request-Timestamp"))
        );
    }

    /// The digest comparison must not be `==`. This cannot prove timing, but
    /// it does pin that the function takes the `subtle` path — the assertion a
    /// reviewer can check against the source.
    #[test]
    fn the_comparison_is_constant_time() {
        let source = include_str!("signature.rs");
        let verify_body = source
            .split_once("pub fn verify(")
            .expect("verify exists")
            .1
            .split_once("\npub fn sign(")
            .expect("sign follows verify")
            .0;
        assert!(
            verify_body.contains("ct_eq"),
            "the signature comparison must stay constant-time"
        );
        assert!(
            !verify_body.contains("expected == signature"),
            "a direct comparison leaks the digest one byte at a time"
        );
    }
}
