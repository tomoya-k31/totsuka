//! The mention pipeline: consumes normalized Socket Mode events, applies the
//! mention filter, enriches a fresh mention with thread context and names,
//! normalizes it to the common [`Task`] schema, and buffers it until the
//! orchestrator's next `tasks/fetch` drains the buffer (pull loop over a
//! push source, issue #105).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use plugin_protocol::Task;

use crate::config::SlackConfig;
use crate::mention::{Mention, MentionFilter};
use crate::slack_api::SlackApi;
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

/// State shared between the pipeline task and the JSON-RPC server: the task
/// buffer `tasks/fetch` drains, and the pending-mention index.
#[derive(Clone, Default)]
pub struct SharedState {
    buffer: Arc<Mutex<Vec<Task>>>,
    pending: Arc<Mutex<PendingIndex>>,
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
}

/// Run the pipeline over `events` until the channel closes: filter each
/// message event, enrich + normalize fresh mentions, and hand the result to
/// `state`. Block Kit interactions are ignored here (repo selection lands
/// with #106, the approval flow with #107).
pub fn spawn<T: SlackTransport + 'static>(
    api: Arc<SlackApi<T>>,
    config: Arc<SlackConfig>,
    mut events: mpsc::UnboundedReceiver<SocketEvent>,
    state: SharedState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut filter = MentionFilter::new(&config.target_user_id);
        // Resolve the self-DM record channel up front (filter row 3). Failure
        // is not fatal: row 2 (own posts) already breaks reply loops.
        match api.conversations_open_self(&config.target_user_id).await {
            Ok(channel) => filter.set_self_dm_channel(channel),
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve the self-DM channel; \
                     continuing without that filter row");
            }
        }

        let mut names = NameCache::default();
        while let Some(event) = events.recv().await {
            match event {
                SocketEvent::Message(message) => {
                    let Some(mention) = filter.assess(&message) else {
                        continue;
                    };
                    let (task, pending) =
                        normalize(api.as_ref(), &config, &mut names, &mention).await;
                    tracing::info!(task_id = task.id, "mention detected; task buffered");
                    state.insert_pending(task.id.clone(), pending);
                    state.push_task(task);
                }
                SocketEvent::BlockActions(_) => {
                    tracing::debug!(
                        "ignoring block_actions (repo selection / approval land with #106/#107)"
                    );
                }
            }
        }
    })
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
        let name = match api.users_info(user_id).await {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!(user_id, error = %e, "users.info failed; using the raw id");
                user_id.to_string()
            }
        };
        self.users.insert(user_id.to_string(), name.clone());
        name
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

/// Enrich `mention` (names, thread context, permalink) and normalize it to
/// the common [`Task`] schema. Enrichment is best-effort: a failed lookup
/// degrades the task (raw ids, missing context note) instead of dropping the
/// mention.
async fn normalize<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    names: &mut NameCache,
    mention: &Mention,
) -> (Task, PendingMention) {
    let sender = names.user(api, &mention.user).await;
    let channel_name = names.channel(api, &mention.channel).await;
    let permalink = match api.chat_get_permalink(&mention.channel, &mention.ts).await {
        Ok(link) => Some(link),
        Err(e) => {
            tracing::warn!(error = %e, "chat.getPermalink failed; task will have no url");
            None
        }
    };
    let context = thread_context(api, config, names, mention).await;

    let snippet: String = mention
        .text
        .replace('\n', " ")
        .chars()
        .take(TITLE_SNIPPET_CHARS)
        .collect();
    let title = format!("Slack: {sender} in #{channel_name}: {snippet}");

    let mut body = String::from(
        "以下の Slack メンションへの返信案を日本語で作成してください。\
         対象リポジトリを調査し、根拠を持って回答してください。\
         出力は返信文のみとし、前置き・後書き・説明を含めないでください。\n",
    );
    if let Some(style) = &config.reply_style {
        body.push_str(&format!("返信スタイル: {style}\n"));
    }
    body.push_str(&format!(
        "\n## メンション\n\n- 送信者: {sender}\n- チャンネル: #{channel_name}\n- 本文:\n\n> {}\n",
        mention.text.replace('\n', "\n> ")
    ));
    match &context {
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

    // Until repo resolution (#106) lands, a hint is only possible when there
    // is exactly one candidate.
    let repo_hint = match config.repos.as_slice() {
        [only] => Some(only.name.clone()),
        _ => None,
    };

    let task = Task {
        id: mention.task_id(),
        source: config.source_name.clone(),
        title,
        body: Some(body),
        repo_hint,
        labels: Vec::new(),
        priority: 0,
        status: None,
        url: permalink.clone(),
        assignee: None,
    };
    let pending = PendingMention {
        channel: mention.channel.clone(),
        reply_ts: mention.reply_ts().to_string(),
        mention_ts: mention.ts.clone(),
        sender_name: sender,
        permalink,
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
