//! Small pure functions the run modules share (#464).
//!
//! No `self`, no I/O: string mappings for what gets persisted, the two
//! side-effect predicates the profiles turn on, and the prompt assembly.
//! They live together because more than one sibling needs each of them,
//! and a `pub(super)` item in a sibling is not reachable from another.

use super::*;

/// The reason a read-only profile's task must not be published as a success:
/// its worktree ended up on a named branch, which the orchestrator never
/// handed it. Free-standing so the rule is unit-testable without an engine;
/// `finalize_success` does the workflow lookup and reads the live `HEAD`.
pub(super) fn read_only_side_effect(
    workflow: &str,
    profile: Option<Profile>,
    branch: Option<&str>,
    task_id: i64,
    worktree_path: &str,
) -> Option<String> {
    let branch = branch?;
    let profile = profile.filter(|p| p.is_read_only())?;
    Some(format!(
        concat!(
            "workflow `{}` is `profile = \"{}\"`, a read-only profile, but its worktree ended ",
            "up on the branch `{}`. A first dispatch hands the worktree over detached, and a ",
            "handoff (#565) into a read-only profile detaches the inherited one and forgets its ",
            "branch (#568), so this is normally the agent having run git. Before reading it that ",
            "way, check the log for a failed detach — a branch inherited from a previous stage ",
            "looks identical here. Nothing ",
            "here prevented that; this check only refuses to publish it as a success. The ",
            "worktree and its commits are kept for inspection. Check whether it also pushed or ",
            "opened a pull request: neither can be undone from here. `totsuka task retry {}` ",
            "hits this same check again while the worktree is still on the branch — detach it ",
            "first (`git -C {} switch --detach`) or `totsuka task cancel {}`."
        ),
        workflow,
        profile.as_str(),
        branch,
        task_id,
        worktree_path,
        task_id
    ))
}

/// The output-policy name, for audit `detail`.
pub(super) fn policy_str(policy: OutputPolicy) -> &'static str {
    match policy {
        OutputPolicy::Source => "source",
        OutputPolicy::None => "none",
    }
}

/// The stable mode string persisted in `tasks.mode`.
pub(super) fn mode_str(mode: WorkflowMode) -> &'static str {
    match mode {
        WorkflowMode::Plan => "plan",
        WorkflowMode::Implement => "implement",
    }
}

/// The warning a plan-mode task earns by having branched, if it has (#378).
///
/// **`mode = "plan"` does not prevent git.** Spec F-82 asks for a mode that
/// creates a worktree to read from but performs no push or PR, and the
/// implementation has been written as though the agent CLI enforced that
/// (`--permission-mode plan`, `--sandbox read-only`, `bash: deny`). It does
/// not: a live plan-mode task branched, committed, pushed and opened a pull
/// request, because the repository's own conventions told it to.
///
/// Detection, not prevention. Making the guarantee true needs something
/// totsuka does not have today — the agent's own permission model is not it —
/// and until that exists, the failure that costs the most is the silent one:
/// an operator picks `plan` **because** they want no side effects (Slack reply
/// drafting is the case `totsuka setup` generates) and gets a pull request
/// without being told.
///
/// **This stayed a warning while the profiles grew a verdict.** A workflow with
/// a read-only `profile` is failed for the same observation — by
/// [`read_only_side_effect`] at publishing time and by
/// [`enforce_read_only`](Engine::enforce_read_only) while it runs — so for
/// those this line is the log entry beside a failure, not the whole response.
/// A bare `mode = "plan"` workflow with no profile is the case that is still
/// only warned about, deliberately: it never promised anything about branches,
/// and failing it would make an existing config start losing tasks on upgrade
/// (#409).
///
/// A branch is the signal because it is the one this side can see: the
/// orchestrator hands the worktree over detached and reads `HEAD` back, so a
/// named branch means the agent ran git. A commit made *on* the detached head
/// is not caught, but that shape cannot be pushed without first naming a ref,
/// which is what the operator actually cares about.
///
/// The message says the worktree **is on** a branch, not that the agent
/// created one. `HEAD` cannot tell the two apart — `git switch -c feat/x` and
/// `git switch main` both land here — and during incident response a wrong
/// claim about what happened costs more than a vague one.
pub(super) fn plan_mode_side_effect(mode: &str, branch: &str) -> Option<String> {
    (mode == "plan").then(|| {
        format!(
            concat!(
                "a plan-mode task's worktree is on the branch `{}` — normally the agent ",
                "ran git, since a first dispatch hands the worktree over detached. A ",
                "conversation handed to another workflow (#565) keeps its worktree, so this ",
                "can also be a branch inherited from the previous stage: that is detached on ",
                "the way in only when the receiving profile is read-only (#568), and this ",
                "warning does not know the profile. Plan is documented as making no branch, ",
                "commit or push (F-82), and nothing stopped it, so the agent followed ",
                "the repository's own conventions instead. Check whether it also pushed or ",
                "opened a pull request. A workflow with a read-only `profile` is failed for ",
                "this; a bare `mode = \"plan\"` one is only warned about."
            ),
            branch
        )
    })
}

/// Parse a persisted mode string into the dispatch execution mode (F-31).
pub(super) fn execution_mode(mode: &str) -> ExecutionMode {
    if mode == "plan" {
        ExecutionMode::Plan
    } else {
        ExecutionMode::Implement
    }
}

/// Put `[[workflows]].initial_prompt` in front of whatever else the pane was
/// going to be shown (#415).
///
/// `initial` is already `None` on a resume dispatch and when the key is unset,
/// so with no `initial_prompt` configured `base` comes back **byte-identical**
/// — which is what keeps every existing dispatch, and the "hook dispatches
/// send a null `extra_context`" test, unchanged.
///
/// The placement is not a choice core gets to make: herdr's `compose_prompt`
/// types `{extra_context}\n\n---\n{task_body}` into the pane, so anything put
/// here lands ahead of the task. That is the right order for a preamble, and
/// it is why the key is named for a *prompt* prepended to the body rather than
/// something that replaces it.
///
/// A non-string `base` is left alone rather than coerced: the only producers
/// today are string contexts, and silently JSON-stringifying a structured
/// value into a pane would be worse than not prepending.
pub(super) fn prepend_initial_prompt(base: Option<Value>, initial: Option<&str>) -> Option<Value> {
    let Some(initial) = initial else {
        return base;
    };
    match base {
        Some(Value::String(rest)) => Some(Value::String(format!("{initial}\n\n{rest}"))),
        None => Some(Value::String(initial.to_string())),
        other => other,
    }
}

/// The prompt for one dispatch: the messages nobody has sent the agent yet,
/// oldest first (#242).
///
/// `None` when there is nothing to say — an empty ledger (a task from before
/// #242, or a row v6 backfilled), or messages that carried no text — so the
/// caller keeps the body it already had rather than replacing it with "".
///
/// Several messages are joined by a blank line and nothing else. They are
/// consecutive messages from one person in one conversation, which is exactly
/// what a blank line separates in the source they came from; numbering or
/// labelling them would put words in the human's mouth.
pub(super) fn conversation_prompt(pending: &[TaskMessage]) -> Option<String> {
    let bodies: Vec<&str> = pending
        .iter()
        .map(|m| m.body.trim())
        .filter(|b| !b.is_empty())
        .collect();
    (!bodies.is_empty()).then(|| bodies.join("\n\n"))
}

/// Reconstruct the normalized [`Task`] from a stored record: the full ingest
/// payload when present, else a minimal task from the columns.
pub(super) fn task_from_record(record: &TaskRecord) -> Task {
    record
        .source_payload
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| Task {
            id: record.source_task_id.clone(),
            source: record.source.clone(),
            title: record.title.clone(),
            body: None,
            repo_hint: None,
            labels: Vec::new(),
            priority: record.priority,
            status: None,
            url: record.url.clone(),
            assignee: None,
            message_key: None,
            instructions: None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #415: the preamble goes in front, and the no-preamble case has to come
    /// back untouched — that is what keeps every existing dispatch identical.
    #[test]
    fn an_initial_prompt_goes_in_front_of_whatever_was_there() {
        let s = |v: &str| Some(Value::String(v.to_string()));

        // Claude: invisible injection, so the visible channel was empty.
        assert_eq!(
            prepend_initial_prompt(None, Some("/grill-me")),
            s("/grill-me")
        );
        // OpenCode / non-hook agents: the preamble leads, the existing
        // instructions and marker convention follow.
        assert_eq!(
            prepend_initial_prompt(s("marker rules"), Some("/grill-me")),
            s("/grill-me\n\nmarker rules")
        );

        // Unset, or a resume dispatch (the caller passes `None` for both):
        // byte-identical to what the dispatch would have sent before.
        assert_eq!(prepend_initial_prompt(None, None), None);
        assert_eq!(
            prepend_initial_prompt(s("marker rules"), None),
            s("marker rules")
        );

        // Literal: no template rendering, so braces survive. A prompt
        // containing a JSON example must not be mangled.
        assert_eq!(
            prepend_initial_prompt(None, Some(r#"reply with {"ok": true}"#)),
            s(r#"reply with {"ok": true}"#)
        );

        // A structured context (none exists today) is left alone rather than
        // JSON-stringified into the pane.
        let structured = Some(Value::Array(vec![Value::from(1)]));
        assert_eq!(
            prepend_initial_prompt(structured.clone(), Some("x")),
            structured
        );
    }

    #[test]
    fn task_round_trips_through_record_payload() {
        let task = Task {
            id: "42".into(),
            source: "github".into(),
            title: "t".into(),
            body: Some("body".into()),
            repo_hint: Some("web".into()),
            labels: vec!["bug".into()],
            priority: 5,
            status: Some("実装待ち".into()),
            url: Some("https://example.com".into()),
            assignee: Some("me".into()),
            message_key: None,
            instructions: None,
        };
        let db = StateDb::open_in_memory().unwrap();
        let id = db
            .upsert_task(&NewTask {
                source: "github".into(),
                source_task_id: task.id.clone(),
                workflow: "implement".into(),
                mode: "implement".into(),
                repo: None,
                priority: task.priority,
                title: task.title.clone(),
                url: task.url.clone(),
                source_payload: serde_json::to_value(&task).ok(),
                last_signal_at: None,
            })
            .unwrap();
        let record = db.get_task(id).unwrap().unwrap();
        assert_eq!(task_from_record(&record), task);
    }

    // -----------------------------------------------------------------
    // Conversation ingest (#242/#258)
    // -----------------------------------------------------------------

    /// The prompt is the unsent messages, oldest first — and an empty ledger
    /// leaves the caller's existing body alone rather than blanking it.
    #[test]
    fn conversation_prompt_joins_unsent_messages_and_leaves_nothing_to_say_alone() {
        fn msg(id: i64, body: &str) -> TaskMessage {
            TaskMessage {
                id,
                task_id: 1,
                message_key: format!("m{id}"),
                author: None,
                body: body.to_string(),
                url: None,
                payload: "{}".to_string(),
                received_at: "2026-01-01T00:00:00Z".to_string(),
                processed_at: None,
            }
        }

        assert_eq!(conversation_prompt(&[]), None, "nothing to say");
        assert_eq!(
            conversation_prompt(&[msg(1, "X を調べて")]),
            Some("X を調べて".to_string()),
            "a single message is its own body, unchanged"
        );
        assert_eq!(
            conversation_prompt(&[msg(1, "X を調べて"), msg(2, "あと Y も"), msg(3, "急ぎで")]),
            Some("X を調べて\n\nあと Y も\n\n急ぎで".to_string()),
            "a burst arrives as one prompt, in order"
        );
        // Blank bodies (a bare mention, an attachment-only message) contribute
        // nothing rather than blank lines...
        assert_eq!(
            conversation_prompt(&[msg(1, "  "), msg(2, "本題")]),
            Some("本題".to_string())
        );
        // ...and a batch of only those is "nothing to say", so the record's
        // own body survives.
        assert_eq!(conversation_prompt(&[msg(1, ""), msg(2, "   ")]), None);
    }

    #[test]
    fn mode_strings_round_trip() {
        assert_eq!(mode_str(WorkflowMode::Plan), "plan");
        assert_eq!(execution_mode("plan"), ExecutionMode::Plan);
        assert_eq!(execution_mode("implement"), ExecutionMode::Implement);
        // Unknown persisted values fall back to implement (never plan: plan is
        // the restrictive read-oriented mode only when explicitly chosen).
        assert_eq!(execution_mode("bogus"), ExecutionMode::Implement);
    }

    /// `tasks.mode` is what routes worktree cleanup (`cleanup_plan` vs
    /// `cleanup_implement`) and what `plan_mode_side_effect` reads, and it is
    /// written from the resolved workflow mode. A profile that resolved to the
    /// wrong string would leave `answer` worktrees behind under the implement
    /// retention — invisible until the disk fills.
    #[test]
    fn profiles_persist_the_mode_string_that_routes_cleanup() {
        let cfg = crate::config::RootConfig::from_toml_str(
            r#"
[[workflows]]
name = "answer"
source = "slack"
trigger = { label = "a" }
profile = "answer"
agent = "herdr"

[[workflows]]
name = "triage"
source = "slack"
trigger = { label = "t" }
profile = "triage"
agent = "herdr"

[[workflows]]
name = "design"
source = "slack"
trigger = { label = "d" }
profile = "design"
agent = "herdr"

[[workflows]]
name = "implement"
source = "slack"
trigger = { label = "i" }
profile = "implement"
agent = "herdr"
"#,
        )
        .unwrap();
        let workflows = crate::domain::Workflow::from_configs(&cfg.workflows);
        for (wf, expected) in workflows.iter().zip(["plan", "plan", "plan", "implement"]) {
            assert_eq!(mode_str(wf.mode), expected, "{}", wf.name);
        }
    }

    /// `mode = "plan"` is documented as making no branch, commit or push
    /// (F-82) but nothing enforces it — a live plan-mode task branched,
    /// committed, pushed and opened a PR because the repository's conventions
    /// told it to (#378). Detection is what keeps that from being silent.
    #[test]
    fn a_plan_task_that_branched_is_reported() {
        let warning =
            plan_mode_side_effect("plan", "feat/count-by-hour").expect("a plan-mode branch warns");
        // Not "created": `HEAD` cannot tell a new branch from an existing one
        // being checked out, and overclaiming misleads incident response.
        assert!(!warning.contains("created"), "{warning}");
        // The branch name has to be in it: "a plan task branched" without
        // saying which one leaves the operator nothing to look at.
        assert!(warning.contains("feat/count-by-hour"), "{warning}");
        // And it must point past the branch itself — the push and the PR are
        // what the operator actually cares about.
        assert!(warning.contains("pull request"), "{warning}");
    }

    /// Branching is the *expected* outcome in implement mode (F-86,
    /// ADR-0026), so warning there would train operators to ignore this.
    #[test]
    fn an_implement_task_that_branched_is_not_reported() {
        assert!(plan_mode_side_effect("implement", "feat/add-slugify").is_none());
    }
}
