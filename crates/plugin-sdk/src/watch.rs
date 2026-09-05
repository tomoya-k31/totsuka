//! The channel watch trigger (#616): `trigger = { channel = … }`.
//!
//! A watch workflow turns **every top-level post in one channel** into a task
//! (#615) — no mention, no reaction, no other gesture. That makes it the one
//! trigger where *writing a message* is the whole gesture, so who may write it
//! is a security boundary, not a convenience: the reaction trigger's
//! operator-only rule ([ADR-0068]) applies here too, and the `from` key below
//! is its single, explicit relaxation.
//!
//! What this module owns is everything about the trigger that is not
//! platform-specific: parsing and validating the table, the author gate
//! ([`WatchTrigger::allows`]), the rename check
//! ([`WatchTrigger::name_mismatch`]), the backfill limits and the one-shot
//! backfill pass ([`backfill_pass`]). Receiving the messages (Socket Mode,
//! the Discord Gateway) and normalizing them into [`Task`]s stays in each
//! source.
//!
//! [ADR-0068]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0068-channel-watch-trigger.md

use std::time::{Duration, SystemTime};

use plugin_protocol::Task;
use plugin_protocol::methods::WorkflowInfo;
use serde_json::Value;

use crate::submit::{Submitter, submit_all};

/// One workflow's channel watch trigger, parsed and validated.
///
/// ```toml
/// [[workflows]]
/// name = "clip"
/// trigger = { channel = "C0123ABC", channel_name = "clip", repo = "my-docs" }
/// # from = ["U0AAA"]   # optional: who may trigger besides the operator
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTrigger {
    /// The workflow this trigger belongs to (`[[workflows]].name`), named on
    /// `task/submit`.
    pub workflow: String,
    /// The platform's channel **id** (Slack `C…`, a Discord snowflake). The
    /// id is the authority: names are free to change, ids are not.
    pub channel: String,
    /// The channel's human name, for verification only. The source checks it
    /// against the live name at startup ([`Self::name_mismatch`]) so a rename
    /// is reported instead of silently watching something else.
    pub channel_name: String,
    /// The repository this channel's tasks resolve to (`Task.repo_hint`).
    /// Validated against the Orchestrator's `[[repositories]]` at
    /// `initialize`.
    pub repo: String,
    /// Authors allowed to trigger **besides the operator** (platform user
    /// ids). Kept private so the only way to consult it is
    /// [`allows`](Self::allows), which always admits the operator — an
    /// allowlist that could lock the operator out would be a misconfiguration
    /// with no use.
    from: Vec<String>,
}

impl WatchTrigger {
    /// Parse one workflow's trigger table. `Ok(None)` means the workflow is
    /// not a watch workflow (no `channel` key); errors mean it tried to be
    /// one and got the table wrong.
    ///
    /// All problems are collected rather than stopping at the first: an
    /// operator fixing config wants the whole list.
    pub fn parse(trigger: &Value, workflow: &str) -> Result<Option<Self>, Vec<String>> {
        let Some(channel_value) = trigger.get("channel") else {
            return Ok(None);
        };
        let mut errors = Vec::new();

        // Ids are strings even when they look numeric: a Discord snowflake
        // exceeds a TOML integer reader's expectations at no benefit, and
        // Slack ids never parse as numbers anyway. One spelling, quoted.
        let channel = match channel_value.as_str() {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                errors.push(format!(
                    "workflow `{workflow}` has `trigger.channel = {channel_value}`, which is not \
                     a channel id → write the platform's channel id as a string, e.g. \
                     `channel = \"C0123ABC\"` (quote it even when it is numeric)"
                ));
                String::new()
            }
        };

        // A trigger names exactly one kind. `reaction` alongside `channel`
        // would leave first-match to decide which one the operator meant.
        if trigger.get("reaction").is_some() {
            errors.push(format!(
                "workflow `{workflow}` has both `channel` and `reaction` in its trigger → a \
                 workflow triggers on exactly one kind; split them into two workflows"
            ));
        }

        // The name is required *because* it is redundant: it is the half a
        // human can read, and the half the startup check compares against the
        // live channel so a rename is noticed (#615).
        let channel_name = match trigger.get("channel_name").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                errors.push(format!(
                    "workflow `{workflow}` watches a channel without `channel_name` → write the \
                     channel's current name next to the id, e.g. `channel_name = \"clip\"`; it is \
                     checked against the live name at startup so a rename cannot silently point \
                     the watch at something else"
                ));
                String::new()
            }
        };

        let repo = match trigger.get("repo").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                errors.push(format!(
                    "workflow `{workflow}` watches a channel without `repo` → name the \
                     `[[repositories]]` entry this channel's tasks belong to, e.g. \
                     `repo = \"my-docs\"`"
                ));
                String::new()
            }
        };

        let from = match trigger.get("from") {
            None => Vec::new(),
            Some(Value::Array(items)) if items.is_empty() => {
                errors.push(format!(
                    "workflow `{workflow}` has `trigger.from = []` → the operator can always \
                     trigger, so an empty list adds nobody; drop the key, or name who else may \
                     trigger"
                ));
                Vec::new()
            }
            Some(Value::Array(items)) => {
                let mut from = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(s) if !s.trim().is_empty() => from.push(s.trim().to_string()),
                        _ => errors.push(format!(
                            "workflow `{workflow}` has {item} inside `trigger.from`, which is not \
                             a user id → write the platform's user ids as strings, e.g. \
                             `from = [\"U0AAA\"]`"
                        )),
                    }
                }
                from
            }
            Some(other) => {
                errors.push(format!(
                    "workflow `{workflow}` has a `trigger.from` of {other}, which is not an array \
                     → write the user ids as a list, e.g. `from = [\"U0AAA\", \"U0BBB\"]`"
                ));
                Vec::new()
            }
        };

        if errors.is_empty() {
            Ok(Some(Self {
                workflow: workflow.to_string(),
                channel,
                channel_name,
                repo,
                from,
            }))
        } else {
            Err(errors)
        }
    }

    /// Whether a post by `author` may trigger this workflow. `operator` is
    /// the operator's own platform user id.
    ///
    /// The operator is **always** allowed — `from` extends the set, it never
    /// replaces it ([ADR-0068]). Ids are compared exactly: these are
    /// platform-issued identifiers, not human-typed logins, so case-folding
    /// would only widen the match for no input that legitimately needs it.
    ///
    /// [ADR-0068]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0068-channel-watch-trigger.md
    pub fn allows(&self, author: &str, operator: &str) -> bool {
        author == operator || self.from.iter().any(|id| id == author)
    }

    /// The warning to log when the live channel name does not match the
    /// configured `channel_name` — one wording for every source, so the
    /// symptom reads the same wherever it appears. `None` means they match.
    pub fn name_mismatch(&self, live_name: &str) -> Option<String> {
        (live_name != self.channel_name).then(|| {
            format!(
                "workflow `{}` watches channel `{}` as `{}`, but its name is now `{live_name}` → \
                 if the channel was renamed, update `channel_name`; if this is a different \
                 channel than intended, fix `channel`",
                self.workflow, self.channel, self.channel_name
            )
        })
    }
}

/// Resolve every workflow's watch trigger at `initialize`.
///
/// `repo_names` is the Orchestrator's `[[repositories]]` (from
/// `InitializeParams.repositories`); a `repo` outside it is refused here, at
/// startup, rather than surfacing later as a task no repository claims.
/// `operator` is the operator's own platform user id, and `identity_key`
/// names the setting that supplies it — required as soon as any watch
/// trigger exists, because the default author gate is "the operator only"
/// and without an identity there is nobody to compare against.
///
/// `Err` carries `CONFIG_INVALID` messages, all of them.
pub fn resolve(
    workflows: &[WorkflowInfo],
    repo_names: &[&str],
    operator: Option<&str>,
    identity_key: &str,
) -> Result<Vec<WatchTrigger>, Vec<String>> {
    let mut errors = Vec::new();
    let mut triggers: Vec<WatchTrigger> = Vec::new();

    for wf in workflows {
        let trigger = match WatchTrigger::parse(&wf.trigger, &wf.workflow) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(mut e) => {
                errors.append(&mut e);
                continue;
            }
        };
        if !repo_names.contains(&trigger.repo.as_str()) {
            errors.push(format!(
                "workflow `{}` watches for repo `{}`, which is not in `[[repositories]]` → \
                 configured repositories: {}",
                trigger.workflow,
                trigger.repo,
                if repo_names.is_empty() {
                    "(none)".to_string()
                } else {
                    repo_names
                        .iter()
                        .map(|r| format!("`{r}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        // One channel selects one workflow. Two watches on the same channel
        // is the same failure as two workflows claiming one reaction emoji:
        // first-match would pick one and say nothing.
        if let Some(first) = triggers.iter().find(|t| t.channel == trigger.channel) {
            errors.push(format!(
                "workflows `{}` and `{}` both watch channel `{}` → one channel selects one \
                 workflow; give them different channels or merge the workflows",
                first.workflow, trigger.workflow, trigger.channel
            ));
            continue;
        }
        triggers.push(trigger);
    }

    if !triggers.is_empty() && operator.is_none() {
        errors.push(format!(
            "a workflow watches a channel, but `{identity_key}` is not set → the author gate \
             defaults to the operator alone, and there is no operator to compare against; set \
             `{identity_key}`"
        ));
    }

    if errors.is_empty() {
        Ok(triggers)
    } else {
        Err(errors)
    }
}

/// How far back a startup backfill reaches: at most [`count`](Self::count)
/// messages per channel, none older than [`max_age`](Self::max_age).
///
/// Both bounds exist because over-fetching is *harmless* while under-fetching
/// loses posts silently (#615): re-submitting a message the ledger already
/// holds is an idempotent `duplicate` ack, so the plugin keeps **no cursor**
/// and simply refetches on every startup. The age bound is what keeps that
/// safe to point at a channel with months of history — without it, the first
/// startup would turn the last `count` historical posts into tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackfillLimits {
    /// Most messages fetched per channel. Config key: `watch_backfill_limit`.
    pub count: u32,
    /// Oldest message considered. Config key: `watch_backfill_max_age_hours`.
    pub max_age: Duration,
}

/// Default [`BackfillLimits::count`].
pub const DEFAULT_BACKFILL_COUNT: u32 = 100;
/// Default [`BackfillLimits::max_age`], in hours.
pub const DEFAULT_BACKFILL_MAX_AGE_HOURS: u64 = 24;

impl Default for BackfillLimits {
    fn default() -> Self {
        Self {
            count: DEFAULT_BACKFILL_COUNT,
            max_age: Duration::from_secs(DEFAULT_BACKFILL_MAX_AGE_HOURS * 3600),
        }
    }
}

impl BackfillLimits {
    /// Build from the plugin's config keys (`watch_backfill_limit`,
    /// `watch_backfill_max_age_hours`), `None` meaning the default. Zero is
    /// refused for both: it would spell "no backfill", and disabling recovery
    /// deserves a clearer switch than a zero nobody reads as one.
    pub fn new(count: Option<u32>, max_age_hours: Option<u64>) -> Result<Self, String> {
        let defaults = Self::default();
        if count == Some(0) {
            return Err(
                "`watch_backfill_limit = 0` would fetch nothing → drop the key for the default, \
                 or set how many recent messages to recover per channel"
                    .to_string(),
            );
        }
        if max_age_hours == Some(0) {
            return Err(
                "`watch_backfill_max_age_hours = 0` would consider no message recent → drop the \
                 key for the default, or set how old a missed message may be and still be \
                 recovered"
                    .to_string(),
            );
        }
        Ok(Self {
            count: count.unwrap_or(defaults.count),
            max_age: max_age_hours
                .map(|h| Duration::from_secs(h * 3600))
                .unwrap_or(defaults.max_age),
        })
    }

    /// The instant before which a message is too old to backfill.
    pub fn cutoff(&self, now: SystemTime) -> SystemTime {
        now.checked_sub(self.max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

/// One startup backfill pass: fetch each watched channel's recent messages
/// and submit them all, relying on the Orchestrator's idempotent ingest to
/// drop what was already seen.
///
/// `fetch` returns the tasks for one channel — already filtered to `limits`
/// (a source with a server-side `oldest` parameter should push the cutoff
/// into the API call) and already normalized, the same contract as
/// [`poll_loop`](crate::poll_loop)'s fetch. A failing channel is logged and
/// skipped, not fatal: backfill is recovery, and failing startup over it
/// would lose the live events too.
pub async fn backfill_pass<S, F, Fut>(
    triggers: &[WatchTrigger],
    limits: &BackfillLimits,
    submitter: &S,
    mut fetch: F,
) where
    S: Submitter,
    F: FnMut(&WatchTrigger, &BackfillLimits) -> Fut,
    Fut: Future<Output = Result<Vec<Task>, String>>,
{
    for trigger in triggers {
        let tasks = match fetch(trigger, limits).await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!(
                    workflow = %trigger.workflow,
                    channel = %trigger.channel,
                    "backfill fetch failed (channel skipped): {e}"
                );
                continue;
            }
        };
        submit_all(submitter, tasks, &trigger.workflow).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wf(name: &str, trigger: Value) -> WorkflowInfo {
        WorkflowInfo {
            workflow: name.to_string(),
            trigger,
            instructions_kind: None,
            task_id_prefix: None,
            options: serde_json::Map::new(),
        }
    }

    fn full() -> Value {
        json!({ "channel": "C1", "channel_name": "clip", "repo": "docs" })
    }

    // ------------------------------------------------------------------ parse

    #[test]
    fn a_trigger_without_channel_is_not_a_watch() {
        for trigger in [
            json!({}),
            json!({ "reaction": "eyes" }),
            json!({ "status": "Todo" }),
        ] {
            assert_eq!(WatchTrigger::parse(&trigger, "wf"), Ok(None), "{trigger}");
        }
    }

    #[test]
    fn a_full_trigger_parses_and_from_defaults_to_empty() {
        let t = WatchTrigger::parse(&full(), "clip").unwrap().unwrap();
        assert_eq!(t.workflow, "clip");
        assert_eq!(t.channel, "C1");
        assert_eq!(t.channel_name, "clip");
        assert_eq!(t.repo, "docs");
        // The default gate: operator only.
        assert!(t.allows("U_OP", "U_OP"));
        assert!(!t.allows("U_OTHER", "U_OP"));
    }

    #[test]
    fn missing_channel_name_and_repo_are_both_reported() {
        let errors = WatchTrigger::parse(&json!({ "channel": "C1" }), "wf").unwrap_err();
        assert_eq!(errors.len(), 2, "got {errors:?}");
        assert!(errors[0].contains("channel_name"), "got {errors:?}");
        assert!(errors[1].contains("`repo`"), "got {errors:?}");
    }

    #[test]
    fn a_numeric_channel_id_is_refused_with_the_quoting_fix() {
        // A Discord snowflake written unquoted arrives as a number.
        let mut trigger = full();
        trigger["channel"] = json!(1234567890);
        let errors = WatchTrigger::parse(&trigger, "wf").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("quote"), "got {errors:?}");
    }

    #[test]
    fn channel_alongside_reaction_is_refused() {
        let mut trigger = full();
        trigger["reaction"] = json!("eyes");
        let errors = WatchTrigger::parse(&trigger, "wf").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("exactly one kind"), "got {errors:?}");
    }

    #[test]
    fn an_empty_from_is_refused_rather_than_read_as_operator_only() {
        let mut trigger = full();
        trigger["from"] = json!([]);
        let errors = WatchTrigger::parse(&trigger, "wf").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("adds nobody"), "got {errors:?}");
    }

    #[test]
    fn a_non_array_from_and_non_string_entries_are_refused() {
        let mut trigger = full();
        trigger["from"] = json!("U0AAA");
        let errors = WatchTrigger::parse(&trigger, "wf").unwrap_err();
        assert!(errors[0].contains("not an array"), "got {errors:?}");

        let mut trigger = full();
        trigger["from"] = json!(["U0AAA", 42]);
        let errors = WatchTrigger::parse(&trigger, "wf").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("42"), "got {errors:?}");
    }

    // ----------------------------------------------------------------- allows

    #[test]
    fn from_extends_the_gate_and_never_locks_the_operator_out() {
        let mut trigger = full();
        trigger["from"] = json!(["U0AAA"]);
        let t = WatchTrigger::parse(&trigger, "wf").unwrap().unwrap();
        assert!(t.allows("U0AAA", "U_OP"), "listed author");
        assert!(t.allows("U_OP", "U_OP"), "the operator, though unlisted");
        assert!(!t.allows("U0BBB", "U_OP"), "everyone else");
    }

    #[test]
    fn ids_are_compared_exactly_not_case_folded() {
        // Platform ids are exact identifiers; `u0aaa` is not `U0AAA`.
        let mut trigger = full();
        trigger["from"] = json!(["U0AAA"]);
        let t = WatchTrigger::parse(&trigger, "wf").unwrap().unwrap();
        assert!(!t.allows("u0aaa", "U_OP"));
        assert!(!t.allows("u_op", "U_OP"));
    }

    // ---------------------------------------------------------- name_mismatch

    #[test]
    fn a_matching_name_is_quiet_and_a_rename_names_both_fixes() {
        let t = WatchTrigger::parse(&full(), "wf").unwrap().unwrap();
        assert_eq!(t.name_mismatch("clip"), None);
        let warning = t.name_mismatch("clipboard").unwrap();
        assert!(warning.contains("`clip`"), "got {warning}");
        assert!(warning.contains("`clipboard`"), "got {warning}");
        assert!(warning.contains("channel_name"), "got {warning}");
        assert!(warning.contains("`channel`"), "got {warning}");
    }

    // ---------------------------------------------------------------- resolve

    #[test]
    fn resolve_collects_triggers_and_ignores_other_workflows() {
        let workflows = [
            wf("mention", json!({})),
            wf("clip", full()),
            wf("eyes", json!({ "reaction": "eyes" })),
        ];
        let triggers = resolve(&workflows, &["docs"], Some("U_OP"), "target_user_id").unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].workflow, "clip");
    }

    #[test]
    fn an_unknown_repo_is_refused_and_names_the_candidates() {
        let workflows = [wf("clip", full())];
        let errors = resolve(
            &workflows,
            &["totsuka", "dotfiles"],
            Some("U_OP"),
            "target_user_id",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("`docs`"), "got {errors:?}");
        assert!(errors[0].contains("`totsuka`"), "got {errors:?}");
        assert!(errors[0].contains("`dotfiles`"), "got {errors:?}");
    }

    #[test]
    fn two_watches_on_one_channel_are_refused() {
        let workflows = [wf("clip", full()), wf("clip2", full())];
        let errors = resolve(&workflows, &["docs"], Some("U_OP"), "target_user_id").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("`clip`"), "got {errors:?}");
        assert!(errors[0].contains("`clip2`"), "got {errors:?}");
    }

    #[test]
    fn a_watch_without_an_operator_identity_is_refused() {
        let workflows = [wf("clip", full())];
        let errors = resolve(&workflows, &["docs"], None, "operator_user_id").unwrap_err();
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("operator_user_id"), "got {errors:?}");
        // …but with no watch trigger, no identity is demanded.
        assert_eq!(
            resolve(&[wf("m", json!({}))], &[], None, "operator_user_id"),
            Ok(vec![])
        );
    }

    #[test]
    fn every_problem_is_reported_in_one_pass() {
        let workflows = [
            wf("a", json!({ "channel": "C1" })), // missing name + repo
            wf("b", full()),                     // repo not configured
        ];
        let errors = resolve(&workflows, &[], Some("U_OP"), "target_user_id").unwrap_err();
        assert_eq!(errors.len(), 3, "got {errors:?}");
    }

    // --------------------------------------------------------------- backfill

    #[test]
    fn limits_default_and_reject_zero() {
        let limits = BackfillLimits::default();
        assert_eq!(limits.count, 100);
        assert_eq!(limits.max_age, Duration::from_secs(24 * 3600));
        assert_eq!(BackfillLimits::new(None, None), Ok(limits));
        assert_eq!(
            BackfillLimits::new(Some(50), Some(6)),
            Ok(BackfillLimits {
                count: 50,
                max_age: Duration::from_secs(6 * 3600)
            })
        );
        assert!(BackfillLimits::new(Some(0), None).is_err());
        assert!(BackfillLimits::new(None, Some(0)).is_err());
    }

    #[test]
    fn cutoff_is_max_age_before_now() {
        let limits = BackfillLimits::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 3600);
        assert_eq!(limits.cutoff(now), now - Duration::from_secs(24 * 3600));
    }

    #[tokio::test]
    async fn a_failing_channel_is_skipped_not_fatal() {
        use std::sync::Mutex;

        struct Recorder(Mutex<Vec<(String, String)>>);
        impl Submitter for Recorder {
            async fn submit(&self, task: Task, workflow: &str) -> crate::SubmitOutcome {
                self.0.lock().unwrap().push((task.id, workflow.to_string()));
                crate::SubmitOutcome::Accepted
            }
        }

        let t1 = WatchTrigger::parse(&full(), "clip").unwrap().unwrap();
        let mut bad = full();
        bad["channel"] = json!("C2");
        let t2 = WatchTrigger::parse(&bad, "clip2").unwrap().unwrap();

        let task = |id: &str| Task {
            id: id.to_string(),
            source: "slack".into(),
            title: "t".into(),
            body: None,
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: None,
            instructions: None,
        };

        let recorder = Recorder(Mutex::new(Vec::new()));
        backfill_pass(
            &[t1, t2],
            &BackfillLimits::default(),
            &recorder,
            |trigger, _limits| {
                let failing = trigger.channel == "C1";
                let tasks = vec![task(&format!("{}:1", trigger.channel))];
                async move {
                    if failing {
                        Err("rate limited".to_string())
                    } else {
                        Ok(tasks)
                    }
                }
            },
        )
        .await;

        // C1 failed and was skipped; C2 still submitted under its workflow.
        assert_eq!(
            *recorder.0.lock().unwrap(),
            vec![("C2:1".to_string(), "clip2".to_string())]
        );
    }
}
