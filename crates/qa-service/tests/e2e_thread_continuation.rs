use chrono::Utc;
use qa_service::adapter_client::{AdapterClient, MockAdapter, ReadRes};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::mode::AnswerMode;
use qa_service::slack::{MockSlackClient, SlackClient};
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use std::sync::Arc;
use totsuka_config::schema::AnswerSection;
use totsuka_core::SystemClock;

fn answer_cfg() -> AnswerSection {
    AnswerSection {
        claude_argv: vec!["claude".into()],
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
async fn existing_thread_mapping_sends_no_spawn() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    // Reused pane: first read is the pre-send baseline showing the PREVIOUS
    // turn's answer; later reads show the pane after the new answer landed.
    adapter.set_read_sequence(vec![
        ReadRes {
            revision: 0,
            text: "<answer>previous turn</answer><<TOTSUKA_DONE>>".into(),
            is_newer: false,
        },
        ReadRes {
            revision: 0,
            text: "<answer>previous turn</answer><<TOTSUKA_DONE>>\n\
                   ❯ follow-up question\n\
                   <answer>follow-up</answer><<TOTSUKA_DONE>>"
                .into(),
            is_newer: false,
        },
    ]);

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));

    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();

    let now = Utc::now();
    thread_map
        .upsert(&ThreadMapping {
            thread_ts: thread_ts.clone(),
            terminal_id: "term_existing".into(),
            repo: "acme/api".into(),
            last_activity_at: now,
            created_at: now,
        })
        .await
        .unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "follow-up question".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
    };
    let _ = handle_answer(&ctx, input).await.unwrap();

    assert!(
        adapter.expected_spawns().is_empty(),
        "must NOT spawn on existing mapping"
    );
    let sends = adapter.expected_sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, "term_existing");
    assert_eq!(sends[0].1, "follow-up question");

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "follow-up");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn continuation_never_reposts_previous_turns_answer() {
    // Regression: the pane keeps the previous turn's <answer>+sentinel
    // visible. If the new question never executes (e.g. input not
    // submitted), the poll must NOT re-extract and repost the old answer.
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    // Pane never changes: every read (baseline included) shows only the
    // previous turn's answer.
    adapter.set_read_response(ReadRes {
        revision: 0,
        text: "<answer>previous turn</answer><<TOTSUKA_DONE>>".into(),
        is_newer: false,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let now = Utc::now();
    thread_map
        .upsert(&ThreadMapping {
            thread_ts: thread_ts.clone(),
            terminal_id: "term_existing".into(),
            repo: "acme/api".into(),
            last_activity_at: now,
            created_at: now,
        })
        .await
        .unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "second question".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
    };
    let _ = handle_answer(&ctx, input).await.unwrap();

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_ne!(
        posts[0].2, "previous turn",
        "stale answer must not be reposted"
    );
    assert_eq!(posts[0].2, "(no answer produced)");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}
