//! The mention pipeline: consumes normalized Socket Mode events, applies the
//! mention filter, enriches a fresh mention with thread context and names,
//! resolves the target repository (issue #106), normalizes to the common
//! [`Task`] schema, and buffers it until the orchestrator's next
//! `tasks/fetch` drains the buffer (pull loop over a push source, #105).
//!
//! Repository resolution runs entirely in the plugin: channel-prefix rules,
//! then the plugin's own LLM classifier, then — when neither decides — an
//! in-thread ephemeral asking the operator. While a selection is pending the
//! mention is **not** submitted as a task (no dangling pending task in the
//! orchestrator); the `block_actions` answer resumes it, and unanswered
//! selections expire after [`SELECTION_TTL`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use plugin_protocol::Task;

use crate::config::SlackConfig;
use crate::draft::{DRAFT_TTL, Draft, DraftStatus, DraftStore};
use crate::llm::ChatTransport;
use crate::mention::{Mention, MentionFilter};
use crate::repo_resolver::{Resolution, resolve};
use crate::slack_api::{PostEphemeral, SlackApi};
use crate::socket_mode::SocketEvent;
use crate::transport::SlackTransport;

/// Slack coordinates a task needs again at `result/publish` time (where the
/// approved reply goes). Keyed by task id; in-memory, lost on restart —
/// acceptable, the draft text survives in the self-DM record.
#[derive(Debug, Clone)]
pub struct PendingMention {
    /// Channel the mention was posted in.
    pub channel: String,
    /// Thread the approved reply must go to (`thread_ts ?? ts`).
    pub reply_ts: String,
    /// The mention message itself.
    pub mention_ts: String,
    /// Sender display name (for the draft header).
    pub sender_name: String,
    /// Permalink to the mention (for the record), when resolvable.
    pub permalink: Option<String>,
}

/// Bound on the pending-mention index. `result/publish` (#107) consumes
/// entries, but until every task round-trips, the oldest entries fall out
/// FIFO instead of growing without bound in a long-running plugin.
const PENDING_CAP: usize = 1024;

/// How long an unanswered repository selection stays alive.
const SELECTION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often expired selections are swept.
const SELECTION_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// State shared between the pipeline task and the JSON-RPC server: the task
/// buffer `tasks/fetch` drains, the pending-mention index, the draft store
/// (#107), and the resolved self-DM record channel.
#[derive(Clone, Default)]
pub struct SharedState {
    buffer: Arc<Mutex<Vec<Task>>>,
    pending: Arc<Mutex<PendingIndex>>,
    drafts: Arc<Mutex<DraftStore>>,
    self_dm: Arc<Mutex<Option<String>>>,
}

/// The pending-mention map plus its FIFO eviction order.
#[derive(Default)]
struct PendingIndex {
    entries: HashMap<String, PendingMention>,
    order: std::collections::VecDeque<String>,
}

impl SharedState {
    /// Queue a normalized task for the next `tasks/fetch`.
    pub fn push_task(&self, task: Task) {
        self.buffer.lock().unwrap().push(task);
    }

    /// Drain the whole buffer (a fetch returns everything and forgets it; a
    /// second fetch must not see the same task).
    pub fn drain_tasks(&self) -> Vec<Task> {
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }

    /// Remember where `task_id`'s reply belongs (bounded: beyond
    /// [`PENDING_CAP`], the oldest entry is evicted with a warning).
    pub fn insert_pending(&self, task_id: String, pending: PendingMention) {
        let mut index = self.pending.lock().unwrap();
        if !index.entries.contains_key(&task_id) {
            if index.order.len() >= PENDING_CAP
                && let Some(evicted) = index.order.pop_front()
            {
                index.entries.remove(&evicted);
                tracing::warn!(
                    task_id = %evicted,
                    "pending-mention index full; evicted the oldest entry \
                     (its reply can no longer be placed)"
                );
            }
            index.order.push_back(task_id.clone());
        }
        index.entries.insert(task_id, pending);
    }

    /// The Slack coordinates for `task_id`, if it is still pending.
    pub fn pending(&self, task_id: &str) -> Option<PendingMention> {
        self.pending.lock().unwrap().entries.get(task_id).cloned()
    }

    /// Remove and return `task_id`'s coordinates — the terminal consumption
    /// at `result/publish` time, which also keeps the index from holding
    /// entries for tasks that already round-tripped.
    pub fn take_pending(&self, task_id: &str) -> Option<PendingMention> {
        let mut index = self.pending.lock().unwrap();
        let taken = index.entries.remove(task_id);
        if taken.is_some() {
            index.order.retain(|id| id != task_id);
        }
        taken
    }

    /// Record the resolved self-DM record channel (set once by the pipeline
    /// at startup; read by the approval flow).
    pub fn set_self_dm_channel(&self, channel: String) {
        *self.self_dm.lock().unwrap() = Some(channel);
    }

    /// The self-DM record channel, when startup resolution succeeded.
    pub fn self_dm_channel(&self) -> Option<String> {
        self.self_dm.lock().unwrap().clone()
    }

    /// Store a draft, returning its fresh id (#107).
    pub fn insert_draft(&self, draft: Draft) -> String {
        self.drafts.lock().unwrap().insert(draft)
    }

    /// The draft behind `draft_id`, if it is still stored.
    pub fn draft(&self, draft_id: &str) -> Option<Draft> {
        self.drafts.lock().unwrap().get(draft_id).cloned()
    }

    /// Record a draft's self-DM record `ts`.
    pub fn set_draft_dm_ts(&self, draft_id: &str, dm_ts: String) {
        self.drafts.lock().unwrap().set_dm_ts(draft_id, dm_ts);
    }

    /// Move a draft to `status`.
    pub fn set_draft_status(&self, draft_id: &str, status: DraftStatus) {
        self.drafts.lock().unwrap().set_status(draft_id, status);
    }

    /// Drop drafts past [`DRAFT_TTL`], returning the dropped ids.
    pub fn sweep_drafts(&self, now: Instant) -> Vec<String> {
        self.drafts.lock().unwrap().sweep(now, DRAFT_TTL)
    }
}

/// A mention with everything the pipeline looked up for it.
#[derive(Clone)]
struct EnrichedMention {
    mention: Mention,
    sender_name: String,
    channel_name: String,
    permalink: Option<String>,
    /// `name: text` lines, oldest first. `None` = lookup failed.
    context_lines: Option<Vec<String>>,
}

/// Bound on parked selections, mirroring [`PENDING_CAP`]'s rationale: an
/// LLM outage that degrades every mention to a picker must not grow memory
/// without bound until the 24h TTL. Evicting loses the mention (same
/// semantics as TTL expiry), logged as a warning.
const AWAITING_CAP: usize = 256;

/// Mentions waiting for the operator's repository choice, keyed by task id.
/// Kept out of [`SharedState`] (only the pipeline touches it), but shared
/// with the per-mention resolution tasks, hence used behind a lock.
#[derive(Default)]
struct AwaitingSelection {
    entries: HashMap<String, (EnrichedMention, Instant)>,
    order: std::collections::VecDeque<String>,
}

impl AwaitingSelection {
    fn insert(&mut self, enriched: EnrichedMention, now: Instant) {
        let task_id = enriched.mention.task_id();
        if !self.entries.contains_key(&task_id) {
            if self.order.len() >= AWAITING_CAP
                && let Some(evicted) = self.order.pop_front()
            {
                self.entries.remove(&evicted);
                tracing::warn!(
                    task_id = %evicted,
                    "awaiting-selection store full; evicted the oldest parked mention"
                );
            }
            self.order.push_back(task_id.clone());
        }
        self.entries.insert(task_id, (enriched, now));
    }

    fn take(&mut self, task_id: &str) -> Option<EnrichedMention> {
        let taken = self.entries.remove(task_id).map(|(e, _)| e);
        if taken.is_some() {
            self.order.retain(|id| id != task_id);
        }
        taken
    }

    /// Drop entries older than `ttl`, returning the dropped task ids.
    fn sweep(&mut self, now: Instant, ttl: Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, (_, at))| now.duration_since(*at) >= ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.entries.remove(id);
            self.order.retain(|other| other != id);
        }
        expired
    }
}

/// Run the pipeline over `events` until the channel closes: filter each
/// message event, enrich + resolve + normalize fresh mentions, hand results
/// to `state`, and answer `block_actions` (repository selections and the
/// approval flow's approve/reject presses).
pub fn spawn<T: SlackTransport + 'static, C: ChatTransport + 'static>(
    api: Arc<SlackApi<T>>,
    chat: Arc<C>,
    config: Arc<SlackConfig>,
    mut events: mpsc::UnboundedReceiver<SocketEvent>,
    state: SharedState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut filter = MentionFilter::new(&config.target_user_id);
        // Resolve the self-DM record channel up front (filter row 3). Failure
        // is not fatal: row 2 (own posts) already breaks reply loops.
        match api.conversations_open_self(&config.target_user_id).await {
            Ok(channel) => {
                filter.set_self_dm_channel(channel.clone());
                // The approval flow posts its draft records there (#107).
                state.set_self_dm_channel(channel);
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve the self-DM channel; \
                     continuing without that filter row");
            }
        }

        let mut names = NameCache::default();
        let awaiting = Arc::new(Mutex::new(AwaitingSelection::default()));
        let mut sweep = tokio::time::interval(SELECTION_SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        sweep.tick().await; // the first tick fires immediately; skip it

        loop {
            let event = tokio::select! {
                event = events.recv() => match event {
                    Some(event) => event,
                    None => return,
                },
                _ = sweep.tick() => {
                    let now = Instant::now();
                    let expired = awaiting.lock().unwrap().sweep(now, SELECTION_TTL);
                    for task_id in expired {
                        tracing::info!(
                            task_id,
                            "repository selection expired unanswered; mention dropped"
                        );
                    }
                    for draft_id in state.sweep_drafts(now) {
                        tracing::info!(
                            draft_id,
                            "draft expired unanswered; its buttons now answer as expired"
                        );
                    }
                    continue;
                }
            };
            match event {
                SocketEvent::Message(message) => {
                    let Some(mention) = filter.assess(&message) else {
                        continue;
                    };
                    let enriched = enrich(api.as_ref(), &config, &mut names, mention).await;
                    // Resolution can block for minutes on a slow LLM; run it
                    // off the event loop so button clicks and further
                    // mentions are never head-of-line blocked behind it.
                    tokio::spawn(handle_mention(
                        Arc::clone(&api),
                        Arc::clone(&chat),
                        Arc::clone(&config),
                        state.clone(),
                        Arc::clone(&awaiting),
                        enriched,
                    ));
                }
                SocketEvent::BlockActions(payload) => {
                    handle_block_actions(api.as_ref(), &config, &state, &awaiting, &payload).await;
                }
            }
        }
    })
}

/// Resolve the repository for an enriched mention and either submit the task
/// or park it behind an ephemeral selection. Runs as its own task (spawned
/// per mention), so slow LLM calls never stall the event loop.
async fn handle_mention<T: SlackTransport, C: ChatTransport>(
    api: Arc<SlackApi<T>>,
    chat: Arc<C>,
    config: Arc<SlackConfig>,
    state: SharedState,
    awaiting: Arc<Mutex<AwaitingSelection>>,
    enriched: EnrichedMention,
) {
    let context_text = enriched.context_lines.as_deref().unwrap_or(&[]).join("\n");
    let resolution = resolve(
        chat.as_ref(),
        &config,
        &enriched.channel_name,
        &enriched.mention.text,
        &context_text,
    )
    .await;

    match resolution {
        Resolution::Resolved(repo) => {
            submit(&state, &config, &enriched, Some(repo));
        }
        Resolution::NeedsSelection(candidates) => {
            let task_id = enriched.mention.task_id();
            // Park BEFORE posting so an operator answering within
            // milliseconds cannot race an entry that is not there yet.
            awaiting
                .lock()
                .unwrap()
                .insert(enriched.clone(), Instant::now());
            match post_selection_ephemeral(api.as_ref(), &config, &enriched, &candidates).await {
                Ok(()) => {
                    tracing::info!(task_id, "asked the operator to pick a repository");
                }
                Err(e) => {
                    // Without the ephemeral the operator can never answer;
                    // parking the mention would strand it silently. Submit
                    // without a hint instead — the orchestrator's own repo
                    // selection handles hintless tasks.
                    tracing::warn!(
                        task_id, error = %e,
                        "could not post the repository picker; submitting without a hint"
                    );
                    awaiting.lock().unwrap().take(&task_id);
                    submit(&state, &config, &enriched, None);
                }
            }
        }
    }
}

/// Answer a Block Kit interaction: repository picks and skips are consumed
/// here; approve/reject presses are delegated to the approval flow. The
/// action-id spaces are disjoint (`select_repo_*` / `skip_mention` vs.
/// `approve_reply` / `reject_reply`).
async fn handle_block_actions<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    state: &SharedState,
    awaiting: &Arc<Mutex<AwaitingSelection>>,
    payload: &Value,
) {
    let Some(action) = payload.get("actions").and_then(|a| a.get(0)) else {
        return;
    };
    let action_id = action
        .get("action_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let value = action.get("value").and_then(Value::as_str).unwrap_or("");
    let response_url = payload.get("response_url").and_then(Value::as_str);

    if action_id.starts_with("select_repo") {
        let (task_id, repo) = match parse_selection_value(value) {
            Some(parsed) => parsed,
            None => {
                tracing::warn!(value, "select_repo action with an unparseable value");
                return;
            }
        };
        let Some(enriched) = awaiting.lock().unwrap().take(&task_id) else {
            // Restart, TTL expiry, or a double click: nothing to resume.
            tracing::info!(task_id, "selection answered but no mention is waiting");
            replace_ephemeral(
                api,
                response_url,
                "この選択は期限切れか処理済みです（再起動などで無効になった可能性があります）。",
            )
            .await;
            return;
        };
        tracing::info!(task_id, repo, "operator picked the repository");
        submit(state, config, &enriched, Some(repo.clone()));
        replace_ephemeral(
            api,
            response_url,
            &format!("リポジトリ `{repo}` で調査を開始します。"),
        )
        .await;
    } else if action_id == "skip_mention" {
        let Some(task_id) = parse_skip_value(value) else {
            tracing::warn!(value, "skip_mention action with an unparseable value");
            return;
        };
        if awaiting.lock().unwrap().take(&task_id).is_some() {
            tracing::info!(task_id, "operator skipped the mention; dropped");
        }
        replace_ephemeral(
            api,
            response_url,
            "このメンションをスキップしました（返信案は作成されません）。",
        )
        .await;
    } else if action_id == "approve_reply" || action_id == "reject_reply" {
        crate::approval::handle_approval_action(
            api,
            state,
            &config.source_name,
            payload,
            action_id,
            value,
            response_url,
        )
        .await;
    } else {
        tracing::debug!(action_id, "ignoring block action (unknown action_id)");
    }
}

/// Build the task from an enriched mention and queue it.
fn submit(
    state: &SharedState,
    config: &SlackConfig,
    enriched: &EnrichedMention,
    repo_hint: Option<String>,
) {
    let (task, pending) = build_task(config, enriched, repo_hint);
    tracing::info!(task_id = task.id, "mention became a task; buffered");
    state.insert_pending(task.id.clone(), pending);
    state.push_task(task);
}

/// The ephemeral repository picker: one button per candidate plus a skip,
/// visible only to the operator, inside the mention's thread.
async fn post_selection_ephemeral<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    enriched: &EnrichedMention,
    candidates: &[String],
) -> Result<(), crate::error::SlackError> {
    let task_id = enriched.mention.task_id();
    let snippet: String = enriched.mention.text.chars().take(80).collect();

    let mut elements = Vec::new();
    for (i, name) in candidates.iter().enumerate() {
        elements.push(json!({
            "type": "button",
            "action_id": format!("select_repo_{i}"),
            "text": { "type": "plain_text", "text": name },
            "value": json!({ "task": task_id, "repo": name }).to_string(),
        }));
    }
    elements.push(json!({
        "type": "button",
        "action_id": "skip_mention",
        "style": "danger",
        "text": { "type": "plain_text", "text": "スキップ（返信案を作らない）" },
        "value": json!({ "task": task_id }).to_string(),
    }));

    let mut blocks = vec![json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!(
                "どのリポジトリについてのメンションですか？\n> {snippet}\n\
                 選ぶと返信案の作成を開始します。"
            ),
        }
    })];
    // Slack rejects an `actions` block with more than 25 elements; chunk so
    // a large [[repos]] catalog still gets a working picker.
    for chunk in elements.chunks(25) {
        blocks.push(json!({ "type": "actions", "elements": chunk }));
    }
    let blocks = Value::Array(blocks);

    api.chat_post_ephemeral(&PostEphemeral {
        channel: &enriched.mention.channel,
        user: &config.target_user_id,
        text: "リポジトリを選択してください",
        thread_ts: Some(enriched.mention.reply_ts()),
        blocks: Some(blocks),
    })
    .await
}

/// Replace the ephemeral the button lived in (best-effort: the URL is only
/// valid for 30 minutes / 5 uses, and a failed rewrite costs nothing).
async fn replace_ephemeral<T: SlackTransport>(
    api: &SlackApi<T>,
    response_url: Option<&str>,
    text: &str,
) {
    let Some(url) = response_url else { return };
    let body = json!({ "replace_original": true, "text": text });
    if let Err(e) = api.post_response_url(url, body).await {
        tracing::warn!(error = %e, "could not rewrite the selection ephemeral");
    }
}

/// The `{"task": …, "repo": …}` value of a `select_repo` button.
fn parse_selection_value(value: &str) -> Option<(String, String)> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    Some((
        parsed.get("task")?.as_str()?.to_string(),
        parsed.get("repo")?.as_str()?.to_string(),
    ))
}

/// The `{"task": …}` value of a `skip_mention` button.
fn parse_skip_value(value: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    Some(parsed.get("task")?.as_str()?.to_string())
}

/// Caches for display names and channel names (stable within a run).
#[derive(Default)]
struct NameCache {
    users: HashMap<String, String>,
    channels: HashMap<String, String>,
}

impl NameCache {
    /// Display name for `user_id`; falls back to the raw id.
    async fn user<T: SlackTransport>(&mut self, api: &SlackApi<T>, user_id: &str) -> String {
        if let Some(hit) = self.users.get(user_id) {
            return hit.clone();
        }
        match api.users_info(user_id).await {
            Ok(name) => {
                self.users.insert(user_id.to_string(), name.clone());
                name
            }
            // Not cached: a transient failure must not pin the raw id for
            // the rest of the run.
            Err(e) => {
                tracing::warn!(user_id, error = %e, "users.info failed; using the raw id");
                user_id.to_string()
            }
        }
    }

    /// Channel name for `channel_id`; falls back to the raw id.
    async fn channel<T: SlackTransport>(&mut self, api: &SlackApi<T>, channel_id: &str) -> String {
        if let Some(hit) = self.channels.get(channel_id) {
            return hit.clone();
        }
        let name = match api.conversations_info_name(channel_id).await {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!(channel_id, error = %e, "conversations.info failed; \
                     using the raw id");
                channel_id.to_string()
            }
        };
        self.channels.insert(channel_id.to_string(), name.clone());
        name
    }
}

/// Title snippet length, in characters.
const TITLE_SNIPPET_CHARS: usize = 40;

/// Look up everything a mention needs (names, thread context, permalink).
/// Best-effort: a failed lookup degrades the result (raw ids, missing
/// context note) instead of dropping the mention.
async fn enrich<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    names: &mut NameCache,
    mention: Mention,
) -> EnrichedMention {
    let sender_name = names.user(api, &mention.user).await;
    let channel_name = names.channel(api, &mention.channel).await;
    let permalink = match api.chat_get_permalink(&mention.channel, &mention.ts).await {
        Ok(link) => Some(link),
        Err(e) => {
            tracing::warn!(error = %e, "chat.getPermalink failed; task will have no url");
            None
        }
    };
    let context_lines = thread_context(api, config, names, &mention).await;
    EnrichedMention {
        mention,
        sender_name,
        channel_name,
        permalink,
        context_lines,
    }
}

/// Normalize an enriched mention to the common [`Task`] schema.
fn build_task(
    config: &SlackConfig,
    enriched: &EnrichedMention,
    repo_hint: Option<String>,
) -> (Task, PendingMention) {
    let mention = &enriched.mention;
    let snippet: String = mention
        .text
        .replace('\n', " ")
        .chars()
        .take(TITLE_SNIPPET_CHARS)
        .collect();
    let title = format!(
        "Slack: {} in #{}: {snippet}",
        enriched.sender_name, enriched.channel_name
    );

    let mut body = String::from(
        "以下の Slack メンションへの返信案を日本語で作成してください。\
         対象リポジトリを調査し、根拠を持って回答してください。\
         出力は返信文のみとし、前置き・後書き・説明を含めないでください。\n",
    );
    if let Some(style) = &config.reply_style {
        body.push_str(&format!("返信スタイル: {style}\n"));
    }
    body.push_str(&format!(
        "\n## メンション\n\n- 送信者: {}\n- チャンネル: #{}\n- 本文:\n\n> {}\n",
        enriched.sender_name,
        enriched.channel_name,
        mention.text.replace('\n', "\n> ")
    ));
    match &enriched.context_lines {
        Some(lines) if !lines.is_empty() => {
            body.push_str(&format!(
                "\n## スレッド文脈（直近 {} 件・古い順）\n\n",
                lines.len()
            ));
            for line in lines {
                body.push_str(&format!("- {line}\n"));
            }
        }
        Some(_) => {}
        None => body.push_str(
            "\n## スレッド文脈\n\n（スレッド文脈の取得に失敗したため省略されています）\n",
        ),
    }

    let task = Task {
        id: mention.task_id(),
        source: config.source_name.clone(),
        title,
        body: Some(body),
        repo_hint,
        labels: Vec::new(),
        priority: 0,
        status: None,
        url: enriched.permalink.clone(),
        assignee: None,
    };
    let pending = PendingMention {
        channel: mention.channel.clone(),
        reply_ts: mention.reply_ts().to_string(),
        mention_ts: mention.ts.clone(),
        sender_name: enriched.sender_name.clone(),
        permalink: enriched.permalink.clone(),
    };
    (task, pending)
}

/// The last `thread_context_limit` thread messages before the mention, as
/// `name: text` lines (oldest first). `None` when the lookup failed, `Some
/// (empty)` when the mention is not in a thread.
async fn thread_context<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    names: &mut NameCache,
    mention: &Mention,
) -> Option<Vec<String>> {
    let Some(thread_ts) = &mention.thread_ts else {
        return Some(Vec::new());
    };
    // Window the thread from above at the mention itself (`latest`), so a
    // long thread yields the messages leading up to the mention, not its
    // head; +1 covers dropping the mention from the result.
    let fetch_limit = config.thread_context_limit.saturating_add(1).min(200);
    let messages = match api
        .conversations_replies(&mention.channel, thread_ts, fetch_limit, Some(&mention.ts))
        .await
    {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!(error = %e, "conversations.replies failed; \
                 the task body will note the missing context");
            return None;
        }
    };

    let mut lines = Vec::new();
    for message in messages
        .iter()
        .filter(|m| m.ts != mention.ts)
        .rev()
        .take(config.thread_context_limit as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let speaker = match &message.user {
            Some(user) => names.user(api, user).await,
            None => message
                .bot_id
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string()),
        };
        lines.push(format!("{speaker}: {}", message.text.replace('\n', " ")));
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enriched(task_id_ts: &str) -> EnrichedMention {
        EnrichedMention {
            mention: Mention {
                channel: "C1".into(),
                user: "U_OTHER".into(),
                text: "<@U_ME> hi".into(),
                ts: task_id_ts.into(),
                thread_ts: None,
            },
            sender_name: "alice".into(),
            channel_name: "general".into(),
            permalink: None,
            context_lines: Some(Vec::new()),
        }
    }

    #[test]
    fn awaiting_sweep_drops_only_expired_entries() {
        let mut awaiting = AwaitingSelection::default();
        let start = Instant::now();
        awaiting.insert(enriched("1.0"), start);
        awaiting.insert(enriched("2.0"), start + Duration::from_secs(60 * 60));

        // 24h after the first insert: the first has hit the TTL, the second
        // (1h younger) has not.
        let expired = awaiting.sweep(start + SELECTION_TTL, SELECTION_TTL);
        assert_eq!(expired, vec!["C1:1.0".to_string()]);
        assert!(awaiting.take("C1:1.0").is_none(), "expired entry is gone");
        assert!(awaiting.take("C1:2.0").is_some(), "fresh entry survives");
    }

    #[test]
    fn selection_values_round_trip() {
        let value = json!({ "task": "C1:1.0", "repo": "web-app" }).to_string();
        assert_eq!(
            parse_selection_value(&value),
            Some(("C1:1.0".to_string(), "web-app".to_string()))
        );
        assert!(parse_selection_value("not json").is_none());

        let value = json!({ "task": "C1:1.0" }).to_string();
        assert_eq!(parse_skip_value(&value), Some("C1:1.0".to_string()));
        assert!(parse_skip_value("{}").is_none());
    }
}
