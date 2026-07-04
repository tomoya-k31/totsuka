//! Answer flow: spawn-or-reuse agent → poll snapshot → extract → post.
//! Concurrency-gated outside this module (Semaphore in the dispatch loop).

use std::sync::Arc;
use std::time::{Duration, Instant};
use totsuka_config::schema::AnswerSection;
use totsuka_core::Clock;

use super::extract::{extract, AnswerExtraction, ExtractConfig};
use crate::adapter_client::{AdapterClient, SpawnReq};
use crate::error::QaError;
use crate::mode::AnswerMode;
use crate::slack::{SlackClient, SlackPostResult};
use crate::thread_history::ThreadHistoryRepo;
use crate::thread_map::{ThreadMapRepo, ThreadMapping};

/// How many persisted history entries to replay into a fresh agent.
const HISTORY_REPLAY_LIMIT: i64 = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerOutcome {
    Posted { ts: String },
    Truncated { ts: String },
    SpawnFailed(String),
    ExtractFallback { ts: String },
}

pub struct AnswerInput {
    pub channel: String,
    pub user: String,
    pub thread_ts: String,
    pub question: String,
    pub repo: String,
    pub mode: AnswerMode,
}

#[derive(Clone)]
pub struct AnswerCtx {
    pub adapter: Arc<dyn AdapterClient>,
    pub slack: Arc<dyn SlackClient>,
    pub thread_map: Arc<ThreadMapRepo>,
    pub thread_history: Arc<ThreadHistoryRepo>,
    pub clock: Arc<dyn Clock>,
    pub answer_cfg: AnswerSection,
    pub system_prompt_template: String,
}

pub async fn handle_answer(ctx: &AnswerCtx, input: AnswerInput) -> Result<AnswerOutcome, QaError> {
    // 1. Resolve or spawn the agent.
    let existing = ctx.thread_map.get(&input.thread_ts).await?;
    let mut resolved: Option<(String, String)> = None;
    if let Some(m) = existing {
        // The reused pane still shows the previous turn's answer
        // (including its sentinel). Capture a baseline BEFORE sending so
        // the poll below can tell a fresh answer from the stale one.
        let baseline = ctx
            .adapter
            .read(&m.terminal_id, 0)
            .await
            .map(|s| s.text)
            .unwrap_or_default();
        match ctx.adapter.send(&m.terminal_id, &input.question).await {
            Ok(()) => {
                ctx.thread_map.touch(&input.thread_ts).await?;
                resolved = Some((m.terminal_id, baseline));
            }
            Err(e) => {
                // Self-heal only when the terminal is definitively gone
                // (herdr restart, manual pane close, sweeper race). On
                // transient errors keep the mapping — the pane may be alive
                // and holds the thread's conversation context.
                if !crate::adapter_client::is_agent_gone(&e) {
                    return Err(e);
                }
                tracing::warn!(error=%e, thread_ts=%input.thread_ts,
                    terminal_id=%m.terminal_id,
                    "mapped terminal is gone; respawning a fresh agent");
                ctx.thread_map.delete(&input.thread_ts).await?;
            }
        }
    }
    let (agent_id, baseline) = match resolved {
        Some(v) => v,
        None => {
            // herdr executes argv[0] as the program. Instructions ride
            // --append-system-prompt and the question is the positional
            // prompt: claude starts consuming its initial prompt immediately,
            // so a post-spawn send() races the boot and loses the question.
            let mut argv = ctx.answer_cfg.claude_argv.clone();
            argv.push("--append-system-prompt".into());
            argv.push(interpolate_prompt(
                &ctx.system_prompt_template,
                &ctx.answer_cfg,
            ));
            // A fresh agent for a thread whose pane was already swept knows
            // nothing — replay the persisted conversation so the thread
            // continues where it left off (best-effort: an empty/missing
            // history degrades to a plain first question).
            let history = ctx
                .thread_history
                .recent(&input.thread_ts, HISTORY_REPLAY_LIMIT)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error=%e, thread_ts=%input.thread_ts,
                        "history fetch failed; spawning without context");
                    Vec::new()
                });
            let prompt = if history.is_empty() {
                input.question.clone()
            } else {
                let mut p = String::from(
                    "以下はこのスレッドでのこれまでの会話履歴です。\
                     文脈として踏まえて回答してください。\n",
                );
                for h in &history {
                    p.push_str(&format!("[{}] {}\n", h.role, h.body));
                }
                p.push_str("\n新しい質問: ");
                p.push_str(&input.question);
                p
            };
            argv.push(prompt);
            let req = SpawnReq {
                task_id: format!("qa-{}", &input.thread_ts),
                phase: "answer".into(),
                attempt: 0,
                repo: input.repo.clone(),
                branch: format!("qa/{}", sanitize_branch(&input.thread_ts)),
                argv,
                env: Default::default(),
                detached: true,
            };
            let res = match ctx.adapter.spawn(req).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error=%e, thread_ts=%input.thread_ts, repo=%input.repo,
                        "qa agent spawn failed");
                    if let Err(pe) = ctx
                        .slack
                        .post_ephemeral(
                            &input.channel,
                            &input.user,
                            Some(&input.thread_ts),
                            "エージェントの起動に失敗しました。ログを確認してください。",
                        )
                        .await
                    {
                        tracing::warn!(error=%pe, "failed to post spawn-failure notice");
                    }
                    return Ok(AnswerOutcome::SpawnFailed(e.to_string()));
                }
            };
            // Question already rides the spawn argv — no post-spawn send.
            // Fresh pane: nothing stale to guard against.
            let baseline = String::new();
            let now = ctx.clock.now();
            ctx.thread_map
                .upsert(&ThreadMapping {
                    thread_ts: input.thread_ts.clone(),
                    terminal_id: res.terminal_id.clone(),
                    repo: input.repo.clone(),
                    last_activity_at: now,
                    created_at: now,
                })
                .await?;
            (res.terminal_id, baseline)
        }
    };

    // 2. Poll for output until sentinel / quiescence / timeout.
    let cfg = &ctx.answer_cfg;
    let extract_cfg = ExtractConfig {
        sentinel: &cfg.sentinel,
        open_tag: &cfg.answer_open_tag,
        close_tag: &cfg.answer_close_tag,
        max_chars: 40_000,
        fallback_tail_lines: 40,
    };
    let tag_of = |snap: &str| match extract(snap, &extract_cfg) {
        AnswerExtraction::TagDelimited(s) => Some(s),
        _ => None,
    };
    // On a reused pane the previous turn's answer (and sentinel) is still
    // visible; completion means a tag block DIFFERENT from this one.
    let baseline_tag = tag_of(&baseline);
    let mut last_change = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(cfg.answer_timeout_secs);
    let stable = Duration::from_secs(cfg.stable_revision_secs);
    let mut latest_snapshot = String::new();
    let mut hit_timeout = false;
    loop {
        if Instant::now() >= deadline {
            hit_timeout = true;
            break;
        }
        let snap = ctx.adapter.read(&agent_id, 0).await?;
        // Change-detect on pane text, not snap.revision: real herdr returns
        // revision 0 on every `visible` read, so a revision-keyed update
        // never fires and the answer is silently dropped.
        if snap.text != latest_snapshot {
            last_change = Instant::now();
            latest_snapshot = snap.text;
        }
        let tag = tag_of(&latest_snapshot);
        let stale = baseline_tag.is_some() && (tag == baseline_tag || latest_snapshot == baseline);
        if latest_snapshot.contains(&cfg.sentinel) && !stale {
            // Reused pane: the old sentinel is always visible, so require a
            // complete NEW tag block before trusting the sentinel check.
            if baseline_tag.is_none() || tag.is_some() {
                break;
            }
        }
        if last_change.elapsed() >= stable && !latest_snapshot.is_empty() && !stale {
            break;
        }
        tokio::time::sleep(Duration::from_millis(cfg.poll_interval_ms)).await;
    }

    // 3. Extract.
    let mut extraction = extract(&latest_snapshot, &extract_cfg);
    if baseline_tag.is_some() {
        if let AnswerExtraction::TagDelimited(ref s) = extraction {
            if Some(s) == baseline_tag.as_ref() {
                tracing::warn!(thread_ts = %input.thread_ts,
                    "pane produced no new answer; refusing to repost the previous turn's");
                extraction = AnswerExtraction::Empty;
            }
        }
    }
    let (text, kind) = match extraction {
        AnswerExtraction::TagDelimited(s) => (s, "tag"),
        AnswerExtraction::FallbackTail(s) => {
            tracing::warn!(
                thread_ts = %input.thread_ts,
                "answer tag missing; posting fallback tail"
            );
            (s, "fallback")
        }
        AnswerExtraction::Empty => {
            tracing::warn!(thread_ts = %input.thread_ts, "no answer text extracted");
            (String::from("(no answer produced)"), "empty")
        }
    };

    // 4. Post.
    let SlackPostResult { ts } = match input.mode {
        AnswerMode::Auto => {
            ctx.slack
                .post_message(&input.channel, Some(&input.thread_ts), &text)
                .await?
        }
        AnswerMode::Delegated => {
            // Address the asker explicitly: ephemeral messages carry no
            // notification badge of their own, so the leading mention is
            // what makes the answer discoverable in a busy thread.
            let mention_text = format!("<@{}> {}", input.user, text);
            ctx.slack
                .post_ephemeral(
                    &input.channel,
                    &input.user,
                    Some(&input.thread_ts),
                    &mention_text,
                )
                .await?;
            SlackPostResult {
                ts: format!("ephemeral-{}", input.thread_ts),
            }
        }
    };

    ctx.thread_map.touch(&input.thread_ts).await?;

    // 5. Persist the exchange (best-effort) so a future respawn can replay
    // it. Only clean tag-delimited answers are recorded — fallback tails
    // and "(no answer produced)" would poison the replayed context.
    if let Err(e) = ctx
        .thread_history
        .append(&input.thread_ts, "user", &input.question)
        .await
    {
        tracing::warn!(error=%e, thread_ts=%input.thread_ts, "history append (user) failed");
    }
    if kind == "tag" {
        if let Err(e) = ctx
            .thread_history
            .append(&input.thread_ts, "assistant", &text)
            .await
        {
            tracing::warn!(error=%e, thread_ts=%input.thread_ts, "history append (assistant) failed");
        }
    }

    Ok(match (hit_timeout, kind) {
        (true, _) => AnswerOutcome::Truncated { ts },
        (_, "fallback") => AnswerOutcome::ExtractFallback { ts },
        _ => AnswerOutcome::Posted { ts },
    })
}

fn interpolate_prompt(template: &str, cfg: &AnswerSection) -> String {
    template
        .replace("{sentinel}", &cfg.sentinel)
        .replace("{open_tag}", &cfg.answer_open_tag)
        .replace("{close_tag}", &cfg.answer_close_tag)
}

fn sanitize_branch(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
