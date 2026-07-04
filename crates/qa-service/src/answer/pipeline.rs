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
use crate::thread_map::{ThreadMapRepo, ThreadMapping};

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
    pub clock: Arc<dyn Clock>,
    pub answer_cfg: AnswerSection,
    pub system_prompt_template: String,
}

pub async fn handle_answer(ctx: &AnswerCtx, input: AnswerInput) -> Result<AnswerOutcome, QaError> {
    // 1. Resolve or spawn the agent.
    let existing = ctx.thread_map.get(&input.thread_ts).await?;
    let agent_id = match existing {
        Some(m) => {
            // Send the new message to the existing agent.
            ctx.adapter.send(&m.terminal_id, &input.question).await?;
            ctx.thread_map.touch(&input.thread_ts).await?;
            m.terminal_id
        }
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
            argv.push(input.question.clone());
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
            res.terminal_id
        }
    };

    // 2. Poll for output until sentinel / quiescence / timeout.
    let cfg = &ctx.answer_cfg;
    let mut prev_revision: u64 = 0;
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
        if snap.revision != prev_revision {
            prev_revision = snap.revision;
            last_change = Instant::now();
            latest_snapshot = snap.text.clone();
        }
        if latest_snapshot.contains(&cfg.sentinel) {
            break;
        }
        if last_change.elapsed() >= stable && !latest_snapshot.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(cfg.poll_interval_ms)).await;
    }

    // 3. Extract.
    let extract_cfg = ExtractConfig {
        sentinel: &cfg.sentinel,
        open_tag: &cfg.answer_open_tag,
        close_tag: &cfg.answer_close_tag,
        max_chars: 40_000,
        fallback_tail_lines: 40,
    };
    let extraction = extract(&latest_snapshot, &extract_cfg);
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
