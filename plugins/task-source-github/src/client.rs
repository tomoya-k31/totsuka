//! GitHub task-source operations over a [`GithubTransport`]: fetch + normalize
//! (F-01/F-02), status write-back (F-84), the exclusion claim (#556, see
//! [`crate::claim`] for the adjudication rule), and token validation (F-59).
//! There is no publish path — the agent writes the deliverable itself (#398). All GraphQL is built as plain JSON bodies so no GraphQL
//! client dependency is needed (mirrors the LLM adapter in orchestrator-core).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use plugin_protocol::Task;
use plugin_protocol::methods::{TaskClaimOutcome, TaskClaimResult};
use plugin_sdk::AssigneeFilter;

use crate::claim::{
    ADD_ASSIGNEES_MUTATION, AdjudicationError, CLAIM_READ_QUERY, ClaimState,
    REMOVE_ASSIGNEES_MUTATION, USER_ID_QUERY, adjudicate, parse_claim_state,
};

use crate::config::{GithubConfig, ProjectConfig};
use crate::error::GithubError;
use crate::transport::GithubTransport;

/// Max project-item pages walked per fetch / status lookup. Bounds a poll on a
/// very large board (50–100 items per page); reaching it is logged, not silent.
const MAX_FETCH_PAGES: usize = 40;

/// A parsed trigger condition (workflow-defined shape, F-81), e.g.
/// `{"status": "実装待ち"}` and/or `{"label": "bug"}`.
#[derive(Debug, Default)]
struct TriggerFilter {
    status: Option<String>,
    label: Option<String>,
    /// Which instruction set this workflow's profile asks for (#398).
    ///
    /// Derived by the Orchestrator rather than here: `[[workflows]].profile`
    /// is core's schema, and this plugin stays unaware of it. It arrives as
    /// `WorkflowInfo.instructions_kind` (a dedicated field since 0.6.0 /
    /// #554, no longer a key inside the trigger table) and is threaded in via
    /// [`GithubClient::fetch`]. Absent means the task carries no
    /// instructions — exactly the pre-#398 behaviour.
    instructions_kind: Option<String>,
    /// Who may hold the task for this workflow to take it (#572).
    ///
    /// Absent from the trigger means the pre-#572 gate (`["@me", "@none"]`),
    /// so this is the *only* assignee gate — there is no plugin-wide one left
    /// behind it that could overrule what the operator wrote.
    assignee: AssigneeFilter,
}

/// The `[[workflows]].trigger` keys this source reads (#574).
///
/// Kept beside `TriggerFilter::parse` because that is what makes them true.
/// `initialize` rejects every other key, so a typo cannot silently widen a
/// trigger — add a key here in the same edit that teaches the parser to read
/// it.
pub const TRIGGER_KEYS: &[&str] = &["assignee", "label", "status"];

impl TriggerFilter {
    /// `Err` only for a malformed `assignee`; everything else is parsed
    /// leniently because `initialize` has already rejected unknown keys (#574).
    fn parse(
        trigger: &Value,
        instructions_kind: Option<&str>,
        workflow: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            status: trigger
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
            label: trigger
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_string),
            instructions_kind: instructions_kind.map(str::to_string),
            assignee: AssigneeFilter::parse(trigger, workflow)?,
        })
    }

    /// Whether a candidate task matches the trigger the workflow asked for.
    fn matches(&self, status: Option<&str>, labels: &[String]) -> bool {
        let status_ok = match &self.status {
            Some(want) => status == Some(want.as_str()),
            None => true,
        };
        let label_ok = match &self.label {
            Some(want) => labels.iter().any(|l| l == want),
            None => true,
        };
        status_ok && label_ok
    }
}

/// GitHub task-source client, generic over its transport for testability.
pub struct GithubClient<T> {
    config: GithubConfig,
    transport: T,
    /// Which `[[projects]]` entry an ingested task came from, keyed by issue
    /// node id (#542).
    ///
    /// `task/update_status` is given only `{task_id, status}`, so with more
    /// than one board there is nothing in the request saying which board holds
    /// the item. Remembering it at ingest turns the common case into one
    /// board's worth of API calls instead of every board's.
    ///
    /// **A miss is normal, not an error**: the map is process-local, so it is
    /// empty after a restart while the Orchestrator's tasks outlive it.
    /// [`update_status`](Self::update_status) falls back to scanning every
    /// board, which is what the memo is an optimisation over — never a
    /// precondition.
    item_project: Mutex<HashMap<String, usize>>,
    /// The operator's user node id, resolved once per process for the claim
    /// mutations (#556). A failed resolution is retried on the next claim.
    my_user_id: OnceLock<String>,
}

impl<T: GithubTransport> GithubClient<T> {
    /// A client using `config` and `transport`.
    pub fn new(config: GithubConfig, transport: T) -> Self {
        Self {
            config,
            transport,
            item_project: Mutex::new(HashMap::new()),
            my_user_id: OnceLock::new(),
        }
    }

    /// The plugin settings.
    pub fn config(&self) -> &GithubConfig {
        &self.config
    }

    /// Fetch issues matching `trigger` from **every** configured board (#542),
    /// normalize to [`Task`], and apply ingest gating (F-08): skip other
    /// people's tasks, in-progress statuses, and repositories the board does
    /// not track.
    ///
    /// One board failing fails the whole poll. The alternative — skip it and
    /// return the rest — would make a broken token or a deleted board look
    /// like "no tasks right now", which is indistinguishable from a quiet
    /// board and never surfaces.
    pub async fn fetch(
        &self,
        trigger: &Value,
        instructions_kind: Option<&str>,
        workflow: &str,
    ) -> Result<Vec<Task>, GithubError> {
        let filter = TriggerFilter::parse(trigger, instructions_kind, workflow)
            .map_err(GithubError::InvalidTrigger)?;
        let mut tasks = Vec::new();
        for (index, project) in self.config.projects.iter().enumerate() {
            self.fetch_project(index, project, &filter, &mut tasks)
                .await?;
        }
        Ok(tasks)
    }

    /// Page one board, appending its ingestable tasks to `tasks` and recording
    /// which board each came from (see [`Self::item_project`]).
    async fn fetch_project(
        &self,
        index: usize,
        project_config: &ProjectConfig,
        filter: &TriggerFilter,
        tasks: &mut Vec<Task>,
    ) -> Result<(), GithubError> {
        let query = fetch_query(project_config.owner_type.graphql_root());
        let mut cursor: Option<String> = None;

        // Filtering is client-side (ProjectsV2 has no server-side status filter),
        // so cap the walk to keep a poll bounded on very large boards; a hit is
        // logged rather than silently truncating.
        for page_num in 0..MAX_FETCH_PAGES {
            let body = json!({
                "query": query,
                "variables": {
                    "owner": project_config.owner,
                    "number": project_config.project_number,
                    "statusField": self.config.status_field,
                    "cursor": cursor,
                },
            });
            let resp = self.transport.post_graphql(body, true).await?;
            let project = self.project_node(&resp, project_config)?;
            let items = &project["items"];

            for node in items["nodes"].as_array().into_iter().flatten() {
                if let Some(task) = self.normalize_item(node, project_config, filter) {
                    self.remember_project(&task.id, index);
                    tasks.push(task);
                }
            }

            let page = &items["pageInfo"];
            if page["hasNextPage"].as_bool() != Some(true) {
                return Ok(());
            }
            cursor = page["endCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(()); // defensive: hasNextPage without a cursor
            }
            if page_num + 1 == MAX_FETCH_PAGES {
                tracing::warn!(
                    pages = MAX_FETCH_PAGES,
                    project = project_config.project_number,
                    "reached the fetch page cap; some project items were not scanned this poll"
                );
            }
        }
        Ok(())
    }

    /// Note that `task_id` lives on board `index`, so a later
    /// `task/update_status` can go straight there.
    fn remember_project(&self, task_id: &str, index: usize) {
        // A poisoned mutex means a previous holder panicked while holding it.
        // The map is a cache, so the right answer is to carry on without it
        // rather than to propagate the panic into a poll.
        if let Ok(mut memo) = self.item_project.lock() {
            memo.insert(task_id.to_string(), index);
        }
    }

    /// Normalize one project item to a [`Task`], or `None` if it is not an
    /// ingestable Issue (non-Issue content, or filtered out by trigger/gating).
    fn normalize_item(
        &self,
        node: &Value,
        project_config: &ProjectConfig,
        filter: &TriggerFilter,
    ) -> Option<Task> {
        let content = &node["content"];
        if content["__typename"].as_str() != Some("Issue") {
            return None; // draft items and PRs are not tasks
        }
        let status = node["status"]["name"].as_str();
        let status_updated_at = node["status"]["updatedAt"].as_str();
        let repo = content["repository"]["name"].as_str().unwrap_or_default();
        let issue_number = content["number"].as_i64().unwrap_or_default().to_string();
        let labels: Vec<String> = content["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_string))
            .collect();
        // Consider *all* assignees, not just the first: whether I am assigned
        // must not depend on GitHub's node ordering (F-08).
        let assignees: Vec<&str> = content["assignees"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["login"].as_str())
            .collect();

        // Workflow trigger first, then the gating that is not the workflow's
        // to state: which repositories this board tracks, and which columns
        // mean "already running" (F-08).
        //
        // The assignee gate lives in the trigger now (#572), defaulting to the
        // old plugin-wide rule when the workflow says nothing. It is checked
        // here rather than in `matches` because it needs the operator's login.
        if !filter.matches(status, &labels)
            || !filter
                .assignee
                .matches(&assignees, Some(&self.config.github_login))
        {
            return None;
        }
        if !project_config.repo_allowed(repo)
            || status.is_some_and(|s| self.config.is_in_progress(s))
        {
            return None;
        }

        let id = content["id"].as_str()?.to_string();
        let body = content["body"].as_str().filter(|b| !b.is_empty());
        // Surface my own login when I'm an assignee, else the first assignee.
        let assignee = assignees
            .iter()
            .find(|l| l.eq_ignore_ascii_case(&self.config.github_login))
            .or(assignees.first())
            .map(|l| l.to_string());
        Some(Task {
            id,
            source: self.config.source_name.clone(),
            title: content["title"].as_str().unwrap_or_default().to_string(),
            body: body.map(str::to_string),
            repo_hint: (!repo.is_empty()).then(|| repo.to_string()),
            labels,
            priority: 0,
            status: status.map(str::to_string),
            url: content["url"].as_str().map(str::to_string),
            assignee,
            // Lane-entry identity (#556, ADR-0059 §5): the status cell's
            // server-issued `updatedAt` changes exactly when the card moves
            // columns, so keying the delivery on it turns "a human moved the
            // card back into the trigger column" into a **new** message —
            // which is what reopens a finished conversation (#242) — while
            // every re-delivery of the same entry (each poll tick, and a
            // stale pre-completion fetch snapshot arriving late) carries the
            // same key and dedups. Server-timestamp equality only: no local
            // clock is ever compared.
            //
            // Only when the workflow triggers on a column: a label-only
            // trigger has no lane, so *any* column move would re-run it —
            // those keep the at-most-once `None` (ingest falls back to the
            // conversation id).
            message_key: match (&filter.status, status, status_updated_at) {
                (Some(_), Some(name), Some(at)) => Some(format!("status:{name}@{at}")),
                _ => None,
            },
            // Layer 1 of ADR-0024: where this task's deliverable goes. Absent
            // unless the Orchestrator asked for a kind (#398), which keeps
            // every pre-profile config behaving exactly as before.
            instructions: filter
                .instructions_kind
                .as_deref()
                .and_then(|kind| self.config.prompts.for_kind(kind))
                .map(|template| {
                    crate::template::render(
                        template,
                        &[("issue_number", issue_number.as_str()), ("repo", repo)],
                    )
                }),
        })
    }

    /// Move a task's project status column to `status` (F-84). `status` is the
    /// orchestrator-side name; it is mapped to a project option via config, and
    /// an unknown option is a hard error rather than a silent no-op.
    ///
    /// With several boards configured (#542) the request does not say which one
    /// holds the item — `TaskUpdateStatusParams` is `{task_id, status}` — so
    /// the board is recovered from the ingest-time memo, and failing that by
    /// trying each board in config order.
    pub async fn update_status(&self, task_id: &str, status: &str) -> Result<(), GithubError> {
        for index in self.project_search_order(task_id) {
            let project_config = &self.config.projects[index];
            if self
                .update_status_in(index, project_config, task_id, status)
                .await?
            {
                return Ok(());
            }
        }
        Err(GithubError::NotFound(format!(
            "issue `{task_id}` is not an item of any board this plugin polls \
             ({}) → check that the issue is still on one of those boards",
            self.config
                .projects
                .iter()
                .map(|p| format!("#{}", p.project_number))
                .collect::<Vec<_>>()
                .join(", "),
        )))
    }

    /// Which boards to try for `task_id`, remembered board first.
    ///
    /// Always yields **every** board, not just the remembered one: the memo
    /// says where the item was at ingest, and an item can be moved between
    /// boards afterwards. Ordering it first makes the common case one board's
    /// worth of calls; keeping the rest makes a stale memo slow, not wrong.
    fn project_search_order(&self, task_id: &str) -> Vec<usize> {
        let remembered = self
            .item_project
            .lock()
            .ok()
            .and_then(|memo| memo.get(task_id).copied())
            .filter(|index| *index < self.config.projects.len());
        let mut order: Vec<usize> = remembered.into_iter().collect();
        order.extend((0..self.config.projects.len()).filter(|i| Some(*i) != remembered));
        order
    }

    /// Try to move `task_id` on one board. `Ok(false)` means the item is not on
    /// this board (try the next); an API or config failure is `Err`.
    async fn update_status_in(
        &self,
        index: usize,
        project_config: &ProjectConfig,
        task_id: &str,
        status: &str,
    ) -> Result<bool, GithubError> {
        let target = status.to_string();
        let query = resolve_query(project_config.owner_type.graphql_root());

        // Resolve the project id + status option once, then page the item list
        // until the issue's item is found (a busy board can hold >1 page).
        //
        // **A missing status option is not an error yet.** With several boards
        // the search visits boards the item is not on, and those boards need
        // not have the target column at all — erroring here would abort the
        // whole search (the caller propagates with `?`) and fail a transition
        // that would have succeeded on the next board. The error is raised
        // below, once the item *is* found here, where it means what it says.
        let mut project_id = String::new();
        let mut field_id = String::new();
        let mut option_id: Option<String> = None;
        let mut item_id: Option<String> = None;
        let mut cursor: Option<String> = None;

        for page_num in 0..MAX_FETCH_PAGES {
            let resolve = json!({
                "query": query,
                "variables": {
                    "owner": project_config.owner,
                    "number": project_config.project_number,
                    "statusField": self.config.status_field,
                    "cursor": cursor,
                },
            });
            let resp = self.transport.post_graphql(resolve, true).await?;
            let project = self.project_node(&resp, project_config)?;

            if page_num == 0 {
                project_id = project["id"]
                    .as_str()
                    .ok_or_else(|| GithubError::InvalidResponse("project id missing".into()))?
                    .to_string();
                let field = &project["field"];
                field_id = field["id"]
                    .as_str()
                    .ok_or_else(|| GithubError::InvalidResponse("status field missing".into()))?
                    .to_string();
                option_id = field["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|o| o["name"].as_str() == Some(target.as_str()))
                    .and_then(|o| o["id"].as_str())
                    .map(str::to_string);
            }

            let items = &project["items"];
            if let Some(found) = items["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|n| n["content"]["id"].as_str() == Some(task_id))
                .and_then(|n| n["id"].as_str())
            {
                item_id = Some(found.to_string());
                break;
            }
            let page = &items["pageInfo"];
            if page["hasNextPage"].as_bool() != Some(true) {
                break;
            }
            cursor = page["endCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }

        let Some(item_id) = item_id else {
            return Ok(false); // not on this board — the caller tries the next
        };
        // The item is here, so a missing option is this board's problem and
        // the operator's to fix — searching on would hide it.
        let option_id = option_id.ok_or_else(|| {
            GithubError::NotFound(format!(
                "unknown status `{target}` for field `{}` on project #{}, which is where issue `{task_id}` lives → add the option in that project, or write the column's exact option name",
                self.config.status_field, project_config.project_number
            ))
        })?;

        let mutation = json!({
            "query": UPDATE_STATUS_MUTATION,
            "variables": {
                "project": project_id, "item": item_id,
                "field": field_id, "option": option_id,
            },
        });
        // Idempotent: setting the same option again yields the same state.
        let resp = self.transport.post_graphql(mutation, true).await?;
        check_errors(&resp)?;
        // Found by a scan when the memo was empty or stale — record it so the
        // next transition on this task goes straight to the right board.
        self.remember_project(task_id, index);
        Ok(true)
    }

    /// Claim `task_id` (an issue node id) for exclusive execution (#556,
    /// ADR-0059): self-assign, read back, and adjudicate races by the
    /// earliest effective `AssignedEvent` (see [`crate::claim`]).
    ///
    /// Contract with the Orchestrator: the three [`TaskClaimOutcome`]s are
    /// **settled answers**; anything transient — transport failures, and an
    /// adjudication that cannot be decided because a competitor's event is
    /// not visible yet — is an `Err`, which the Orchestrator answers by
    /// leaving the task queued and retrying next cycle.
    pub async fn claim(&self, task_id: &str) -> Result<TaskClaimResult, GithubError> {
        let me = self.config.github_login.as_str();

        // Pre-read. Already holding — a human pre-assigned this operator, a
        // previous run claimed it, or this is a retry — is a Won without a
        // write: adjudication is a tool for breaking the race between
        // *automated* claimers starting from unassigned, and must not
        // overrule a human's explicit routing (me + a reviewer, say).
        let state = self.read_claim_state(task_id).await?;
        if state.holds(me) {
            return Ok(won());
        }
        if let Some(other) = state.assignees.first() {
            // Someone else already holds it; the fetch that queued this task
            // was simply stale. Nothing was written, nothing to undo. The
            // holder is adjudicated when the events allow it — `first()` is
            // an unordered list's head, not the winner — but it is an
            // informational label, so an invisible event degrades to the
            // first assignee rather than to a retry.
            let holder = adjudicate(&state)
                .map(str::to_string)
                .unwrap_or_else(|_| other.clone());
            return Ok(lost(holder));
        }

        // Unassigned: write the claim, then read back after the measured
        // propagation delay.
        let my_id = self.my_user_id(me).await?;
        self.assign_mutation(ADD_ASSIGNEES_MUTATION, task_id, &my_id)
            .await?;
        tokio::time::sleep(self.config.claim_verify_delay()).await;
        let mut state = self.read_claim_state(task_id).await?;

        if !state.holds(me) {
            // The API answered 200 and the assignee is still not there. One
            // slower re-read distinguishes propagation from the silent
            // discard GitHub performs for assignees without push access —
            // only then is it the permanent, human-must-act answer.
            tokio::time::sleep(self.config.claim_verify_delay() * 2).await;
            state = self.read_claim_state(task_id).await?;
            if !state.holds(me) {
                return Ok(TaskClaimResult {
                    outcome: TaskClaimOutcome::Forbidden,
                    holder: None,
                });
            }
        }

        if state.assignees.len() == 1 {
            return Ok(won()); // sole assignee — no race to adjudicate
        }
        match adjudicate(&state) {
            Ok(winner) if winner.eq_ignore_ascii_case(me) => Ok(won()),
            Ok(winner) => {
                let winner = winner.to_string();
                // Step aside: remove only this operator's own assignment.
                // Best-effort for the *outcome* — the winner is already
                // running either way — but not for trying: a leftover
                // self-assignment makes a later `task retry` re-win through
                // the pre-read fast path (this operator is an assignee
                // again, indistinguishable from a human's routing) and
                // double-run the task. One in-call retry closes most of that
                // window; the final failure names the consequence.
                let mut removed = Ok(());
                for _ in 0..2 {
                    removed = self
                        .assign_mutation(REMOVE_ASSIGNEES_MUTATION, task_id, &my_id)
                        .await;
                    if removed.is_ok() {
                        break;
                    }
                }
                if let Err(e) = removed {
                    tracing::error!(
                        task_id,
                        "lost the claim but could not remove own assignee: {e} → remove it \
                         by hand, or a later `task retry` will re-win the claim and \
                         double-run the task"
                    );
                }
                Ok(lost(winner))
            }
            // A current assignee's event is not visible yet. Err — not a
            // forfeit: if both racers yielded on mutual invisibility the
            // task would be held by nobody and re-ingested by nobody. The
            // Orchestrator retries next cycle, when the event is there.
            Err(AdjudicationError::MissingEvent(login)) => {
                Err(GithubError::InvalidResponse(format!(
                    "assignee `{login}`'s AssignedEvent is not visible yet — the claim \
                     cannot be adjudicated on this read; it will be retried"
                )))
            }
        }
    }

    /// Read the issue's assignees + assignment history (see [`crate::claim`]).
    async fn read_claim_state(&self, task_id: &str) -> Result<ClaimState, GithubError> {
        let body = json!({ "query": CLAIM_READ_QUERY, "variables": { "id": task_id } });
        let resp = self.transport.post_graphql(body, true).await?;
        let data = check_errors(&resp)?;
        parse_claim_state(data).ok_or_else(|| {
            GithubError::NotFound(format!(
                "issue `{task_id}` cannot be read (deleted, or not an Issue node) → \
                 `totsuka task cancel` the task if the issue is gone"
            ))
        })
    }

    /// The operator's user node id, resolved once per process.
    async fn my_user_id(&self, login: &str) -> Result<String, GithubError> {
        if let Some(id) = self.my_user_id.get() {
            return Ok(id.clone());
        }
        let body = json!({ "query": USER_ID_QUERY, "variables": { "login": login } });
        let resp = self.transport.post_graphql(body, true).await?;
        let data = check_errors(&resp)?;
        let id = data["user"]["id"].as_str().ok_or_else(|| {
            GithubError::NotFound(format!(
                "user `{login}` was not found → check `github_login` in `[github]` of config.toml"
            ))
        })?;
        // A concurrent resolution racing here stores the same value; ignore.
        let _ = self.my_user_id.set(id.to_string());
        Ok(id.to_string())
    }

    /// Run one of the two assignee mutations for this operator only.
    async fn assign_mutation(
        &self,
        mutation: &str,
        task_id: &str,
        my_id: &str,
    ) -> Result<(), GithubError> {
        let body = json!({
            "query": mutation,
            "variables": { "a": task_id, "u": [my_id] },
        });
        // `idempotent: true`: adding an assignee that is already there and
        // removing one that is not are both no-ops on GitHub's side, so a
        // replay after a timed-out attempt cannot double anything — and
        // without it a transient 5xx/429 fails the whole claim for a cycle.
        let resp = self.transport.post_graphql(body, true).await?;
        check_errors(&resp)?;
        Ok(())
    }

    /// Confirm the token works by reading `viewer.login` (F-59). Static config
    /// problems are reported separately by [`static_config_errors`].
    pub async fn validate(&self) -> Result<(), GithubError> {
        let resp = self
            .transport
            .post_graphql(json!({ "query": VIEWER_QUERY }), true)
            .await?;
        let data = check_errors(&resp)?;
        if data["viewer"]["login"].as_str().is_some() {
            Ok(())
        } else {
            Err(GithubError::InvalidResponse(
                "viewer query returned no login".into(),
            ))
        }
    }

    /// Extract `data.<owner-root>.projectV2`, surfacing GraphQL errors and a
    /// missing project (e.g. wrong owner/number) as actionable failures.
    fn project_node<'a>(
        &self,
        resp: &'a Value,
        project_config: &ProjectConfig,
    ) -> Result<&'a Value, GithubError> {
        let data = check_errors(resp)?;
        let root = project_config.owner_type.graphql_root();
        let project = &data[root]["projectV2"];
        if project.is_null() {
            return Err(GithubError::NotFound(format!(
                "project #{} not found for {} `{}` → check owner/owner_type/project_number in the `[[projects]]` entry `{}` of config.toml",
                project_config.project_number, root, project_config.owner, project_config.name
            )));
        }
        Ok(project)
    }
}

/// A settled `won` answer.
fn won() -> TaskClaimResult {
    TaskClaimResult {
        outcome: TaskClaimOutcome::Won,
        holder: None,
    }
}

/// A settled `lost` answer naming the holder.
fn lost(holder: String) -> TaskClaimResult {
    TaskClaimResult {
        outcome: TaskClaimOutcome::Lost,
        holder: Some(holder),
    }
}

/// Return the `data` object, or a [`GithubError::GraphQl`] if the response
/// carried a non-empty top-level `errors` array.
fn check_errors(resp: &Value) -> Result<&Value, GithubError> {
    if let Some(errors) = resp.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let joined = errors
            .iter()
            .filter_map(|e| e["message"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(GithubError::GraphQl(joined));
    }
    Ok(&resp["data"])
}

/// Static (offline) config problems for `config/validate` (F-63): things a
/// viewer ping cannot catch. Required fields are already enforced by serde.
pub fn static_config_errors(config: &GithubConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if config.token.is_empty() {
        errors.push("`token` is empty → set it (or its ${ENV}/keychain: reference)".into());
    }
    if config.github_login.is_empty() {
        errors.push(
            "`github_login` is empty → set your GitHub login for ingest gating (F-08)".into(),
        );
    }
    if config.projects.is_empty() {
        errors.push(
            "no `[[projects]]` entry has `source = \"github\"` → declare at least one board (name / owner / project_number)"
                .into(),
        );
    }
    for project in &config.projects {
        if project.owner.is_empty() {
            errors.push(format!(
                "`owner` is empty in the `[[projects]]` entry `{}` → set the project owner login",
                project.name
            ));
        }
        if project.project_number <= 0 {
            errors.push(format!(
                "`project_number` must be positive, got {} → use the ProjectsV2 number",
                project.project_number
            ));
        }
        if project.repos.is_empty() {
            errors.push(format!(
                "no repository is bound to the `[[projects]]` entry `{}` → set `project = \"{}\"` on the `[[repositories]]` entries this board tracks, or drop the board",
                project.name, project.name
            ));
        }
    }
    errors
}

/// The paginated project-items query (F-02). `root` is `user` or `organization`
/// — a fixed, non-user-controlled keyword, so string interpolation is safe.
fn fetch_query(root: &str) -> String {
    format!(
        r#"query($owner: String!, $number: Int!, $statusField: String!, $cursor: String) {{
  {root}(login: $owner) {{
    projectV2(number: $number) {{
      items(first: 50, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          status: fieldValueByName(name: $statusField) {{
            ... on ProjectV2ItemFieldSingleSelectValue {{ name updatedAt }}
          }}
          content {{
            __typename
            ... on Issue {{
              id number title body url
              repository {{ name }}
              assignees(first: 10) {{ nodes {{ login }} }}
              labels(first: 100) {{ nodes {{ name }} }}
            }}
          }}
        }}
      }}
    }}
  }}
}}"#
    )
}

/// Resolves the project id, status field + options, and (paginated) item ids
/// for `update_status`. `root` is `user`/`organization` (see [`fetch_query`]).
fn resolve_query(root: &str) -> String {
    format!(
        r#"query($owner: String!, $number: Int!, $statusField: String!, $cursor: String) {{
  {root}(login: $owner) {{
    projectV2(number: $number) {{
      id
      field(name: $statusField) {{
        ... on ProjectV2SingleSelectField {{ id options {{ id name }} }}
      }}
      items(first: 100, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{ id content {{ ... on Issue {{ id }} }} }}
      }}
    }}
  }}
}}"#
    )
}

const UPDATE_STATUS_MUTATION: &str = r#"mutation($project: ID!, $item: ID!, $field: ID!, $option: String!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $project, itemId: $item, fieldId: $field,
    value: { singleSelectOptionId: $option }
  }) { projectV2Item { id } }
}"#;

const VIEWER_QUERY: &str = "query { viewer { login } }";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_filter_matches_status_and_label() {
        let f = TriggerFilter::parse(&json!({ "status": "実装待ち", "label": "bug" }), None, "wf")
            .unwrap();
        assert!(f.matches(Some("実装待ち"), &["bug".into()]));
        assert!(!f.matches(Some("実装中"), &["bug".into()])); // wrong status
        assert!(!f.matches(Some("実装待ち"), &["docs".into()])); // missing label
    }

    #[test]
    fn empty_trigger_matches_everything() {
        let f = TriggerFilter::parse(&json!({}), None, "wf").unwrap();
        assert!(f.matches(Some("anything"), &[]));
        assert!(f.matches(None, &["x".into()]));
    }

    /// A project item as the fetch query returns it.
    fn item(status: &str) -> Value {
        json!({
            "status": { "name": status },
            "content": {
                "__typename": "Issue",
                "id": "I_1",
                "number": 42,
                "title": "カウント集計を時間帯別にする",
                "body": "現状は日次のみ",
                "url": "https://github.com/me/web-app/issues/42",
                "repository": { "name": "web-app" },
                "labels": { "nodes": [] },
                "assignees": { "nodes": [] }
            }
        })
    }

    /// `normalize_item` never touches the transport, so a stub that refuses
    /// every call is both sufficient and a guard: a future change that started
    /// making network calls from the mapper would fail loudly here.
    struct NeverCalled;

    impl crate::transport::GithubTransport for NeverCalled {
        async fn post_graphql(
            &self,
            _body: Value,
            _idempotent: bool,
        ) -> Result<Value, GithubError> {
            panic!("normalize_item must not perform a request")
        }
    }

    fn client_for_tests() -> GithubClient<NeverCalled> {
        let cfg: GithubConfig = crate::config::config_from_json(json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["web-app"] }]
        }));
        GithubClient::new(cfg, NeverCalled)
    }

    /// The board `item()` belongs to, for the `normalize_item` callers below.
    fn project_for_tests() -> ProjectConfig {
        ProjectConfig::new("board-0", "me", 1, &["web-app"])
    }

    /// The `instructions_kind` the Orchestrator derives beside the trigger
    /// (a dedicated `WorkflowInfo` field since 0.6.0, #554) picks the
    /// instruction text, and the placeholders are filled from the issue
    /// (#398).
    #[test]
    fn a_design_trigger_tells_the_agent_where_to_put_the_design() {
        let filter =
            TriggerFilter::parse(&json!({ "status": "設計待ち" }), Some("design"), "wf").unwrap();
        let task = client_for_tests()
            .normalize_item(&item("設計待ち"), &project_for_tests(), &filter)
            .expect("ingestable");
        let instructions = task.instructions.expect("design tasks carry instructions");

        assert!(
            instructions.contains("gh issue comment 42"),
            "{instructions}"
        );
        assert!(instructions.contains("web-app"), "{instructions}");
        // The URL demand is the whole reason these exist: nothing else tells
        // the Orchestrator the comment was ever posted.
        assert!(instructions.contains("URL"), "{instructions}");
        // No leftover placeholder — a `{issue_number}` shipped verbatim would
        // be read by the agent as literal text.
        assert!(!instructions.contains('{'), "{instructions}");
    }

    #[test]
    fn each_kind_selects_its_own_text() {
        let for_kind = |kind: &str| {
            let filter = TriggerFilter::parse(&json!({}), Some(kind), "wf").unwrap();
            client_for_tests()
                .normalize_item(&item("any"), &project_for_tests(), &filter)
                .unwrap()
                .instructions
                .unwrap()
        };
        assert!(for_kind("implement").contains("open a pull request"));
        assert!(for_kind("triage").contains("work out what has to be done"));
        assert!(for_kind("design").contains("produce a detailed design"));
    }

    /// **The compatibility half.** An Orchestrator that sends no
    /// `instructions_kind` — anything before #398, and any workflow written in
    /// the spelled-out notation — must produce exactly the task it did before.
    #[test]
    fn no_instructions_kind_means_no_instructions() {
        let filter = TriggerFilter::parse(&json!({ "status": "実装待ち" }), None, "wf").unwrap();
        let task = client_for_tests()
            .normalize_item(&item("実装待ち"), &project_for_tests(), &filter)
            .expect("ingestable");
        assert_eq!(task.instructions, None);
    }

    /// A kind this plugin has no text for yields nothing rather than guessing.
    /// Dispatching an agent with instructions for the wrong deliverable is
    /// worse than dispatching it with the instructions it had before.
    #[test]
    fn an_unknown_kind_falls_back_to_no_instructions() {
        let filter = TriggerFilter::parse(&json!({}), Some("audit"), "wf").unwrap();
        let task = client_for_tests()
            .normalize_item(&item("any"), &project_for_tests(), &filter)
            .expect("ingestable");
        assert_eq!(task.instructions, None);
    }

    #[test]
    fn embedded_defaults_parse() {
        // Force the LazyLock so a malformed or key-missing `defaults.toml`
        // fails here rather than at `initialize` — and prove no key is empty.
        let p = crate::config::GithubPrompts::default();
        for (name, value) in [
            ("triage_instructions", &p.triage_instructions),
            ("design_instructions", &p.design_instructions),
            ("implement_instructions", &p.implement_instructions),
        ] {
            assert!(!value.trim().is_empty(), "`{name}` is empty");
        }
    }

    #[test]
    fn static_errors_flag_bad_project_number() {
        let cfg: GithubConfig = crate::config::config_from_json(json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 0, "repos": ["r"] }]
        }));
        let errors = static_config_errors(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("project_number")),
            "got {errors:?}"
        );
    }

    /// The "a repository is on two boards" check `static_config_errors` used to
    /// carry (#542) is gone, because #554 made the state unwritable rather than
    /// merely invalid: `repos` is no longer typed by the operator, it is
    /// derived from the `[[repositories]]` entries whose single-valued
    /// `project` names this board. Pinned through `resolve`, not through the
    /// deleted check — a test asserting "no error is reported" would keep
    /// passing if the derivation itself started handing one repository to two
    /// boards, which is the failure the old check existed to prevent.
    #[test]
    fn resolve_gives_each_repository_to_exactly_one_board() {
        use plugin_protocol::methods::{ProjectInfo, RepoInfo};

        let project = |name: &str, number: i64| ProjectInfo {
            name: name.to_string(),
            options: serde_json::json!({ "owner": "me", "project_number": number })
                .as_object()
                .unwrap()
                .clone(),
        };
        let repo = |name: &str, project: &str| RepoInfo {
            name: name.to_string(),
            summary: None,
            path: None,
            project: Some(project.to_string()),
        };

        let boards = crate::config::ProjectConfig::resolve(
            &[project("first", 1), project("second", 2)],
            &[
                repo("totsuka", "first"),
                repo("shared", "first"),
                repo("web", "second"),
            ],
        )
        .expect("resolves");

        let mut homes: Vec<(&str, &str)> = Vec::new();
        for board in &boards {
            for r in &board.repos {
                homes.push((r.as_str(), board.name.as_str()));
            }
        }
        homes.sort_unstable();
        assert_eq!(
            homes,
            [("shared", "first"), ("totsuka", "first"), ("web", "second")]
        );
    }

    /// A board no repository points at polls nothing and claims nothing, so it
    /// is reported — the operator either meant to bind a repository to it or
    /// meant to delete it. The message names the entry and the key to set,
    /// because there is no longer a `repos` list to fill in on the board side.
    #[test]
    fn static_errors_flag_a_board_no_repository_is_bound_to() {
        let cfg: GithubConfig = crate::config::config_from_json(json!({
            "token": "t", "github_login": "me",
            "projects": [{ "name": "lonely", "owner": "me", "project_number": 1, "repos": [] }]
        }));
        let errors = static_config_errors(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("lonely") && e.contains("[[repositories]]")),
            "got {errors:?}"
        );
    }

    /// No boards at all: serde accepts `projects = []` (it is a list, not a
    /// missing field), so the check has to be here.
    #[test]
    fn static_errors_flag_no_boards() {
        let cfg: GithubConfig = crate::config::config_from_json(json!({
            "token": "t", "github_login": "me", "projects": []
        }));
        let errors = static_config_errors(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("`source = \"github\"`")),
            "got {errors:?}"
        );
    }
}
