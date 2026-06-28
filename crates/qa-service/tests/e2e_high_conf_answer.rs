use qa_service::adapter_client::{AdapterClient, AgentSummary, MockAdapter, ReadRes, SpawnRes};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::mode::AnswerMode;
use qa_service::slack::{MockSlackClient, SlackClient};
use qa_service::thread_map::ThreadMapRepo;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_config::schema::AnswerSection;
use totsuka_core::SystemClock;

fn answer_cfg() -> AnswerSection {
    AnswerSection {
        sentinel: "<<TOTSUKA_DONE>>".into(),
        answer_open_tag: "<answer>".into(),
        answer_close_tag: "</answer>".into(),
        poll_interval_ms: 20,
        stable_revision_secs: 1,
        answer_timeout_secs: 5,
        pane_idle_ttl_secs: 1800,
        max_concurrent_answers: 4,
    }
}

#[tokio::test]
async fn high_conf_answer_spawns_polls_extracts_posts() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
    let pool = PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_1".into(),
        terminal_id: "term_e2e_1".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });
    adapter.set_list_response(vec![AgentSummary {
        agent_id: "agent_e2e_1".into(),
        terminal_id: "term_e2e_1".into(),
        label: "totsuka:qa-1:answer:0".into(),
    }]);

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    // Clean any prior state for this thread.
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }));

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "OK");                            // text
    assert_eq!(posts[0].1.as_deref(), Some(thread_ts.as_str())); // thread_ts

    let mapping = thread_map.get(&thread_ts).await.unwrap().unwrap();
    assert_eq!(mapping.terminal_id, "term_e2e_1");
    assert_eq!(mapping.repo, "acme/api");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts).execute(&pool).await.unwrap();
}
