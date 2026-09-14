//! The registration table: which opaque path belongs to whom, what key
//! verifies their deliveries, and where their records go.
//!
//! One row per operator. Slack apps are per-operator (ADR-0072 decision 6)
//! because a shared app collapses two people's deliveries into one event and
//! then needs an extra API call per message to say who it was for — not
//! something to put on every message in every channel.
//!
//! **Nothing here is committed.** The table lives in the operator's own Secret
//! Manager and arrives as a mounted file or an environment variable.

use std::collections::HashMap;

use serde::Deserialize;
use subtle::ConstantTimeEq;

/// One operator.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    /// The unguessable path segment this operator's Slack app posts to.
    ///
    /// **This is the routing key, and it has to be unguessable** — Slack
    /// cannot be an IAM principal and publishes no stable source-IP list, so
    /// the only things standing between the open internet and this container
    /// are the path, the signature, and the timestamp window (decision 11).
    pub path_token: String,
    /// The operator's Slack user id. Mentions matching it, and reactions they
    /// added, are what gets published.
    pub slack_user_id: String,
    /// The Slack app's signing secret, used to verify every delivery.
    pub signing_secret: String,
    /// Fully-qualified topic for messages and reactions.
    pub topic: String,
    /// Fully-qualified topic for button presses. Separate because its
    /// retention has to clear `response_url`'s ~30-minute life while the
    /// other's is measured in days (decision 5).
    pub block_actions_topic: String,
}

/// Every registered operator.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// The rows.
    pub users: Vec<Registration>,
}

/// Why a table was refused.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The JSON did not parse, or had the wrong shape.
    #[error("the registration table is not valid JSON: {0}")]
    Malformed(String),
    /// A row is unusable.
    #[error("the registration table is unusable: {0}")]
    Invalid(String),
}

/// Shortest `path_token` the container will start with.
///
/// **Kept in step with `tofu/variables.tf`**, whose `operators` variable
/// carries the same 32 (`openssl rand -hex 24` is what its error message tells
/// you to generate). The number is written in two places because the two
/// guards protect different people: the OpenTofu one stops a bad table from
/// being built, this one stops a bad table from being *run*, and a table can
/// reach the container without going through OpenTofu at all — `main.rs`
/// loads it from `REGISTRATIONS_PATH` or `REGISTRATIONS`, either of which an
/// operator can write by hand into Secret Manager.
///
/// Length is counted in characters, not bytes, so the two agree: HCL's
/// `length()` on a string counts characters.
const MIN_PATH_TOKEN_CHARS: usize = 32;

impl Registry {
    /// Parse and check a table.
    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let registry: Self =
            serde_json::from_str(raw).map_err(|e| RegistryError::Malformed(e.to_string()))?;
        registry.validate()?;
        Ok(registry)
    }

    /// Refuse a table that would start cleanly and then misbehave.
    ///
    /// Every check here is something whose runtime symptom is silence rather
    /// than an error — a duplicate path routing to whichever row was read
    /// first, an empty secret verifying nothing, a short path token that holds
    /// until somebody guesses it, a shared topic delivering one operator's
    /// messages to another.
    fn validate(&self) -> Result<(), RegistryError> {
        if self.users.is_empty() {
            return Err(RegistryError::Invalid(
                "no `users` — every delivery would be refused as an unknown path".into(),
            ));
        }
        let mut seen: HashMap<&str, ()> = HashMap::new();
        // **One set for both topic kinds, not two.** Split into a set per
        // column and "A's `topic` equals B's `block_actions_topic`" walks
        // through — which is the collision that breaks the retention premise
        // (decision 5): presses need their own topic precisely because its
        // retention has to clear a `response_url`'s ~30-minute life while the
        // other's is measured in days.
        // Value: `(owner, which column they used)` — in a cross-column
        // collision the two sides are different fields, and naming only the
        // current row's leaves the reader to work out where the other half is.
        let mut topics: HashMap<&str, (&str, &str)> = HashMap::new();
        for user in &self.users {
            for (field, value) in [
                ("path_token", &user.path_token),
                ("slack_user_id", &user.slack_user_id),
                ("signing_secret", &user.signing_secret),
                ("topic", &user.topic),
                ("block_actions_topic", &user.block_actions_topic),
            ] {
                if value.trim().is_empty() {
                    return Err(RegistryError::Invalid(format!(
                        "a row has an empty `{field}`"
                    )));
                }
            }
            // **The path is the credential, and it faces the open internet.**
            // Slack cannot be an IAM principal and publishes no stable source
            // IP list, so the path, the signature and the timestamp window are
            // the three things standing in front of this container (decision
            // 11). A short token can be guessed, and the symptom until it is
            // guessed is nothing at all — which is what puts this check here
            // rather than only in the OpenTofu module.
            if user.path_token.chars().count() < MIN_PATH_TOKEN_CHARS {
                return Err(RegistryError::Invalid(format!(
                    "`{}` has a `path_token` shorter than {MIN_PATH_TOKEN_CHARS} characters; it \
                     is the routing credential on an endpoint with no IAM in front of it, and a \
                     short one can be guessed with nothing to show for the attempts — generate \
                     one with `openssl rand -hex 24`",
                    user.slack_user_id
                )));
            }
            if user.topic == user.block_actions_topic {
                return Err(RegistryError::Invalid(format!(
                    "`{}` uses one topic for both kinds; presses need their own, because its \
                     retention has to clear the ~30 minutes a `response_url` lives while the \
                     other's is measured in days",
                    user.slack_user_id
                )));
            }
            if seen.insert(user.path_token.as_str(), ()).is_some() {
                return Err(RegistryError::Invalid(
                    "two rows share a `path_token`; one of them would never receive anything"
                        .into(),
                ));
            }
            // The same silence, one field over: two operators pointed at one
            // topic start without a word, and then one person's deliveries —
            // their channel ids, their `ts`, their reactions — land in the
            // other's subscription. Decision 6 separates operators by path
            // *and* topic; this is what makes the second half true.
            for (field, topic) in [
                ("topic", &user.topic),
                ("block_actions_topic", &user.block_actions_topic),
            ] {
                if let Some((owner, owner_field)) =
                    topics.insert(topic.as_str(), (user.slack_user_id.as_str(), field))
                {
                    return Err(RegistryError::Invalid(format!(
                        "`{}` (`{field}`) and `{owner}` (`{owner_field}`) both publish to \
                         `{topic}`; one operator's deliveries would land in the other's \
                         subscription",
                        user.slack_user_id
                    )));
                }
            }
        }
        Ok(())
    }

    /// The row a request path belongs to.
    ///
    /// The comparison is constant-time and every row is examined, so the
    /// response time does not narrow down which prefix of a guessed token was
    /// right. That matters more here than it looks: the path *is* the routing
    /// credential, and an attacker can retry it as often as they like.
    pub fn lookup(&self, path_token: &str) -> Option<&Registration> {
        let mut found = None;
        for user in &self.users {
            let hit: bool = user
                .path_token
                .as_bytes()
                .ct_eq(path_token.as_bytes())
                .into();
            if hit {
                found = Some(user);
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &str) -> String {
        format!(r#"{{"users":[{rows}]}}"#)
    }

    fn row(token: &str, user: &str) -> String {
        format!(
            r#"{{"path_token":"{token}","slack_user_id":"{user}","signing_secret":"s",
                 "topic":"projects/p/topics/{user}-e",
                 "block_actions_topic":"projects/p/topics/{user}-a"}}"#
        )
    }

    /// A token that passes the length check, distinguished by `seed`.
    ///
    /// Every row in these tests goes through here. A fixture short enough to
    /// be refused on length would make each assertion below pass for a reason
    /// it does not name — "empty signing secret" would be proving the length
    /// check instead.
    fn tok(seed: &str) -> String {
        let pad = MIN_PATH_TOKEN_CHARS.saturating_sub(seed.chars().count());
        format!("{seed}{}", "0".repeat(pad))
    }

    #[test]
    fn a_registered_path_resolves_to_its_row() {
        let (a, b) = (tok("tok-a"), tok("tok-b"));
        let registry = Registry::parse(&table(&format!("{},{}", row(&a, "U_A"), row(&b, "U_B"))))
            .expect("parses");
        assert_eq!(registry.lookup(&b).unwrap().slack_user_id, "U_B");
        assert!(registry.lookup(&tok("tok-c")).is_none());
        // A prefix of a real token is not a match, and neither is an extension.
        assert!(registry.lookup(&a[..a.len() - 1]).is_none());
        assert!(registry.lookup(&format!("{a}0")).is_none());
    }

    #[test]
    fn a_table_that_would_misbehave_silently_is_refused() {
        assert!(Registry::parse(r#"{"users":[]}"#).is_err(), "empty");
        let same = tok("same");
        assert!(
            Registry::parse(&table(&format!(
                "{},{}",
                row(&same, "U_A"),
                row(&same, "U_B")
            )))
            .is_err(),
            "duplicate path_token"
        );
        let t = tok("t");
        assert!(
            Registry::parse(&table(&format!(
                r#"{{"path_token":"{t}","slack_user_id":"U","signing_secret":"",
                    "topic":"a","block_actions_topic":"b"}}"#
            )))
            .is_err(),
            "empty signing secret"
        );
        assert!(
            Registry::parse(&table(&format!(
                r#"{{"path_token":"{t}","slack_user_id":"U","signing_secret":"s",
                    "topic":"same","block_actions_topic":"same"}}"#
            )))
            .is_err(),
            "one topic for both kinds"
        );
        // An unknown field is a typo in a secret nobody can diff; refuse it
        // rather than run with a key that turned out to do nothing.
        assert!(
            Registry::parse(&table(&format!(
                r#"{{"path_token":"{t}","slack_user_id":"U","signing_secret":"s",
                    "topic":"a","block_actions_topic":"b","signing_secrets":"oops"}}"#
            )))
            .is_err(),
            "unknown field"
        );
    }

    /// **Two operators pointed at one topic is refused, across all four
    /// pairings.**
    ///
    /// The runtime symptom is the one this whole function exists for: nothing
    /// is logged, the container starts, and one person's deliveries — their
    /// channel ids, their `ts`, their reactions — arrive in the other's
    /// subscription. Decision 6 separates operators by path *and* topic, and
    /// until now only the path half was enforced.
    ///
    /// The cross pairings are the reason both kinds share **one** set. A set
    /// per column accepts "A's `topic` is B's `block_actions_topic`", which
    /// then puts presses into a subscription whose retention is measured in
    /// days — the premise decision 5 splits the topics to hold.
    #[test]
    fn two_rows_sharing_a_topic_are_refused() {
        // Through `tok()`, like every other fixture here: `validate` checks
        // the path-token floor **before** it looks at topics, so a short
        // `tok-a` would fail each case below on length and prove nothing about
        // the collision it is named for.
        let (a, b) = (tok("tok-a"), tok("tok-b"));
        let shared = |a_events, a_presses, b_events, b_presses| {
            table(&format!(
                r#"{{"path_token":"{a}","slack_user_id":"U_A","signing_secret":"s",
                     "topic":"{a_events}","block_actions_topic":"{a_presses}"}},
                   {{"path_token":"{b}","slack_user_id":"U_B","signing_secret":"s",
                     "topic":"{b_events}","block_actions_topic":"{b_presses}"}}"#
            ))
        };
        let t = "projects/p/topics";
        // Both column names travel with the case. The message says which
        // column each side used, and a regression that dropped or swapped
        // them would leave all four cases green on the ids and the topic
        // alone — which is exactly what the cross-column rows are here for.
        for (case, field, owner_field, raw) in [
            (
                "topic == topic",
                "topic",
                "topic",
                shared(
                    &format!("{t}/shared"),
                    &format!("{t}/a-a"),
                    &format!("{t}/shared"),
                    &format!("{t}/b-a"),
                ),
            ),
            (
                "block_actions_topic == block_actions_topic",
                "block_actions_topic",
                "block_actions_topic",
                shared(
                    &format!("{t}/a-e"),
                    &format!("{t}/shared"),
                    &format!("{t}/b-e"),
                    &format!("{t}/shared"),
                ),
            ),
            (
                "A's topic == B's block_actions_topic",
                "block_actions_topic",
                "topic",
                shared(
                    &format!("{t}/shared"),
                    &format!("{t}/a-a"),
                    &format!("{t}/b-e"),
                    &format!("{t}/shared"),
                ),
            ),
            (
                "A's block_actions_topic == B's topic",
                "topic",
                "block_actions_topic",
                shared(
                    &format!("{t}/a-e"),
                    &format!("{t}/shared"),
                    &format!("{t}/shared"),
                    &format!("{t}/b-a"),
                ),
            ),
        ] {
            let err = Registry::parse(&raw).expect_err(case);
            // The message has to name the two operators and the topic: the
            // table is a secret nobody can diff in a review, so "some rows
            // collide" leaves the reader to find them by eye.
            let text = err.to_string();
            for needle in ["U_A", "U_B", "shared"] {
                assert!(text.contains(needle), "{case}: `{needle}` missing: {text}");
            }
            // Matched as the rendered `id (column)` pairs, not as bare
            // substrings: `block_actions_topic` *contains* `topic`, so a plain
            // `contains("topic")` passes on a message naming the other column
            // and the cross-column cases would prove nothing.
            assert!(
                text.contains(&format!("`U_B` (`{field}`)")),
                "{case}: the second row's column must be named: {text}"
            );
            assert!(
                text.contains(&format!("`U_A` (`{owner_field}`)")),
                "{case}: the first row's column must be named too: {text}"
            );
        }

        // Distinct topics across the board still parse — the check must not
        // refuse the arrangement the OpenTofu module produces.
        Registry::parse(&table(&format!("{},{}", row(&a, "U_A"), row(&b, "U_B"))))
            .expect("two fully separated operators");
    }

    /// **The 32-character floor holds in the container, not just in OpenTofu.**
    ///
    /// `tofu/variables.tf` refuses a short `path_token`, but the table does not
    /// have to come through OpenTofu: `load_registrations()` reads it from
    /// `REGISTRATIONS_PATH` or `REGISTRATIONS`, and an operator can write
    /// either by hand. On that route nobody was checking, and the path *is*
    /// the routing credential on an endpoint with no IAM in front of it —
    /// short enough to guess, with no symptom until somebody does.
    ///
    /// Asserted on the boundary in both directions, because an off-by-one here
    /// is invisible: it neither fails to start nor logs anything.
    #[test]
    fn a_short_path_token_is_refused_at_the_documented_floor() {
        let one_short = "a".repeat(MIN_PATH_TOKEN_CHARS - 1);
        let err = Registry::parse(&table(&row(&one_short, "U_A")))
            .expect_err("31 characters is below the floor");
        let text = err.to_string();
        // Name the row and say how to fix it: the table is a secret nobody
        // diffs in review, so "a row is too short" leaves the reader guessing.
        assert!(text.contains("U_A"), "{text}");
        assert!(text.contains("openssl rand -hex 24"), "{text}");

        let exactly = "a".repeat(MIN_PATH_TOKEN_CHARS);
        Registry::parse(&table(&row(&exactly, "U_A")))
            .expect("32 characters is the floor, not one over");
    }
}
