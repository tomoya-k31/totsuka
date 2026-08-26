//! The `trigger.assignee` condition (#572).
//!
//! Ingest gating used to be a plugin-wide constant — "unassigned, or assigned
//! to me" (F-08) — with no way to say it per workflow. That makes one useful
//! arrangement unwritable: **leave the unassigned tasks to people, and start
//! only on the ones a human handed over by assigning them.** The first half was
//! the default; the second half had no switch.
//!
//! [`AssigneeFilter`] is that switch, and it *replaces* the old constant rather
//! than sitting in front of it: when the key is absent the filter is
//! `["@me", "@none"]`, which is the old rule exactly. One code path, so a
//! written condition can never be overruled by an unwritten one.

use plugin_protocol::methods::WorkflowInfo;
use serde_json::Value;

/// One alternative in the condition. The whole condition is their OR.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    /// The operator themself (`@me`) — needs an identity to compare against.
    Me,
    /// Nobody is assigned (`@none`).
    Unassigned,
    /// This exact login / user id.
    Named(String),
}

/// A parsed `trigger.assignee` condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssigneeFilter {
    /// `None` is `@any`: no condition at all.
    terms: Option<Vec<Term>>,
    /// Whether the operator wrote the key. The default is a real condition, so
    /// this is not derivable from `terms` — and the errors that only apply to
    /// a written condition need to tell the two apart.
    explicit: bool,
}

impl Default for AssigneeFilter {
    /// The pre-#572 gate: unassigned, or assigned to the operator.
    fn default() -> Self {
        Self {
            terms: Some(vec![Term::Me, Term::Unassigned]),
            explicit: false,
        }
    }
}

impl AssigneeFilter {
    /// Parse the `assignee` value out of a trigger table.
    ///
    /// The special words carry an `@` so they cannot collide with a real
    /// login: `me`, `none` and `any` are all names a GitHub account can have,
    /// and `@` is not a character one can contain. It also reads like GitHub's
    /// own search syntax (`assignee:@me`).
    ///
    /// | value | meaning |
    /// |---|---|
    /// | absent | `["@me", "@none"]` — the pre-#572 gate |
    /// | `"@me"` | the operator is among the assignees |
    /// | `"@none"` | nobody is assigned |
    /// | `"@any"` | no condition — **other people's tasks are ingested too** |
    /// | `"<login>"` | that login is among the assignees |
    /// | array | any of the above (OR) |
    ///
    /// `Err` carries a message naming the workflow; the caller fails
    /// `initialize` with it. Rejected: an empty array (matches nothing, so it
    /// is a mistake rather than a filter), `@any` inside an array (it would
    /// make its neighbours dead text), and an unknown `@word` (which is how a
    /// mistyped `@mee` gets caught rather than read as a login).
    pub fn parse(trigger: &Value, workflow: &str) -> Result<Self, String> {
        let Some(value) = trigger.get("assignee") else {
            return Ok(Self::default());
        };
        let terms = match value {
            Value::String(s) => {
                if s == "@any" {
                    return Ok(Self {
                        terms: None,
                        explicit: true,
                    });
                }
                vec![Self::term(s, workflow)?]
            }
            Value::Array(items) => {
                if items.is_empty() {
                    return Err(format!(
                        "workflow `{workflow}` has `trigger.assignee = []`, which matches no task \
                         → drop the key to keep the default (`[\"@me\", \"@none\"]`), or name who \
                         may hold the task"
                    ));
                }
                let mut terms = Vec::with_capacity(items.len());
                for item in items {
                    let Some(s) = item.as_str() else {
                        return Err(Self::not_a_string(workflow, item));
                    };
                    if s == "@any" {
                        return Err(format!(
                            "workflow `{workflow}` lists `\"@any\"` inside `trigger.assignee` \
                             alongside other values → `@any` is the absence of a condition, so it \
                             would make them dead text; write `assignee = \"@any\"` on its own or \
                             drop it"
                        ));
                    }
                    terms.push(Self::term(s, workflow)?);
                }
                terms
            }
            other => return Err(Self::not_a_string(workflow, other)),
        };
        Ok(Self {
            terms: Some(terms),
            explicit: true,
        })
    }

    fn term(s: &str, workflow: &str) -> Result<Term, String> {
        match s {
            "@me" => Ok(Term::Me),
            "@none" => Ok(Term::Unassigned),
            _ if s.starts_with('@') => Err(format!(
                "workflow `{workflow}` has `trigger.assignee = \"{s}\"`, which is not one of \
                 `@me` / `@none` / `@any` → a login never starts with `@`, so this is a typo; \
                 write the login without it if you meant a person"
            )),
            _ => Ok(Term::Named(s.to_string())),
        }
    }

    fn not_a_string(workflow: &str, value: &Value) -> String {
        format!(
            "workflow `{workflow}` has a `trigger.assignee` of {value}, which is neither a string \
             nor an array of strings → write `\"@me\"`, `\"@none\"`, `\"@any\"`, a login, or a \
             list of those"
        )
    }

    /// Whether the operator wrote the key (as opposed to getting the default).
    pub fn is_explicit(&self) -> bool {
        self.explicit
    }

    /// Whether the condition mentions `@me`, and so cannot be evaluated without
    /// knowing who the operator is.
    ///
    /// A named login does **not** need it: matching a name against the assignee
    /// list is the same work whoever is running.
    pub fn needs_self_identity(&self) -> bool {
        self.terms.as_ref().is_some_and(|t| t.contains(&Term::Me))
    }

    /// Whether a task with these `assignees` may be ingested. `me` is the
    /// operator's own login / user id, absent when the source has no setting
    /// for it.
    ///
    /// Comparison ignores ASCII case, matching how GitHub treats logins. Notion
    /// user ids are UUIDs, where it is harmless but not free: their matching
    /// was case-sensitive before this, so a differently-cased id now matches.
    ///
    /// `@me` with no `me` matches nothing. That is only reachable for a
    /// *default* filter — an explicit one is refused at `initialize` — and
    /// there it is the pre-#572 behaviour: with no identity configured, a
    /// source could only ever ingest the unassigned.
    pub fn matches(&self, assignees: &[&str], me: Option<&str>) -> bool {
        let Some(terms) = &self.terms else {
            return true; // `@any`
        };
        terms.iter().any(|term| match term {
            Term::Unassigned => assignees.is_empty(),
            Term::Me => me.is_some_and(|m| Self::holds(assignees, m)),
            Term::Named(name) => Self::holds(assignees, name),
        })
    }

    fn holds(assignees: &[&str], who: &str) -> bool {
        assignees.iter().any(|a| a.eq_ignore_ascii_case(who))
    }
}

/// Parse every workflow's `assignee` at startup, and check the settings it
/// needs.
///
/// The condition is parsed again at every fetch, but a bad one has to stop the
/// plugin **starting**: a per-tick error would leave a workflow that silently
/// never fires, which is the failure this key exists to remove.
///
/// `self_identity` is the operator's own login / user id, absent when the
/// source has no setting for it, and `identity_key` names that setting for the
/// error message. `people_property` says whether the source can read assignees
/// at all — `None` when it always can (GitHub Issues carry them), `Some(false)`
/// when this installation has not mapped one — and `property_key` names it.
///
/// Returns `(errors, warnings)`. Errors are for conditions that cannot be
/// evaluated at all; the caller fails `initialize` with them.
pub fn check(
    workflows: &[WorkflowInfo],
    self_identity: Option<&str>,
    identity_key: &str,
    people_property: Option<bool>,
    property_key: &str,
    status_mints_lane_identity: bool,
) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for wf in workflows {
        let filter = match AssigneeFilter::parse(&wf.trigger, &wf.workflow) {
            Ok(f) => f,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        if !filter.is_explicit() {
            continue;
        }
        if people_property == Some(false) {
            errors.push(format!(
                "workflow `{}` sets `trigger.assignee`, but `{property_key}` is not set → every \
                 task would read as unassigned, so the condition could not do anything; map the \
                 property or drop the key",
                wf.workflow
            ));
        }
        if filter.needs_self_identity() && self_identity.is_none() {
            errors.push(format!(
                "workflow `{}` uses `@me` in `trigger.assignee`, but `{identity_key}` is not set \
                 → there is nobody to compare the assignees against, so the workflow would never \
                 fire; set `{identity_key}`, or name the holder explicitly",
                wf.workflow
            ));
        }
        if status_mints_lane_identity && wf.trigger.get("status").is_none() {
            warnings.push(format!(
                "workflow `{}` triggers on `assignee` with no `status`, so its deliveries carry no \
                 lane identity and it runs at most once per task — re-assigning will not re-run \
                 it. Add a `status` to the trigger if moving the card back should repeat it",
                wf.workflow
            ));
        }
    }
    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Result<AssigneeFilter, String> {
        AssigneeFilter::parse(&v, "wf")
    }

    #[test]
    fn the_default_is_the_pre_572_gate() {
        let f = parse(json!({})).unwrap();
        assert!(!f.is_explicit());
        assert!(f.matches(&[], Some("me")), "unassigned");
        assert!(f.matches(&["me"], Some("me")), "assigned to me");
        assert!(f.matches(&["other", "ME"], Some("me")), "case-insensitive");
        assert!(!f.matches(&["other"], Some("me")), "someone else's");
    }

    #[test]
    fn me_alone_leaves_the_unassigned_to_people() {
        let f = parse(json!({ "assignee": "@me" })).unwrap();
        assert!(f.is_explicit());
        assert!(f.matches(&["me"], Some("me")));
        // The whole point of the issue: this must be false.
        assert!(
            !f.matches(&[], Some("me")),
            "unassigned is not ours to take"
        );
        assert!(!f.matches(&["other"], Some("me")));
    }

    #[test]
    fn none_and_named_and_arrays() {
        let f = parse(json!({ "assignee": "@none" })).unwrap();
        assert!(f.matches(&[], Some("me")));
        assert!(!f.matches(&["me"], Some("me")));

        let f = parse(json!({ "assignee": "teammate" })).unwrap();
        assert!(f.matches(&["teammate"], Some("me")));
        assert!(!f.matches(&["me"], Some("me")));

        let f = parse(json!({ "assignee": ["@none", "teammate"] })).unwrap();
        assert!(f.matches(&[], Some("me")));
        assert!(f.matches(&["teammate"], Some("me")));
        assert!(!f.matches(&["me"], Some("me")));
    }

    #[test]
    fn any_takes_other_peoples_tasks_too() {
        let f = parse(json!({ "assignee": "@any" })).unwrap();
        assert!(f.matches(&["someone-else"], Some("me")));
        assert!(f.matches(&[], None));
    }

    #[test]
    fn a_login_that_looks_like_a_special_word_is_a_login() {
        // `me`, `none` and `any` are all real GitHub accounts; the `@` is what
        // keeps them addressable.
        let f = parse(json!({ "assignee": "any" })).unwrap();
        assert!(f.matches(&["any"], Some("me")), "the user named `any`");
        assert!(!f.matches(&["someone-else"], Some("me")), "not a wildcard");
    }

    #[test]
    fn rejected_shapes_say_what_to_write_instead() {
        for (value, needle) in [
            (json!({ "assignee": [] }), "matches no task"),
            (json!({ "assignee": ["@me", "@any"] }), "dead text"),
            (json!({ "assignee": "@mee" }), "typo"),
            (json!({ "assignee": 1 }), "neither a string"),
            (json!({ "assignee": ["@me", 2] }), "neither a string"),
        ] {
            let err = parse(value.clone()).unwrap_err();
            assert!(err.contains(needle), "{value} → {err}");
            assert!(err.contains("`wf`"), "names the workflow: {err}");
        }
    }

    #[test]
    fn only_me_needs_an_identity() {
        assert!(
            parse(json!({ "assignee": "@me" }))
                .unwrap()
                .needs_self_identity()
        );
        assert!(
            parse(json!({ "assignee": ["@none", "@me"] }))
                .unwrap()
                .needs_self_identity()
        );
        assert!(
            !parse(json!({ "assignee": "@none" }))
                .unwrap()
                .needs_self_identity()
        );
        assert!(
            !parse(json!({ "assignee": "teammate" }))
                .unwrap()
                .needs_self_identity()
        );
        assert!(
            !parse(json!({ "assignee": "@any" }))
                .unwrap()
                .needs_self_identity()
        );
    }

    #[test]
    fn the_lane_warning_is_only_for_sources_that_mint_one() {
        let wf = |trigger| WorkflowInfo {
            workflow: "wf".into(),
            trigger,
            instructions_kind: None,
            task_id_prefix: None,
            options: serde_json::Map::new(),
        };
        let assignee_only = [wf(json!({ "assignee": "@me" }))];

        let (errors, warnings) = check(&assignee_only, Some("me"), "`login`", None, "", true);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("at most once"), "{warnings:?}");

        // A source with no lane identity would not be helped by adding a
        // `status`, so it is not told to.
        let (_, warnings) = check(&assignee_only, Some("me"), "`login`", None, "", false);
        assert!(warnings.is_empty(), "{warnings:?}");

        // With a `status` beside it there is nothing to warn about.
        let paired = [wf(json!({ "status": "Todo", "assignee": "@me" }))];
        let (_, warnings) = check(&paired, Some("me"), "`login`", None, "", true);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unevaluable_conditions_stop_startup() {
        let wf = |trigger| WorkflowInfo {
            workflow: "wf".into(),
            trigger,
            instructions_kind: None,
            task_id_prefix: None,
            options: serde_json::Map::new(),
        };
        // `@me` with nobody configured to be "me".
        let (errors, _) = check(
            &[wf(json!({ "status": "Todo", "assignee": "@me" }))],
            None,
            "`notion_user_id`",
            Some(true),
            "`property_map.assignee`",
            false,
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("notion_user_id"), "{errors:?}");

        // A condition written with no way to read assignees at all.
        let (errors, _) = check(
            &[wf(json!({ "status": "Todo", "assignee": "@none" }))],
            Some("u"),
            "`notion_user_id`",
            Some(false),
            "`property_map.assignee`",
            false,
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("property_map.assignee"), "{errors:?}");

        // The default filter asks for nothing, so neither fires.
        let (errors, warnings) = check(
            &[wf(json!({ "status": "Todo" }))],
            None,
            "`notion_user_id`",
            Some(false),
            "`property_map.assignee`",
            true,
        );
        assert!(
            errors.is_empty() && warnings.is_empty(),
            "{errors:?} {warnings:?}"
        );
    }

    #[test]
    fn without_an_identity_the_default_ingests_only_the_unassigned() {
        let f = parse(json!({})).unwrap();
        assert!(f.matches(&[], None));
        assert!(!f.matches(&["anyone"], None));
    }
}
