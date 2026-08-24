//! The mention pipeline: consumes normalized Socket Mode events, applies the
//! mention filter, enriches a fresh mention with thread context and names,
//! resolves the target repository (issue #106), normalizes to the common
//! [`Task`] schema, and pushes it to the orchestrator via `task/submit`
//! (protocol 0.1.6, ADR-0008 — the orchestrator persists before acking, so
//! the plugin holds no task buffer and a restart loses nothing acked).
//!
//! Repository resolution runs entirely in the plugin: channel-prefix rules,
//! then the plugin's own LLM classifier, then — when neither decides — an
//! in-thread ephemeral asking the operator. While a selection is pending the
//! mention is **not** submitted as a task (no dangling pending task in the
//! orchestrator); the `block_actions` answer resumes it, and unanswered
//! selections expire after `SELECTION_TTL`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use plugin_sdk::{LookupClient, SubmitOutcome, Submitter};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use plugin_protocol::Task;

use crate::config::SlackConfig;
use crate::draft::{DRAFT_TTL, Draft, DraftStatus, DraftStore};
use crate::llm::ChatTransport;
use crate::mention::{Mention, MentionFilter};
use crate::reaction::{ReactionTriggers, reaction_target, to_mention};
use crate::repo_resolver::{Resolution, resolve};
use crate::slack_api::{PostEphemeral, SlackApi};
use crate::socket_mode::SocketEvent;
use crate::template;
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
    /// Sender's Slack user id (for the `<@…>` mention prefix on the reply).
    pub sender_id: String,
    /// Sender display name (for the draft header).
    pub sender_name: String,
    /// Permalink to the mention (for the record), when resolvable.
    pub permalink: Option<String>,
    /// The workflow this task was submitted under (#554).
    ///
    /// `result/publish` arrives with a task id and nothing else, so this is
    /// how the plugin gets back to the workflow whose `publish` key decides
    /// whether the result goes through the approval gate.
    pub workflow: String,
}

/// Bound on the pending-mention index. `result/publish` (#107) consumes
/// entries, but until every task round-trips, the oldest entries fall out
/// FIFO instead of growing without bound in a long-running plugin.
const PENDING_CAP: usize = 1024;

/// How long an unanswered repository selection stays alive.
const SELECTION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often expired selections are swept.
const SELECTION_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// State shared between the pipeline task and the JSON-RPC server: the
/// pending-mention index, the draft store (#107), the resolved self-DM
/// record channel, and the resolved bot-DM nudge channel (#305). (The task
/// buffer is gone — tasks are pushed via `task/submit` the moment they are
/// built, 0.1.6.)
#[derive(Clone, Default)]
pub struct SharedState {
    pending: Arc<Mutex<PendingIndex>>,
    drafts: Arc<Mutex<DraftStore>>,
    self_dm: Arc<Mutex<Option<String>>>,
    bot_dm: Arc<Mutex<Option<String>>>,
}

/// The pending-mention map plus its FIFO eviction order.
#[derive(Default)]
struct PendingIndex {
    entries: HashMap<String, PendingMention>,
    order: std::collections::VecDeque<String>,
}

impl SharedState {
    /// A state whose draft store was loaded from — and mirrors to — disk
    /// (#122). `Default` keeps the in-memory-only store (tests, or an
    /// environment where no state directory could be resolved).
    pub fn new(drafts: DraftStore) -> Self {
        Self {
            drafts: Arc::new(Mutex::new(drafts)),
            ..Self::default()
        }
    }

    /// Remember where `task_id`'s reply belongs (bounded: beyond
    /// `PENDING_CAP`, the oldest entry is evicted with a warning).
    ///
    /// Since #242 the key is the **conversation**, so a follow-up mention
    /// overwrites the thread's entry rather than adding one. That is the
    /// wanted behaviour: the agent answers the latest turn, and the reply
    /// should address whoever asked it.
    pub fn insert_pending(&self, task_id: String, pending: PendingMention) {
        let mut index = self.pending.lock().unwrap();
        // Overwriting refreshes the eviction position. Before #242 an
        // overwrite was practically unreachable (one task per message), so
        // leaving the order alone was the same as appending; now a live
        // conversation would keep the position of its *first* message and be
        // evicted ahead of threads nobody has touched in weeks.
        index.order.retain(|id| id != &task_id);
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
        index.entries.insert(task_id, pending);
    }

    /// The Slack coordinates for `task_id`, if it is still pending.
    pub fn pending(&self, task_id: &str) -> Option<PendingMention> {
        self.pending.lock().unwrap().entries.get(task_id).cloned()
    }

    /// Remove and return `task_id`'s coordinates — the terminal consumption
    /// at `result/publish` time, which also keeps the index from holding
    /// entries for tasks that already round-tripped.
    /// The workflow a pending task was submitted under (#554), without
    /// consuming the entry — `result/publish` needs it *before* deciding which
    /// presentation path takes (and consumes) it.
    pub fn workflow_of(&self, task_id: &str) -> Option<String> {
        self.pending
            .lock()
            .unwrap()
            .entries
            .get(task_id)
            .map(|p| p.workflow.clone())
    }

    pub fn take_pending(&self, task_id: &str) -> Option<PendingMention> {
        let mut index = self.pending.lock().unwrap();
        let taken = index.entries.remove(task_id);
        if taken.is_some() {
            index.order.retain(|id| id != task_id);
        }
        taken
    }

    /// Drop `task_id`'s coordinates **only if they are still the ones this
    /// delivery installed** (identified by `mention_ts`).
    ///
    /// The rollback path for a submission that permanently failed. Since #242
    /// the key is the conversation, so a blind removal is a live hazard: a
    /// second message whose submit fails would take down the coordinates the
    /// *first* one installed, and that first task — running happily — would
    /// find nothing at `result/publish` time and lose its reply.
    fn discard_pending_delivery(&self, task_id: &str, mention_ts: &str) {
        let mut index = self.pending.lock().unwrap();
        if index
            .entries
            .get(task_id)
            .is_some_and(|p| p.mention_ts == mention_ts)
        {
            index.entries.remove(task_id);
            index.order.retain(|id| id != task_id);
        }
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

    /// Record the resolved bot↔operator DM channel the notification nudges
    /// go to (#305; set once by the pipeline at startup when a `bot_token`
    /// is configured).
    pub fn set_bot_dm_channel(&self, channel: String) {
        *self.bot_dm.lock().unwrap() = Some(channel);
    }

    /// The bot↔operator DM channel — `None` when the nudge is disabled (no
    /// `bot_token`) or startup resolution failed.
    pub fn bot_dm_channel(&self) -> Option<String> {
        self.bot_dm.lock().unwrap().clone()
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

    /// Drop drafts past [`DRAFT_TTL`], returning the dropped ids. Wall-clock
    /// (`SystemTime`): draft ages span restarts (#122).
    pub fn sweep_drafts(&self, now: SystemTime) -> Vec<String> {
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

/// Bound on parked selections, mirroring `PENDING_CAP`'s rationale: an
/// LLM outage that degrades every mention to a picker must not grow memory
/// without bound until the 24h TTL. Evicting loses the mention (same
/// semantics as TTL expiry), logged as a warning.
const AWAITING_CAP: usize = 256;

/// Mentions waiting for the operator's repository choice, keyed by
/// **message key** — one entry per parked *mention*.
///
/// Not by task id: since #242 that names the whole thread, so two mentions
/// posted into one thread before either was answered would collide and the
/// second would evict the first, dropping a message the operator never got
/// to answer.
///
/// Kept out of [`SharedState`] (only the pipeline touches it), but shared
/// with the per-mention resolution tasks, hence used behind a lock.
#[derive(Default)]
struct AwaitingSelection {
    entries: HashMap<String, (EnrichedMention, Instant)>,
    order: std::collections::VecDeque<String>,
}

impl AwaitingSelection {
    fn insert(&mut self, enriched: EnrichedMention, now: Instant) {
        let key = enriched.mention.message_key();
        if !self.entries.contains_key(&key) {
            if self.order.len() >= AWAITING_CAP
                && let Some(evicted) = self.order.pop_front()
            {
                self.entries.remove(&evicted);
                tracing::warn!(
                    message_key = %evicted,
                    "awaiting-selection store full; evicted the oldest parked mention"
                );
            }
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, (enriched, now));
    }

    fn take(&mut self, message_key: &str) -> Option<EnrichedMention> {
        let taken = self.entries.remove(message_key).map(|(e, _)| e);
        if taken.is_some() {
            self.order.retain(|key| key != message_key);
        }
        taken
    }

    /// Drop entries older than `ttl`, returning the dropped message keys.
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

/// The plugin's two channels to the Orchestrator: push a task in
/// (`task/submit`, 0.1.6) and ask whether a conversation is already known
/// (`task/lookup`, 0.2.4). Bundled because every per-mention task needs both.
struct Orchestrator<S> {
    submit: S,
    lookup: LookupClient,
}

// Derived `Clone` would demand `S: Clone` on the *struct*, which is stricter
// than what the pipeline needs.
impl<S: Clone> Clone for Orchestrator<S> {
    fn clone(&self) -> Self {
        Self {
            submit: self.submit.clone(),
            lookup: self.lookup.clone(),
        }
    }
}

/// Run the pipeline over `events` until the channel closes: filter each
/// message event, enrich + resolve + normalize fresh mentions, push results
/// via `submitter` (`task/submit`, 0.1.6), and answer `block_actions`
/// (repository selections and the approval flow's approve/reject presses).
#[allow(clippy::too_many_arguments)]
pub fn spawn<T, C, S>(
    api: Arc<SlackApi<T>>,
    chat: Arc<C>,
    config: Arc<SlackConfig>,
    // Resolved at `initialize`, where the workflow triggers are in hand
    // (#396); the pipeline is handed the answer rather than re-deriving it.
    trigger_reactions: ReactionTriggers,
    mut events: mpsc::UnboundedReceiver<SocketEvent>,
    state: SharedState,
    submitter: S,
    lookup: LookupClient,
) -> tokio::task::JoinHandle<()>
where
    T: SlackTransport + 'static,
    C: ChatTransport + 'static,
    S: Submitter + Clone + 'static,
{
    tokio::spawn(async move {
        let mut filter = MentionFilter::new(
            &config.target_user_id,
            trigger_reactions.mention_workflow().map(str::to_string),
        );
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
        // Resolve the bot↔operator DM the notification nudges go to (#305).
        // Also once, also non-fatal: without it the nudges are skipped and
        // the draft/picker surfaces still work — the operator just gets no
        // push. (No mention-filter row needed: `message.im` is not
        // subscribed, so bot-DM posts never enter the pipeline.)
        match &config.bot_token {
            None => {
                tracing::warn!(
                    "`bot_token` is not configured; drafts and pickers will generate no \
                     Slack push notification (see `[slack]` in config.toml to enable the nudge)"
                );
            }
            Some(_) => match api.conversations_open_bot(&config.target_user_id).await {
                Ok(channel) => state.set_bot_dm_channel(channel),
                Err(e) => {
                    tracing::warn!(error = %e, "could not resolve the bot DM channel; \
                         notification nudges are disabled for this run");
                }
            },
        }

        let mut names = NameCache::default();
        let orchestrator = Orchestrator {
            submit: submitter,
            lookup,
        };
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
                    for message_key in expired {
                        tracing::info!(
                            message_key,
                            "repository selection expired unanswered; mention dropped"
                        );
                    }
                    for draft_id in state.sweep_drafts(SystemTime::now()) {
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
                        orchestrator.clone(),
                    ));
                }
                // #319: a trigger reaction the operator added becomes a task
                // the same way a mention does. The event carries no message
                // body, so the filter runs either side of a re-fetch.
                SocketEvent::Reaction(event) => {
                    let Some(target) =
                        reaction_target(&event, filter.target_user_id(), &trigger_reactions)
                    else {
                        continue;
                    };
                    if filter.is_self_dm_channel(&target.channel) {
                        continue;
                    }
                    // Skip a known duplicate without paying for the round trip,
                    // but do **not** record anything yet: recording here would
                    // burn the key on a transient fetch failure, and the
                    // envelope was already acked (`socket_mode`: ack first),
                    // so Slack never redelivers it. Worse, the operator's
                    // natural retry — remove the reaction and add it again —
                    // keys on the *message* ts, so it would be deduped away
                    // too. The trigger would be unrecoverable until a restart
                    // cleared the LRU.
                    let dedup_key = target.dedup_key();
                    if filter.already_processed(&dedup_key) {
                        continue;
                    }
                    let fetched = match api.fetch_message(&target.channel, &target.ts).await {
                        Ok(Some(message)) => message,
                        Ok(None) => {
                            tracing::warn!(
                                channel = target.channel,
                                ts = target.ts,
                                "reacted-to message not found; ignoring the trigger"
                            );
                            continue;
                        }
                        // One unreachable message must not take the plugin
                        // down, so this is a warn-and-drop rather than an
                        // error path.
                        Err(e) => {
                            tracing::warn!(
                                channel = target.channel,
                                ts = target.ts,
                                error = %e,
                                "could not fetch the reacted-to message; ignoring the trigger"
                            );
                            continue;
                        }
                    };
                    let Some(mention) = to_mention(&target, fetched) else {
                        continue;
                    };
                    // Only now is the trigger definitely going to produce a
                    // task, so this is where the key is spent. The re-check is
                    // not redundant with the one above: `remember` is what
                    // actually records it, and returning early on `false`
                    // keeps the mention path's contract (one message, one
                    // task) even if the two ever race.
                    if !filter.remember(dedup_key) {
                        continue;
                    }
                    // From here the two triggers are the same code path.
                    let enriched = enrich(api.as_ref(), &config, &mut names, mention).await;
                    tokio::spawn(handle_mention(
                        Arc::clone(&api),
                        Arc::clone(&chat),
                        Arc::clone(&config),
                        state.clone(),
                        Arc::clone(&awaiting),
                        enriched,
                        orchestrator.clone(),
                    ));
                }
                SocketEvent::BlockActions(payload) => {
                    handle_block_actions(
                        api.as_ref(),
                        &config,
                        &state,
                        &awaiting,
                        &payload,
                        &orchestrator.submit,
                    )
                    .await;
                }
            }
        }
    })
}

/// Resolve the repository for an enriched mention and either submit the task
/// or park it behind an ephemeral selection. Runs as its own task (spawned
/// per mention), so slow LLM calls never stall the event loop.
async fn handle_mention<T: SlackTransport, C: ChatTransport, S: Submitter>(
    api: Arc<SlackApi<T>>,
    chat: Arc<C>,
    config: Arc<SlackConfig>,
    state: SharedState,
    awaiting: Arc<Mutex<AwaitingSelection>>,
    enriched: EnrichedMention,
    orchestrator: Orchestrator<S>,
) {
    // Ask first whether this conversation already exists (#242). Resolution
    // is a *new* conversation's business: for a reply the orchestrator
    // already knows the repository, and resolving anyway would spend an LLM
    // call — or put a picker in front of an operator who already chose.
    //
    // An unanswerable lookup (timeout, transport, error) is deliberately
    // indistinguishable from "new" in effect: the mention takes the path it
    // took before this RPC existed. The orchestrator answers from its event
    // loop, which can be busy creating a worktree, so this must never be
    // load-bearing.
    //
    // Asked with the **conversation** id, not the task id. For a mention they
    // are the same string; for a prefixed task (#397) they are not, and asking
    // with the prefixed one would always answer "new" — the repository the
    // answering task already settled would be resolved from scratch.
    let answer = orchestrator
        .lookup
        .lookup(&config.source_name, &enriched.mention.conversation_id())
        .await;
    let inherited = match &answer {
        plugin_sdk::Lookup::Known { repo } => repo.as_deref(),
        _ => None,
    };
    if enriched.mention.task_id_prefix.is_some() {
        // A prefixed task is **new** to the orchestrator even when its
        // conversation is not, so "skip resolution" does not apply: nothing
        // downstream will settle a repository for it. Inherit the answering
        // task's if there is one, else fall through and resolve normally.
        if let Some(repo) = inherited {
            tracing::info!(
                task_id = enriched.mention.task_id(),
                conversation = enriched.mention.conversation_id(),
                repo,
                "inheriting the repository the conversation already settled"
            );
            submit(
                &state,
                &config,
                &enriched,
                Some(repo.to_string()),
                &orchestrator.submit,
            )
            .await;
            return;
        }
    } else if answer.skips_resolution() {
        tracing::info!(
            task_id = enriched.mention.task_id(),
            repo = tracing::field::debug(inherited),
            "a message in a conversation the orchestrator already knows; \
             submitting without resolving a repository"
        );
        // No `repo_hint`: the conversation is already bound to a repository
        // (or is in the middle of settling one), and the orchestrator
        // finishes that job. A hint here could only disagree with it.
        submit(&state, &config, &enriched, None, &orchestrator.submit).await;
        return;
    }

    // Known race: `handle_mention` is spawned per mention, so two mentions
    // posted into the *same new* thread at once can both see `known: false`
    // and both resolve — two pickers for one thread. Harmless: each parks
    // under its own message key and each submit carries its own
    // `message_key`, so the orchestrator ingests them as two messages of one
    // conversation. Only the resolution work is duplicated — and, since
    // #305, the bot nudge with it (one per posted picker). Accepted: each
    // picker is separately answerable, so "one nudge per answerable picker"
    // stays consistent; deduplicating would need cross-task coordination for
    // a rare race whose cost is one extra push notification.
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
            submit(&state, &config, &enriched, Some(repo), &orchestrator.submit).await;
        }
        Resolution::NeedsSelection(candidates) => {
            let message_key = enriched.mention.message_key();
            // Park BEFORE posting so an operator answering within
            // milliseconds cannot race an entry that is not there yet.
            awaiting
                .lock()
                .unwrap()
                .insert(enriched.clone(), Instant::now());
            match post_selection_ephemeral(api.as_ref(), &config, &enriched, &candidates).await {
                Ok(()) => {
                    tracing::info!(message_key, "asked the operator to pick a repository");
                    // The picker is an ephemeral, which generates no Slack
                    // notification of its own — nudge via the bot DM (#305).
                    crate::notify::send_nudge(
                        api.as_ref(),
                        &state,
                        "リポジトリ選択の確認が届きました",
                        enriched.permalink.as_deref(),
                        // No log payload: the picker has no content worth
                        // preserving beyond the mention it links to (#456).
                        None,
                    )
                    .await;
                }
                Err(e) => {
                    // Without the ephemeral the operator can never answer;
                    // parking the mention would strand it silently. Submit
                    // without a hint instead — the orchestrator's own repo
                    // selection handles hintless tasks.
                    tracing::warn!(
                        message_key, error = %e,
                        "could not post the repository picker; submitting without a hint"
                    );
                    awaiting.lock().unwrap().take(&message_key);
                    submit(&state, &config, &enriched, None, &orchestrator.submit).await;
                }
            }
        }
    }
}

/// Answer a Block Kit interaction: repository picks and skips are consumed
/// here; approve/reject presses are delegated to the approval flow. The
/// action-id spaces are disjoint (`select_repo_*` / `skip_mention` vs.
/// `approve_reply` / `reject_reply`).
async fn handle_block_actions<T: SlackTransport, S: Submitter>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    state: &SharedState,
    awaiting: &Arc<Mutex<AwaitingSelection>>,
    payload: &Value,
    submitter: &S,
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
        let (message_key, repo) = match parse_selection_value(value) {
            Some(parsed) => parsed,
            None => {
                tracing::warn!(value, "select_repo action with an unparseable value");
                return;
            }
        };
        let Some(enriched) = awaiting.lock().unwrap().take(&message_key) else {
            // Restart, TTL expiry, or a double click: nothing to resume.
            tracing::info!(message_key, "selection answered but no mention is waiting");
            replace_ephemeral(
                api,
                response_url,
                "この選択は期限切れか処理済みです（再起動などで無効になった可能性があります）。",
            )
            .await;
            return;
        };
        tracing::info!(message_key, repo, "operator picked the repository");
        submit(state, config, &enriched, Some(repo.clone()), submitter).await;
        replace_ephemeral(
            api,
            response_url,
            &format!("リポジトリ `{repo}` で調査を開始します。"),
        )
        .await;
    } else if action_id == "skip_mention" {
        let Some(message_key) = parse_skip_value(value) else {
            tracing::warn!(value, "skip_mention action with an unparseable value");
            return;
        };
        if awaiting.lock().unwrap().take(&message_key).is_some() {
            tracing::info!(message_key, "operator skipped the mention; dropped");
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
            config,
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

/// Build the task from an enriched mention and push it via `task/submit`
/// (0.1.6). The pending entry is inserted **before** submitting so a
/// lightning-fast `result/publish` can never miss it; a submission that
/// permanently fails withdraws **its own** entry again (the mention stays
/// answerable on Slack — re-mention to retry).
async fn submit<S: Submitter>(
    state: &SharedState,
    config: &SlackConfig,
    enriched: &EnrichedMention,
    repo_hint: Option<String>,
    submitter: &S,
) {
    let (task, mut pending) = build_task(config, enriched, repo_hint);
    let task_id = task.id.clone();
    // No workflow claims this task, so there is nowhere to submit it (#554).
    // Dropping here rather than submitting is the honest end: the
    // Orchestrator would reject it anyway, and going through the motions
    // would install a pending entry for a task that never exists.
    let Some(workflow) = enriched.mention.workflow.clone() else {
        tracing::warn!(
            task_id,
            "no workflow claims this task → configure a `[[workflows]]` entry \
             with source = \"slack\" (a mention needs one without a `reaction` \
             trigger); dropping"
        );
        return;
    };
    pending.workflow = workflow.clone();
    // Identifies *this* delivery's entry on the rollback paths below: since
    // #242 the pending index is keyed by conversation, and a sibling message
    // may have installed (or may yet install) coordinates under the same key.
    let mention_ts = pending.mention_ts.clone();
    state.insert_pending(task_id.clone(), pending);
    match submitter.submit(task, &workflow).await {
        SubmitOutcome::Accepted => {
            tracing::info!(task_id, "mention became a task; submitted");
        }
        SubmitOutcome::Duplicate => {
            tracing::info!(task_id, "mention already submitted earlier; dropped");
        }
        SubmitOutcome::Rejected { reason } => {
            tracing::warn!(
                task_id,
                "orchestrator rejected the task: {}",
                reason.as_deref().unwrap_or("no reason given")
            );
            state.discard_pending_delivery(&task_id, &mention_ts);
        }
        SubmitOutcome::GaveUp { error } => {
            tracing::error!(
                task_id,
                "task submission gave up: {error} → re-mention on Slack to retry"
            );
            state.discard_pending_delivery(&task_id, &mention_ts);
        }
    }
}

/// The ephemeral repository picker: one button per candidate plus a skip,
/// visible only to the operator, inside the mention's thread.
async fn post_selection_ephemeral<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    enriched: &EnrichedMention,
    candidates: &[String],
) -> Result<(), crate::error::SlackError> {
    // What the button hands back is the parked mention's key — its
    // **message key**, matching `AwaitingSelection`. The JSON field is still
    // named `task` on purpose: a picker posted by an older build and clicked
    // after an upgrade then still parses, and answers "expired" instead of
    // "unparseable value" (the parked entry is gone across a restart either
    // way).
    let key = enriched.mention.message_key();
    let snippet: String = enriched.mention.text.chars().take(80).collect();

    let mut elements = Vec::new();
    for (i, name) in candidates.iter().enumerate() {
        elements.push(json!({
            "type": "button",
            "action_id": format!("select_repo_{i}"),
            "text": { "type": "plain_text", "text": name },
            "value": json!({ "task": key, "repo": name }).to_string(),
        }));
    }
    elements.push(json!({
        "type": "button",
        "action_id": "skip_mention",
        "style": "danger",
        "text": { "type": "plain_text", "text": "スキップ（返信案を作らない）" },
        "value": json!({ "task": key }).to_string(),
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
        match api.conversations_info_name(channel_id).await {
            Ok(name) => {
                self.channels.insert(channel_id.to_string(), name.clone());
                name
            }
            // Not cached: a transient failure must not pin the raw id for
            // the rest of the run (same rule as `user()`, #129).
            Err(e) => {
                // Without the name, `[[channel_groups]]` prefix rules can never
                // match, so repo resolution silently degrades to the LLM /
                // ephemeral fallbacks — say why and how to fix it.
                tracing::warn!(channel_id, error = %e, "conversations.info failed; \
                     using the raw id (prefix rules will not match; if the error \
                     is `missing_scope`, re-install the app with the \
                     `channels:read` / `groups:read` user scopes from manifest.yml \
                     and refresh the Keychain token)");
                channel_id.to_string()
            }
        }
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

    // Reply-crafting directions travel as `instructions` (0.1.5), separated
    // from the factual `body`, so the host can deliver them out-of-band
    // (invisible prompt-context injection) while the pane shows only the
    // mention and its thread context.
    let p = &config.prompts;
    // Which deliverable this run is for comes from `instructions_kind` (#398),
    // never from the task-id prefix. **`triage` and `implement` both carry a
    // prefix** (`books` / `impl`), so the prefix cannot tell them apart — and
    // branching on it told a triage agent to implement and open a PR (#450).
    //
    // The approval gate is unchanged for every kind: the output is still a
    // draft the operator presses before it is posted, because a wrong report
    // is exactly the kind of message that should not go out unreviewed.
    //
    // An absent or unrecognised kind falls back to the reply draft rather than
    // guessing a deliverable — working on the wrong one is worse than
    // answering the thread. The two cases are not equally hypothetical:
    //
    // - **absent**: a core older than #404. It sends no `task_id_prefix`
    //   either (#405 came after), so this is the plain mention path and the
    //   reply draft is exactly what that core always produced.
    // - **unrecognised**: `design` reaches here from a **current** core.
    //   `source = "slack"` + `profile = "design"` is a config nothing rejects,
    //   and this plugin has no text for it — so it draws the reply draft, and
    //   `design`'s `output = "none"` then publishes nothing at all. The warn
    //   is the only signal that a configured workflow silently does nothing.
    let mut instructions = match mention.instructions_kind.as_deref() {
        Some("implement") => p.implement_instructions.clone(),
        Some("triage") => p.triage_instructions.clone(),
        Some(unhandled) => {
            tracing::warn!(
                kind = %unhandled,
                "no instruction set for this profile → falling back to the reply draft; \
                 a `design` profile on a Slack workflow also publishes nothing \
                 (output = \"none\"), so the task produces no visible result"
            );
            p.reply_instructions.clone()
        }
        None => p.reply_instructions.clone(),
    };
    if let Some(style) = &config.reply_style {
        instructions.push_str(&template::render(
            &p.reply_style_suffix,
            &[("style", style.as_str())],
        ));
    }

    // `{text}` is handed over already `>`-quoted: the newline rewrite happens
    // here, before substitution, so an override that drops the leading `> `
    // still gets sane continuation lines.
    let quoted = mention.text.replace('\n', "\n> ");
    let mut body = template::render(
        &p.body_template,
        &[
            ("sender", enriched.sender_name.as_str()),
            ("channel", enriched.channel_name.as_str()),
            ("text", quoted.as_str()),
        ],
    );
    match &enriched.context_lines {
        Some(lines) if !lines.is_empty() => {
            body.push_str(&template::render(
                &p.body_thread_header,
                &[("count", lines.len().to_string().as_str())],
            ));
            for line in lines {
                body.push_str(&template::render(
                    &p.body_thread_line,
                    &[("line", line.as_str())],
                ));
            }
        }
        Some(_) => {}
        None => body.push_str(&p.body_thread_unavailable),
    }

    let task = Task {
        id: mention.task_id(),
        source: config.source_name.clone(),
        title,
        body: Some(body),
        repo_hint,
        // How a reaction-derived task reaches its `trigger = { reaction = ... }`
        // workflow (#396). The Orchestrator re-checks the emoji against this
        // label, so its absence is not cosmetic — the task would fall through
        // to the catch-all and be answered instead of implemented.
        labels: mention
            .reaction
            .iter()
            .map(|emoji| format!("reaction:{emoji}"))
            .collect(),
        priority: 0,
        status: None,
        url: enriched.permalink.clone(),
        assignee: None,
        // This delivery's identity, which the orchestrator dedups
        // re-deliveries on. Two mentions in one thread share a task id and
        // differ here — that difference is what makes the second one a new
        // message rather than a duplicate.
        message_key: Some(mention.message_key()),
        instructions: Some(instructions),
    };
    let pending = PendingMention {
        channel: mention.channel.clone(),
        reply_ts: mention.reply_ts().to_string(),
        mention_ts: mention.ts.clone(),
        sender_id: mention.user.clone(),
        sender_name: enriched.sender_name.clone(),
        permalink: enriched.permalink.clone(),
        // Filled in by `submit`, which is where the workflow is known.
        workflow: String::new(),
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
    // Scope for a prefixed task (#393 D6, #397). Reacting to **one reply**
    // means "implement this message", so its thread is not context — pulling
    // it in would hand the agent a conversation it was not pointed at, and the
    // whole point of reacting to a specific message is to narrow the ask.
    // Reacting to the root means "implement what this thread concluded", which
    // takes the thread and falls through below.
    if mention.task_id_prefix.is_some() && !mention.is_thread_root() {
        return Some(Vec::new());
    }
    // Window the thread from above at the mention itself (`latest`), so a
    // long thread yields the messages leading up to the mention, not its
    // head; +1 covers dropping the mention from the result.
    //
    // A prefixed task reacting to the root wants the **whole** conversation,
    // not the `thread_context_limit` window: the thread is where the approach
    // was agreed, and a truncated one is a brief with the middle missing. The
    // clamp to 200 stays — `conversations.replies` pages beyond that, and
    // paging is not implemented (documented in config-reference).
    let limit = if mention.task_id_prefix.is_some() {
        u32::MAX
    } else {
        config.thread_context_limit
    };
    let fetch_limit = limit.saturating_add(1).min(200);
    // …and the same limit again below. The fetch and the trim are two separate
    // caps, and widening only the first leaves the thread trimmed to
    // `thread_context_limit` anyway — a whole-thread context that quietly is
    // not one.
    //
    // `limit`, not `fetch_limit`: the `+1` above pays for dropping the mention
    // itself from the fetched window and must not widen what is kept.
    let keep = limit as usize;
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
        .take(keep)
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
    use crate::error::SlackError;
    use crate::transport::TokenKind;
    use std::collections::VecDeque;

    /// A transport that replays a fixed script of responses, one per call.
    /// An exhausted script errors, so a test can prove a value came from the
    /// cache rather than another API call.
    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<Value, SlackError>>>,
    }

    impl SlackTransport for ScriptedTransport {
        async fn call(
            &self,
            _token: TokenKind,
            _method: &str,
            _body: Option<Value>,
            _idempotent: bool,
        ) -> Result<Value, SlackError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(SlackError::Transport("script exhausted".into())))
        }

        async fn post_url(&self, _url: &str, _body: Value) -> Result<(), SlackError> {
            unreachable!("name lookups never use the response_url channel")
        }
    }

    fn scripted(responses: Vec<Result<Value, SlackError>>) -> SlackApi<ScriptedTransport> {
        SlackApi::new(ScriptedTransport {
            responses: Mutex::new(responses.into()),
        })
    }

    /// A thread of `count` replies, plus one user-name lookup per speaker.
    ///
    /// Replies start at `1.0`; the thread root is `0.0`, so a reaction on the
    /// root does not filter one of them out of the context.
    fn thread_of(count: usize) -> Vec<Result<Value, SlackError>> {
        let messages: Vec<Value> = (1..=count)
            .map(
                |i| json!({ "user": "U_OTHER", "text": format!("msg{i}"), "ts": format!("{i}.0") }),
            )
            .collect();
        let mut script = vec![Ok(json!({ "ok": true, "messages": messages }))];
        // One `users.info` per line; the cache collapses them to one call, but
        // an extra scripted response is harmless.
        script.push(Ok(
            json!({"ok": true, "user": {"name": "alice", "profile": {"display_name": "アリス"}}}),
        ));
        script
    }

    /// A reaction on the thread **root** (`ts == thread_ts`), which is the
    /// "implement what this thread concluded" case.
    fn threaded_mention(prefix: Option<&str>) -> Mention {
        Mention {
            workflow: Some("slack-reply".into()),
            channel: "C1".into(),
            user: "U_ME".into(),
            text: "やろう".into(),
            ts: "0.0".into(),
            thread_ts: Some("0.0".into()),
            reaction: prefix.map(|_| "hammer".to_string()),
            task_id_prefix: prefix.map(str::to_string),
            instructions_kind: None,
        }
    }

    fn small_limit_config() -> SlackConfig {
        serde_json::from_value(json!({
            "app_token": "xapp-1-A1-t",
            "user_token": "xoxp-t",
            "target_user_id": "U_ME",
            "thread_context_limit": 3,
        }))
        .unwrap()
    }

    /// **A prefixed task takes the whole thread, not `thread_context_limit`.**
    ///
    /// There are two caps here — how many messages are fetched and how many are
    /// kept — and widening only the first leaves the context trimmed anyway.
    /// That is what shipped in the first draft of #397: `limit = u32::MAX` for
    /// the fetch, `take(thread_context_limit)` below it. The symptom would have
    /// been an implement task briefed on the last 3 messages of the discussion
    /// that decided its approach, with nothing anywhere saying so.
    #[tokio::test]
    async fn a_prefixed_task_keeps_the_whole_thread_not_the_context_window() {
        let api = scripted(thread_of(10));
        let config = small_limit_config();
        let mut names = NameCache::default();
        let lines = thread_context(&api, &config, &mut names, &threaded_mention(Some("impl")))
            .await
            .expect("context fetched");
        assert_eq!(
            lines.len(),
            10,
            "the prefixed task must see the whole thread, not `thread_context_limit`: {lines:?}"
        );
    }

    /// …and an ordinary mention still gets the window it always got.
    #[tokio::test]
    async fn a_mention_still_gets_the_configured_window() {
        let api = scripted(thread_of(10));
        let config = small_limit_config();
        let mut names = NameCache::default();
        let lines = thread_context(&api, &config, &mut names, &threaded_mention(None))
            .await
            .expect("context fetched");
        assert_eq!(lines.len(), 3, "{lines:?}");
    }

    /// Reacting to one reply means "implement this message" — its thread is
    /// not context, and pulling it in would hand the agent a conversation it
    /// was not pointed at.
    #[tokio::test]
    async fn a_prefixed_task_on_a_reply_takes_no_thread_context() {
        let api = scripted(vec![]); // any API call would error
        let config = small_limit_config();
        let mut names = NameCache::default();
        let mut mention = threaded_mention(Some("impl"));
        mention.ts = "5.0".into(); // a reply, not the root
        assert!(!mention.is_thread_root());
        let lines = thread_context(&api, &config, &mut names, &mention)
            .await
            .expect("no fetch, no failure");
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// #129: a failed `conversations.info` must not pin the raw id in the
    /// cache — the next lookup retries and can recover (e.g. after the
    /// operator fixes a `missing_scope` mid-run).
    #[tokio::test]
    async fn channel_lookup_failure_is_not_cached() {
        let api = scripted(vec![
            Err(SlackError::Transport("boom".into())),
            Ok(json!({"ok": true, "channel": {"name": "general"}})),
        ]);
        let mut names = NameCache::default();
        assert_eq!(names.channel(&api, "C1").await, "C1", "fallback on failure");
        assert_eq!(
            names.channel(&api, "C1").await,
            "general",
            "retried instead of serving the pinned raw id"
        );
        // The script is exhausted: a third call would fail, so "general"
        // proves the success (and only the success) was cached.
        assert_eq!(names.channel(&api, "C1").await, "general");
    }

    /// The `user()` twin of the test above (its fix predates #129, a997d3c).
    #[tokio::test]
    async fn user_lookup_failure_is_not_cached() {
        let api = scripted(vec![
            Err(SlackError::Transport("boom".into())),
            Ok(json!({"ok": true, "user": {"profile": {"display_name": "alice"}}})),
        ]);
        let mut names = NameCache::default();
        assert_eq!(names.user(&api, "U1").await, "U1", "fallback on failure");
        assert_eq!(names.user(&api, "U1").await, "alice", "retried");
        assert_eq!(names.user(&api, "U1").await, "alice", "cached");
    }

    fn enriched(task_id_ts: &str) -> EnrichedMention {
        EnrichedMention {
            mention: Mention {
                workflow: Some("slack-reply".into()),
                channel: "C1".into(),
                user: "U_OTHER".into(),
                text: "<@U_ME> hi".into(),
                ts: task_id_ts.into(),
                thread_ts: None,
                reaction: None,
                task_id_prefix: None,
                instructions_kind: None,
            },
            sender_name: "alice".into(),
            channel_name: "general".into(),
            permalink: None,
            context_lines: Some(Vec::new()),
        }
    }

    fn slack_config() -> SlackConfig {
        serde_json::from_value(json!({
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
        }))
        .unwrap()
    }

    #[test]
    fn an_in_thread_mention_is_another_message_of_the_thread_s_task() {
        // #242: a reply carries the *thread's* id and its own message key, so
        // the orchestrator appends it to that conversation instead of opening
        // a second task.
        let mut enriched = enriched("100.1");
        enriched.mention.thread_ts = Some("100.0".into());
        let (task, _pending) = build_task(&slack_config(), &enriched, None);
        assert_eq!(task.id, "C1:100.0");
        assert_eq!(task.message_key.as_deref(), Some("C1:100.1"));
    }

    #[test]
    fn a_top_level_mention_opens_a_task_whose_id_is_unchanged_by_242() {
        // No thread: id == message key. This equality is why the change of
        // meaning needed no data migration — an opening mention's task id (and
        // therefore its branch name and worktree path) is what it always was.
        let (task, _pending) = build_task(&slack_config(), &enriched("200.0"), None);
        assert_eq!(task.id, "C1:200.0");
        assert_eq!(task.message_key.as_deref(), Some(task.id.as_str()));
    }

    #[test]
    fn build_task_splits_instructions_from_body() {
        // The reply-crafting directive (+ reply_style) rides `instructions`
        // (0.1.5) so the host can inject it invisibly; the body keeps only the
        // factual content (mention + thread context) shown in the pane.
        let config: SlackConfig = serde_json::from_value(json!({
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "reply_style": "簡潔・断定調",
        }))
        .unwrap();
        let (task, _pending) = build_task(&config, &enriched("300.0"), None);

        let instructions = task.instructions.expect("instructions are set");
        assert!(instructions.contains("Draft a reply to the Slack mention"));
        assert!(instructions.contains("Reply style: 簡潔・断定調"));

        let body = task.body.expect("body is set");
        assert!(body.starts_with("## メンション\n"), "body: {body}");
        assert!(
            !body.contains("Draft a reply to the Slack mention"),
            "no directive in body"
        );
        assert!(!body.contains("Reply style:"), "no style in body");
    }

    /// One `EnrichedMention` per profile shape, differing only in what the
    /// Orchestrator baked into the trigger.
    fn enriched_with(prefix: Option<&str>, kind: Option<&str>) -> EnrichedMention {
        let mut e = enriched("300.0");
        e.mention.task_id_prefix = prefix.map(str::to_string);
        e.mention.instructions_kind = kind.map(str::to_string);
        e
    }

    /// **The instruction set follows `instructions_kind`, never the task-id
    /// prefix (#450).**
    ///
    /// `triage` and `implement` both carry a prefix (`books` / `impl`), so the
    /// prefix cannot separate them — and the pre-#450 code branched on exactly
    /// that, handing a triage run the implement directive ("実装して Pull
    /// Request を作成"). Reverting `build_task` to `task_id_prefix.is_some()`
    /// fails the triage row here.
    #[test]
    fn the_instruction_set_follows_the_kind_not_the_prefix() {
        let config: SlackConfig = serde_json::from_value(json!({
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
        }))
        .unwrap();
        let instructions_for = |prefix, kind| {
            build_task(&config, &enriched_with(prefix, kind), None)
                .0
                .instructions
                .expect("instructions are always set")
        };

        // triage: prefixed, but the deliverable is an issue — not a PR.
        let triage = instructions_for(Some("books"), Some("triage"));
        assert!(
            triage.contains("file it as a GitHub issue"),
            "triage must be told to file an issue: {triage}"
        );
        assert!(
            !triage.contains("Implement what this thread agreed on"),
            "triage must NOT be told to implement — this is #450: {triage}"
        );

        // implement: prefixed too, and this one really is a PR run.
        let implement = instructions_for(Some("impl"), Some("implement"));
        assert!(
            implement.contains("Implement what this thread agreed on"),
            "implement keeps its directive: {implement}"
        );

        // answer / plain mention: no prefix, no kind → the reply draft.
        let answer = instructions_for(None, None);
        assert!(
            answer.contains("Draft a reply to the Slack mention"),
            "the catch-all still drafts a reply: {answer}"
        );

        // An unrecognised kind from a *newer* core degrades to the reply
        // draft rather than guessing a deliverable, and a prefix present
        // without a kind (an older core) does the same — neither may silently
        // resolve to "implement".
        //
        // The negative markers are the *distinctive openings*, not a phrase
        // like "pull request": every one of the three directives mentions one
        // (`reply` to forbid it, `implement` to require it), so matching on
        // that would assert something weaker than intended.
        // `design` leads this list because a **current** core sends it: it is
        // the one unhandled kind actually reachable today
        // (`instructions_kind(Profile::Design) == Some("design")`, and nothing
        // rejects `source = "slack"` + `profile = "design"`). The pairing
        // `(Some("impl"), None)` is deliberately absent — `instructions_kind`
        // shipped in #404, *before* `task_id_prefix` in #405, so no released
        // core sends a prefix without a kind.
        for (prefix, kind) in [
            (None, Some("design")),
            (Some("books"), Some("future-profile")),
            (None, None),
        ] {
            let degraded = instructions_for(prefix, kind);
            assert!(
                degraded.contains("Draft a reply to the Slack mention")
                    && !degraded.contains("Implement what this thread agreed on")
                    && !degraded.contains("file it as a GitHub issue"),
                "unknown/absent kind must fall back to the reply draft ({prefix:?}, {kind:?}): {degraded}"
            );
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

    fn coords(mention_ts: &str) -> PendingMention {
        PendingMention {
            workflow: "slack-reply".into(),
            channel: "C1".into(),
            reply_ts: "100.0".into(),
            mention_ts: mention_ts.into(),
            sender_id: "U_OTHER".into(),
            sender_name: "アリス".into(),
            permalink: None,
        }
    }

    #[test]
    fn a_failed_submit_only_withdraws_its_own_coordinates() {
        // #242 made the pending index conversation-keyed, which turns a blind
        // rollback into a live hazard: the first message's task is running
        // fine, a second message in the same thread fails to submit, and a
        // blind `take_pending` would delete the only entry — leaving the
        // finished first task with nowhere to put its reply.
        let state = SharedState::default();
        state.insert_pending("C1:100.0".into(), coords("100.1"));

        // A sibling delivery that never became the current entry.
        state.discard_pending_delivery("C1:100.0", "100.2");
        assert_eq!(
            state.pending("C1:100.0").map(|p| p.mention_ts),
            Some("100.1".to_string()),
            "another delivery's failure must not take these coordinates down"
        );

        // Its own failure does withdraw them.
        state.discard_pending_delivery("C1:100.0", "100.1");
        assert!(state.pending("C1:100.0").is_none());
    }

    #[test]
    fn overwriting_a_conversation_refreshes_its_eviction_position() {
        // Since #242 a follow-up overwrites its conversation's entry. If the
        // order were left alone, a live conversation would keep the position
        // of its *first* message and be evicted ahead of threads nobody has
        // touched since.
        let state = SharedState::default();
        state.insert_pending("old".into(), coords("1"));
        state.insert_pending("other".into(), coords("2"));
        // `old` gets a new message: it is now the most recently touched.
        state.insert_pending("old".into(), coords("3"));

        let order: Vec<String> = state
            .pending
            .lock()
            .unwrap()
            .order
            .iter()
            .cloned()
            .collect();
        assert_eq!(order, vec!["other".to_string(), "old".to_string()]);
        // Overwriting must not duplicate the key either, or the index would
        // evict a live entry while a stale name for it lingers.
        assert_eq!(state.pending.lock().unwrap().entries.len(), 2);
    }
}
