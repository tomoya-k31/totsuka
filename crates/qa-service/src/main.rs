use std::sync::Arc;

use qa_service::adapter_client::{AdapterClient, HyperlocalAdapter};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::catchup::run_catchup_once;
use qa_service::classifier::{self, ClassifyRequest, RepoCandidate};
use qa_service::gh_inbox::GhInboxClient;
use qa_service::lifecycle::{probe_adapter, probe_db, probe_repo_descriptions, wait_for_signals};
use qa_service::listener::{bind_uds, resolve_uds_path, serve_uds};
use qa_service::mode::AnswerMode;
use qa_service::question_filter::{QuestionFilter, Trigger};
use qa_service::reaction::{handle_reaction, ReactionCtx};
use qa_service::recovery::reconcile;
use qa_service::repo_select::{RepoSelector, SelectOutcome};
use qa_service::schema_check::check_schema_version;
use qa_service::slack::{
    envelope::SlackEvent,
    socket::{run_socket_loop, SocketModeConfig},
    HttpSlackClient, SlackClient,
};
use qa_service::sweeper::run_sweeper;
use qa_service::thread_map::ThreadMapRepo;
use qa_service::QaApp;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + tracing
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);
    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard =
        totsuka_telemetry::init_tracing(&state_dir, "qa-service", &config.totsuka.log_level);

    // 2. DB + schema
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            config.postgres.user,
            config.postgres.password.expose(),
            config.postgres.host,
            config.postgres.port,
            config.postgres.database,
        )
    });
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await?;
    check_schema_version(&pool).await?;

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    // 3. Adapter + Slack + Classifier + Inbox + RepoSelector
    let adapter_path = resolve_uds_path(&config.qa_service.adapter_uds);
    let adapter: Arc<dyn AdapterClient> = Arc::new(HyperlocalAdapter::new(adapter_path));

    let slack: Arc<dyn SlackClient> = Arc::new(HttpSlackClient::new(
        config.qa_service.slack_bot_token.clone(),
        None,
    ));

    let classifier_arc = classifier::build(&config.qa_service.classifier)?;

    let inbox = Arc::new(GhInboxClient::new(
        config.github_watcher.github_token.clone(),
        None,
    ));

    let selector = Arc::new(RepoSelector::from_cfg(
        config.qa_service.classifier.confidence_threshold,
        &config.qa_service.classifier.on_low_confidence,
    )?);

    let default_mode = AnswerMode::parse(&config.qa_service.default_mode)?;

    // 4. Probes + ready
    let health = HealthState::new();
    probe_db(&pool, &health).await;
    probe_adapter(adapter.as_ref(), &health).await;
    probe_repo_descriptions(&config, &health).await;
    health.set_ready(true).await;

    // 5. Recovery
    let _report = reconcile(thread_map.as_ref(), adapter.as_ref()).await?;

    // 6. Catchup (best-effort)
    if !config.qa_service.catchup_channels.is_empty() {
        let _ = run_catchup_once(
            slack.as_ref(),
            &pool,
            &config.qa_service.catchup_channels,
            None,
        )
        .await;
    }

    // 7. Socket Mode loop → mpsc → dispatch worker
    let (event_tx, mut event_rx) = mpsc::channel::<SlackEvent>(128);
    let shutdown = CancellationToken::new();

    let socket_h = {
        let cfg = SocketModeConfig::new(config.qa_service.slack_app_token.clone());
        let http = Arc::new(
            reqwest::Client::builder()
                .user_agent("totsuka-qa-service")
                .build()?,
        );
        let s = shutdown.clone();
        tokio::spawn(async move { run_socket_loop(cfg, http, event_tx, s).await })
    };

    let semaphore = Arc::new(Semaphore::new(
        config.qa_service.answer.max_concurrent_answers as usize,
    ));

    let project_node_id = {
        let token = config.github_watcher.github_token.clone();
        let owner = config.github.project_owner.clone();
        let number = config.github.project_number;
        let http = reqwest::Client::builder()
            .user_agent("totsuka-qa-service")
            .build()?;
        let body = serde_json::json!({
            "query": "query($login:String!,$number:Int!){user(login:$login){projectV2(number:$number){id}}}",
            "variables": { "login": owner, "number": number },
        });
        let v: serde_json::Value = http
            .post("https://api.github.com/graphql")
            .bearer_auth(token.expose())
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        v.pointer("/data/user/projectV2/id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("project node id resolve failed"))?
            .to_string()
    };

    let dispatch_h = {
        let adapter = adapter.clone();
        let slack = slack.clone();
        let classifier_arc = classifier_arc.clone();
        let selector = selector.clone();
        let inbox = inbox.clone();
        let thread_map = thread_map.clone();
        let clock = clock.clone();
        let config = config.clone();
        let semaphore = semaphore.clone();
        let project_node_id = project_node_id.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            let filter = QuestionFilter::new(
                config.qa_service.allowed_user_ids.clone(),
                std::env::var("SLACK_BOT_USER_ID").unwrap_or_default(),
            );
            // The `[agent_adapter.repos.HASH_KEY]` map key IS the `owner/repo`
            // string used by both the classifier and the adapter's spawn call —
            // NOT `RepoSection.repo_path` (which is a local filesystem path).
            let candidates: Vec<RepoCandidate> = config
                .agent_adapter
                .repos
                .iter()
                .map(|(owner_repo, r)| RepoCandidate {
                    repo: owner_repo.clone(),
                    description: r.description.clone(),
                })
                .collect();
            let answer_ctx = AnswerCtx {
                adapter: adapter.clone(),
                slack: slack.clone(),
                thread_map: thread_map.clone(),
                clock: clock.clone(),
                answer_cfg: config.qa_service.answer.clone(),
                system_prompt_template:
                    "Answer the user question. Wrap your answer in {open_tag}…{close_tag} and end \
                     with {sentinel}. Use Slack mrkdwn formatting (*bold*, _italic_, ```code```)."
                        .to_string(),
            };
            let reaction_ctx = ReactionCtx {
                slack: slack.clone(),
                inbox: inbox.clone(),
                project_node_id,
                trigger_emoji: config.qa_service.reaction_trigger.clone(),
            };
            loop {
                tokio::select! {
                    _ = s.cancelled() => break,
                    Some(ev) = event_rx.recv() => {
                        match ev {
                            SlackEvent::Message(m) => {
                                let thread_key = m.thread_ts.clone().unwrap_or_else(|| m.ts.clone());
                                let existing = thread_map.get(&thread_key).await.unwrap_or(None).is_some();
                                let trig = filter.evaluate(&m, existing);
                                if trig == Trigger::None { continue; }
                                let req = ClassifyRequest {
                                    question: m.text.clone(),
                                    thread_context: None,
                                    candidates: candidates.clone(),
                                };
                                let resp = match classifier_arc.classify(req).await {
                                    Ok(r) => r,
                                    Err(e) => { tracing::warn!(error=%e, "classify failed"); continue; }
                                };
                                let outcome = selector.decide(&resp);
                                let (repo, mode) = match outcome {
                                    SelectOutcome::HighConfidence { repo, .. } => (repo, default_mode),
                                    SelectOutcome::LowConfidenceUseTop1 { repo, .. } => (repo, default_mode),
                                    SelectOutcome::LowConfidenceDelegated { .. }
                                    | SelectOutcome::LowConfidenceRefused => {
                                        let _ = slack.post_ephemeral(
                                            &m.channel, &m.user, Some(&thread_key),
                                            "リポジトリを特定できませんでした。明示的に指定してください。",
                                        ).await;
                                        continue;
                                    }
                                };
                                let permit = semaphore.clone().acquire_owned().await.expect("permit");
                                let input = AnswerInput {
                                    channel: m.channel.clone(),
                                    user: m.user.clone(),
                                    thread_ts: thread_key,
                                    question: m.text.clone(),
                                    repo,
                                    mode,
                                };
                                let ctx_cloned = answer_ctx.clone();
                                tokio::spawn(async move {
                                    let _p = permit;
                                    if let Err(e) = handle_answer(&ctx_cloned, input).await {
                                        tracing::warn!(error=%e, "answer pipeline failed");
                                    }
                                });
                            }
                            SlackEvent::ReactionAdded { channel, item_ts, reaction, .. } => {
                                if let Err(e) =
                                    handle_reaction(&reaction_ctx, &channel, &item_ts, &reaction).await
                                {
                                    tracing::warn!(error=%e, "reaction handler failed");
                                }
                            }
                            SlackEvent::Other => {}
                        }
                    }
                }
            }
            Ok::<(), qa_service::error::QaError>(())
        })
    };

    let sweeper_h = {
        let adapter = adapter.clone();
        let thread_map = thread_map.clone();
        let clock = clock.clone();
        let ttl = chrono::Duration::seconds(config.qa_service.answer.pane_idle_ttl_secs as i64);
        let s = shutdown.clone();
        tokio::spawn(async move { run_sweeper(thread_map, adapter, clock, ttl, 60, s).await })
    };

    let listener_h = {
        let uds = resolve_uds_path(&config.qa_service.uds_path);
        let listener = bind_uds(&uds).await?;
        let router = totsuka_telemetry::http::router(health.clone()).layer(
            axum::middleware::from_fn(totsuka_telemetry::request_id::middleware),
        );
        tokio::spawn(async move { serve_uds(listener, router).await })
    };

    let _signals = tokio::spawn(wait_for_signals(shutdown.clone()));
    let _app = QaApp::new(config.clone(), clock.clone());

    tokio::select! {
        r = socket_h   => { let _ = r?; },
        r = dispatch_h => { let _ = r?; },
        r = sweeper_h  => { let _ = r?; },
        r = listener_h => { let _ = r?; },
    }
    Ok(())
}
