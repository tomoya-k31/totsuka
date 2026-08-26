//! Exclusion-claim adjudication for `task/claim` (#556, ADR-0059).
//!
//! The mechanics of *writing* the claim (self-assign, read-back, removal on
//! loss) live on [`GithubClient`](crate::client::GithubClient); this module
//! holds the pure part — the queries and the adjudication rule — so the rule
//! that decides who runs a task is testable without a transport.
//!
//! ## The rule
//!
//! Among the issue's **current** assignees, each one's *effective event* is
//! their most recent `AssignedEvent` (an assign → unassign → re-assign cycle
//! makes the last one the event that created the current tenure). The winner
//! is the assignee whose effective event is **oldest**; a `createdAt` tie
//! breaks on the event node id. The ordering is server-issued, so every
//! instance converges on the same answer regardless of when it read.
//!
//! Judged on the **assignee's login, not the actor's**: the actor is whoever
//! held the token, and a token acting for a different `github_login` must not
//! shift the outcome.

use serde_json::Value;

/// One `AssignedEvent`, as read from the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedEvent {
    /// The assignee's login (the person the event put on the issue).
    pub login: String,
    /// Server-issued creation time, ISO-8601 UTC.
    ///
    /// Compared **lexicographically**. That is safe here — and was not in
    /// #478 — because GitHub's GraphQL `DateTime` for these events is a
    /// fixed-width `YYYY-MM-DDTHH:MM:SSZ` from a single writer, so string
    /// order equals time order. A variable-length fraction would break this;
    /// none of these events carries one (measured, #556 Phase 0).
    pub created_at: String,
    /// The event's node id — the tie-breaker for same-second events.
    pub id: String,
}

/// The claim read: the issue's current assignees plus its assignment history.
#[derive(Debug, Clone, Default)]
pub struct ClaimState {
    /// Current assignee logins, in API order (order is not meaningful).
    pub assignees: Vec<String>,
    /// `ASSIGNED_EVENT` timeline entries, unordered as far as this module is
    /// concerned — the adjudication sorts what it needs.
    pub events: Vec<AssignedEvent>,
}

impl ClaimState {
    /// Whether `login` is a current assignee (case-insensitive, like every
    /// login comparison in this plugin).
    pub fn holds(&self, login: &str) -> bool {
        self.assignees.iter().any(|a| a.eq_ignore_ascii_case(login))
    }
}

/// Why the adjudication could not decide.
#[derive(Debug, PartialEq, Eq)]
pub enum AdjudicationError {
    /// A current assignee has no visible `AssignedEvent` yet. Eventual
    /// consistency: their event exists (something made them an assignee) but
    /// this read did not see it. **Deliberately an error, not a forfeit** —
    /// if both racers yielded on mutual invisibility the task would end up
    /// held by nobody and re-ingested by nobody. An error keeps the task
    /// queued and the next cycle reads again, by which time the event is
    /// there: a delay, never a wrong answer.
    MissingEvent(String),
}

/// Decide which current assignee holds the task. `Ok` is the winner's login.
///
/// Deterministic over one read: everyone who sees the same `(assignees,
/// events)` names the same winner, and because the ordering is the server's,
/// later readers seeing *more* events still name the same winner as long as
/// the assignee set is the same.
pub fn adjudicate(state: &ClaimState) -> Result<&str, AdjudicationError> {
    let mut winner: Option<(&AssignedEvent, &str)> = None;
    for login in &state.assignees {
        // The effective event: the latest one naming this login.
        let effective = state
            .events
            .iter()
            .filter(|e| e.login.eq_ignore_ascii_case(login))
            .max_by(|a, b| {
                (a.created_at.as_str(), a.id.as_str()).cmp(&(b.created_at.as_str(), b.id.as_str()))
            })
            .ok_or_else(|| AdjudicationError::MissingEvent(login.clone()))?;
        // Oldest effective event wins; ties break on the event id.
        let better = match &winner {
            None => true,
            Some((w, _)) => {
                (effective.created_at.as_str(), effective.id.as_str())
                    < (w.created_at.as_str(), w.id.as_str())
            }
        };
        if better {
            winner = Some((effective, login.as_str()));
        }
    }
    winner
        .map(|(_, login)| login)
        .ok_or_else(|| AdjudicationError::MissingEvent("(no assignees)".into()))
}

/// The claim read query: current assignees + assignment history in one call.
/// `last: 100` reads from the tail — with at most 10 assignees the effective
/// events are always inside it — and the result order is **not relied on**
/// (the adjudication sorts).
pub const CLAIM_READ_QUERY: &str = r#"query($id: ID!) {
  node(id: $id) {
    ... on Issue {
      assignees(first: 10) { nodes { login } }
      timelineItems(last: 100, itemTypes: [ASSIGNED_EVENT]) {
        nodes { ... on AssignedEvent {
          id createdAt
          assignee { ... on User { login } }
        } }
      }
    }
  }
}"#;

/// Resolves the operator's user node id for the assign mutations.
pub const USER_ID_QUERY: &str = r#"query($login: String!) { user(login: $login) { id } }"#;

/// Self-assign (idempotent per GitHub: assigning an existing assignee again
/// changes nothing).
pub const ADD_ASSIGNEES_MUTATION: &str = r#"mutation($a: ID!, $u: [ID!]!) {
  addAssigneesToAssignable(input: {assignableId: $a, assigneeIds: $u}) { clientMutationId }
}"#;

/// Remove **only this operator's own** assignment — the loser steps aside;
/// nobody ever removes anybody else.
pub const REMOVE_ASSIGNEES_MUTATION: &str = r#"mutation($a: ID!, $u: [ID!]!) {
  removeAssigneesFromAssignable(input: {assignableId: $a, assigneeIds: $u}) { clientMutationId }
}"#;

/// Parse the [`CLAIM_READ_QUERY`] response's `data`. `None` when the node is
/// missing or not an Issue (deleted, or the id is something else entirely).
pub fn parse_claim_state(data: &Value) -> Option<ClaimState> {
    let node = data.get("node")?;
    // A deleted issue answers `"node": null`; an id of another type answers
    // an object without these fields. Both are "cannot read", not "empty".
    let assignee_nodes = node.get("assignees")?.get("nodes")?.as_array()?;
    let assignees = assignee_nodes
        .iter()
        .filter_map(|a| a["login"].as_str().map(str::to_string))
        .collect();
    let events = node["timelineItems"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some(AssignedEvent {
                // An assignee that is not a User (bot, mannequin) has no
                // `login` in this selection; its event cannot belong to a
                // configured `github_login`, so dropping it is correct.
                login: e["assignee"]["login"].as_str()?.to_string(),
                created_at: e["createdAt"].as_str()?.to_string(),
                id: e["id"].as_str()?.to_string(),
            })
        })
        .collect();
    Some(ClaimState { assignees, events })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(login: &str, at: &str, id: &str) -> AssignedEvent {
        AssignedEvent {
            login: login.into(),
            created_at: at.into(),
            id: id.into(),
        }
    }

    #[test]
    fn earliest_assignment_wins() {
        let state = ClaimState {
            assignees: vec!["alice".into(), "bob".into()],
            events: vec![
                ev("bob", "2026-08-25T10:00:05Z", "E2"),
                ev("alice", "2026-08-25T10:00:01Z", "E1"),
            ],
        };
        assert_eq!(adjudicate(&state).unwrap(), "alice");
    }

    #[test]
    fn a_reassignment_forfeits_seniority() {
        // alice assigned first, stepped aside, came back after bob claimed:
        // her *effective* event is the re-assignment, so bob holds the task.
        let state = ClaimState {
            assignees: vec!["alice".into(), "bob".into()],
            events: vec![
                ev("alice", "2026-08-25T10:00:01Z", "E1"),
                ev("bob", "2026-08-25T10:00:05Z", "E2"),
                ev("alice", "2026-08-25T10:00:09Z", "E3"),
            ],
        };
        assert_eq!(adjudicate(&state).unwrap(), "bob");
    }

    #[test]
    fn same_second_breaks_on_event_id() {
        let state = ClaimState {
            assignees: vec!["bob".into(), "alice".into()],
            events: vec![
                ev("bob", "2026-08-25T10:00:01Z", "E_b"),
                ev("alice", "2026-08-25T10:00:01Z", "E_a"),
            ],
        };
        // Identical createdAt: the smaller event node id wins — arbitrary,
        // but the same arbitrary answer for every reader.
        assert_eq!(adjudicate(&state).unwrap(), "alice");
    }

    #[test]
    fn logins_compare_case_insensitively() {
        let state = ClaimState {
            assignees: vec!["Alice".into()],
            events: vec![ev("alice", "2026-08-25T10:00:01Z", "E1")],
        };
        assert_eq!(adjudicate(&state).unwrap(), "Alice");
        assert!(state.holds("ALICE"));
    }

    #[test]
    fn an_invisible_event_is_an_error_not_a_forfeit() {
        let state = ClaimState {
            assignees: vec!["alice".into(), "bob".into()],
            events: vec![ev("alice", "2026-08-25T10:00:01Z", "E1")],
        };
        assert_eq!(
            adjudicate(&state).unwrap_err(),
            AdjudicationError::MissingEvent("bob".into())
        );
    }

    #[test]
    fn unassign_events_do_not_confuse_the_reading() {
        // Only *current* assignees are judged: carol's old event is ignored
        // because she is no longer assigned.
        let state = ClaimState {
            assignees: vec!["bob".into()],
            events: vec![
                ev("carol", "2026-08-25T09:00:00Z", "E0"),
                ev("bob", "2026-08-25T10:00:05Z", "E2"),
            ],
        };
        assert_eq!(adjudicate(&state).unwrap(), "bob");
    }

    #[test]
    fn parses_the_read_response() {
        let data = serde_json::json!({
            "node": {
                "assignees": { "nodes": [ { "login": "alice" } ] },
                "timelineItems": { "nodes": [
                    { "id": "E1", "createdAt": "2026-08-25T10:00:01Z",
                      "assignee": { "login": "alice" } },
                    { "id": "E2", "createdAt": "2026-08-25T10:00:02Z",
                      "assignee": {} }
                ] }
            }
        });
        let state = parse_claim_state(&data).unwrap();
        assert_eq!(state.assignees, vec!["alice"]);
        // The non-User assignee's event dropped; alice's kept.
        assert_eq!(state.events.len(), 1);
        assert!(state.holds("alice"));
    }

    #[test]
    fn a_deleted_issue_is_unreadable_not_empty() {
        assert!(parse_claim_state(&serde_json::json!({ "node": null })).is_none());
    }
}
