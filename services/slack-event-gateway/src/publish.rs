//! Publishing to Pub/Sub, and the credential that authorises it.
//!
//! **The 200 goes out only after a publish is accepted.** Answering first
//! would lose the event permanently — Slack considers a 200 delivered and
//! never sends it again — and answering with a failure instead is not free
//! either: sustained failures are what makes Slack disable the subscription,
//! which is the disease ADR-0072 exists to cure. So the order is: publish,
//! then answer; and a retry has to fit inside Slack's ~3-second ack window.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Where the token comes from on Cloud Run.
///
/// The metadata server hands out a token for the revision's service account,
/// so **no service-account key exists to distribute or rotate** (ADR-0072
/// decision 6). The only grant that matters is `roles/pubsub.publisher` on the
/// operators' topics.
const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// Refresh this long before the token actually expires, so a publish is never
/// racing the expiry it just checked.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// How long one publish may take, retries included.
///
/// Slack expects an acknowledgement within ~3 seconds and redelivers
/// otherwise. Overrunning does not just lose time: the redelivery is counted
/// as a failed attempt, and enough of those disable the subscription.
pub const PUBLISH_BUDGET: Duration = Duration::from_millis(2_500);

/// Why a publish failed.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The credential could not be obtained.
    #[error("could not get an access token: {0}")]
    Token(String),
    /// Pub/Sub refused or could not be reached.
    #[error("Pub/Sub refused the publish: {0}")]
    Rejected(String),
}

/// Supplies the bearer token.
///
/// A trait so tests need neither a metadata server nor a credential.
pub trait AccessTokens: Send + Sync {
    /// A currently-valid access token.
    fn token(&self) -> impl Future<Output = Result<String, PublishError>> + Send;
}

/// The Cloud Run metadata server, with the token cached until it expires.
pub struct MetadataTokens {
    client: reqwest::Client,
    cached: Mutex<Option<(String, Instant)>>,
}

impl MetadataTokens {
    /// A token source over `client`.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            cached: Mutex::new(None),
        }
    }
}

impl AccessTokens for MetadataTokens {
    async fn token(&self) -> Result<String, PublishError> {
        if let Some((token, expires_at)) = self.cached.lock().unwrap().as_ref()
            && Instant::now() < *expires_at
        {
            return Ok(token.clone());
        }
        let response = self
            .client
            .get(METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| PublishError::Token(e.to_string()))?;
        if !response.status().is_success() {
            return Err(PublishError::Token(format!(
                "the metadata server answered {}",
                response.status()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| PublishError::Token(e.to_string()))?;
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| PublishError::Token("no `access_token` in the response".into()))?;
        // Unlike the `gcloud` CLI, the metadata server does report a lifetime.
        // Treat a missing one as already-expiring rather than assuming an
        // hour: being wrong that way costs a round trip, the other way costs
        // every publish until the process restarts.
        let lifetime = body
            .get("expires_in")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or(EXPIRY_MARGIN);
        *self.cached.lock().unwrap() = Some((
            token.to_string(),
            Instant::now() + lifetime.saturating_sub(EXPIRY_MARGIN),
        ));
        Ok(token.to_string())
    }
}

/// Publishes records.
///
/// A trait for the same reason [`AccessTokens`] is: the HTTP handler's
/// "answer only after the publish is accepted" rule has to be testable without
/// a Pub/Sub project.
pub trait Publisher: Send + Sync {
    /// Put `record` on `topic`. `Ok` means Pub/Sub accepted it.
    fn publish(
        &self,
        topic: &str,
        record: &Value,
    ) -> impl Future<Output = Result<(), PublishError>> + Send;
}

/// The production publisher: the Pub/Sub REST API.
pub struct PubSub<A: AccessTokens> {
    client: reqwest::Client,
    base_url: String,
    tokens: A,
}

impl<A: AccessTokens> PubSub<A> {
    /// A publisher against `base_url`, authenticating with `tokens`.
    pub fn new(client: reqwest::Client, base_url: &str, tokens: A) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens,
        }
    }
}

impl<A: AccessTokens> Publisher for PubSub<A> {
    async fn publish(&self, topic: &str, record: &Value) -> Result<(), PublishError> {
        let token = self.tokens.token().await?;
        let body = json!({
            "messages": [{ "data": base64_encode(record.to_string().as_bytes()) }],
        });
        let response = self
            .client
            .post(format!("{}/v1/{topic}:publish", self.base_url))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| PublishError::Rejected(e.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // The response body can name the topic and the missing role, which is
        // the whole difference between "publishing is broken" and "grant
        // roles/pubsub.publisher on projects/…/topics/…". It contains no
        // Slack content — it is Google's error, about our own request.
        let detail = response.text().await.unwrap_or_default();
        Err(PublishError::Rejected(format!(
            "{status}: {}",
            detail.chars().take(400).collect::<String>()
        )))
    }
}

/// Standard-alphabet base64 with padding, which is what Pub/Sub expects for
/// `message.data`.
///
/// Hand-rolled to keep the dependency list of a container that handles every
/// message the operator can see as short as it can be. Twenty lines, and the
/// round trip is pinned by a test.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = ((triple >> (18 - 6 * i)) & 0x3F) as usize;
                out.push(ALPHABET[index] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode with an independent implementation, so the test is not the
    /// encoder checking itself.
    fn decode(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits: u32 = 0;
        let mut width = 0;
        let mut out = Vec::new();
        for byte in text.bytes().filter(|b| *b != b'=') {
            let value = ALPHABET
                .iter()
                .position(|c| *c == byte)
                .unwrap_or_else(|| panic!("`{}` is not base64", byte as char));
            bits = (bits << 6) | value as u32;
            width += 6;
            if width >= 8 {
                width -= 8;
                out.push(((bits >> width) & 0xFF) as u8);
            }
        }
        out
    }

    #[test]
    fn base64_round_trips_at_every_padding_length() {
        for input in [
            "".as_bytes(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            br#"{"v":1,"kind":"message"}"#,
            // Multi-byte UTF-8, which a Slack channel id never is but a topic
            // name in an error path might be.
            "メンション".as_bytes(),
        ] {
            let encoded = base64_encode(input);
            assert_eq!(decode(&encoded), input, "round trip of {input:?}");
            assert_eq!(encoded.len() % 4, 0, "padding of {input:?}");
        }
        // Pinned against known vectors (RFC 4648 §10), so a bug that is
        // self-consistent in both directions still fails.
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
