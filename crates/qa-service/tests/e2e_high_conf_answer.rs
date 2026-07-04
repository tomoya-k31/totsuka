use qa_service::adapter_client::{AdapterClient, AgentSummary, MockAdapter, ReadRes, SpawnRes};
use qa_service::answer::pipeline::{handle_answer, AnswerCtx, AnswerInput};
use qa_service::mode::AnswerMode;
use qa_service::slack::{MockSlackClient, SlackClient};
use qa_service::thread_history::ThreadHistoryRepo;
use qa_service::thread_map::ThreadMapRepo;
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
        dm_copy_enabled: true,
    }
}

#[tokio::test]
async fn high_conf_answer_spawns_polls_extracts_posts() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
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
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));

    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    // Clean any prior state for this thread.
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
        dm_only: false,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }
    ));

    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "OK"); // text
    assert_eq!(posts[0].1.as_deref(), Some(thread_ts.as_str())); // thread_ts

    let mapping = thread_map.get(&thread_ts).await.unwrap().unwrap();
    assert_eq!(mapping.terminal_id, "term_e2e_1");
    assert_eq!(mapping.repo, "acme/api");

    // herdr executes argv[0] as the program. The system instructions ride
    // --append-system-prompt and the QUESTION is the final positional arg:
    // typing it in post-spawn (adapter.send) races claude's boot and loses
    // the question.
    let spawns = adapter.expected_spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].argv[0], "claude");
    let sys_pos = spawns[0]
        .argv
        .iter()
        .position(|a| a == "--append-system-prompt")
        .expect("system prompt flag present");
    assert!(spawns[0].argv[sys_pos + 1].contains("answer with"));
    assert_eq!(spawns[0].argv.last().unwrap(), "where is auth?");
    assert!(
        adapter.expected_sends().is_empty(),
        "fresh spawn must not race a send"
    );

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn answer_extracted_even_when_revision_stays_zero() {
    // Real herdr returns revision:0 on every `visible` read — change
    // detection must key on pane text, not on the revision counter.
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_r0".into(),
        terminal_id: "term_e2e_r0".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 0,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: false,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
        dm_only: false,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }
    ));
    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "OK");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn delegated_answer_is_ephemeral_with_mention() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_d1".into(),
        terminal_id: "term_e2e_d1".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    handle_answer(&ctx, input).await.unwrap();

    // Delegated answers arrive as an in-thread ephemeral addressed to the
    // asker via a leading mention.
    let ephemerals = slack.ephemerals();
    assert_eq!(ephemerals.len(), 1);
    assert_eq!(ephemerals[0].0, "C1");
    assert_eq!(ephemerals[0].1, "U1");
    assert_eq!(ephemerals[0].2.as_deref(), Some(thread_ts.as_str()));
    assert_eq!(ephemerals[0].3, "<@U1> OK");
    // 公開チャンネルへの投稿はゼロ。唯一の post_message は DM コピー。
    let posts = slack.posts();
    assert_eq!(posts.len(), 1, "expected exactly the DM copy");
    assert_eq!(posts[0].0, "D_U1"); // MockSlackClient::open_dm("U1")
    assert_eq!(posts[0].1, None);
    assert!(posts[0].2.contains("where is auth?"), "got: {}", posts[0].2);
    assert!(
        posts[0].2.contains(&format!(
            "https://mock.slack/archives/C1/p{}",
            thread_ts.replace('.', "")
        )),
        "got: {}",
        posts[0].2
    );
    assert!(posts[0].2.ends_with("OK"), "got: {}", posts[0].2);

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn spawn_failure_notifies_user_ephemerally() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    // No spawn_response set → MockAdapter::spawn errors, like a dead herdr.
    let adapter = Arc::new(MockAdapter::new());
    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::SpawnFailed(_)
    ));

    // The failure must be surfaced to the asking user, not swallowed.
    let ephemerals = slack.ephemerals();
    assert_eq!(ephemerals.len(), 1);
    assert_eq!(ephemerals[0].1, "U1");
    assert!(ephemerals[0].3.contains("起動に失敗"));

    // No mapping row for a failed spawn.
    assert!(thread_map.get(&thread_ts).await.unwrap().is_none());
}

#[tokio::test]
async fn fresh_spawn_replays_persisted_history_and_records_new_exchange() {
    // A thread whose pane was swept respawns with NO agent memory — the
    // persisted history must ride the initial prompt, and the new Q&A must
    // be appended for the next respawn.
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_hist".into(),
        terminal_id: "term_hist".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 0,
        text: "<answer>A2</answer><<TOTSUKA_DONE>>".into(),
        is_newer: false,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    // Prior conversation, persisted before the pane was swept.
    thread_history
        .append(&thread_ts, "user", "Q1")
        .await
        .unwrap();
    thread_history
        .append(&thread_ts, "assistant", "A1")
        .await
        .unwrap();

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "Q2".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Auto,
        dm_only: false,
    };
    handle_answer(&ctx, input).await.unwrap();

    // The initial prompt (last argv element) replays the conversation and
    // ends with the new question.
    let spawns = adapter.expected_spawns();
    assert_eq!(spawns.len(), 1);
    let prompt = spawns[0].argv.last().unwrap();
    assert!(prompt.contains("[user] Q1"), "got: {prompt}");
    assert!(prompt.contains("[assistant] A1"), "got: {prompt}");
    assert!(prompt.contains("新しい質問: Q2"), "got: {prompt}");

    // The new exchange is persisted for the NEXT respawn.
    let hist = thread_history.recent(&thread_ts, 10).await.unwrap();
    let flat: Vec<(String, String)> = hist.into_iter().map(|h| (h.role, h.body)).collect();
    assert_eq!(
        flat,
        vec![
            ("user".into(), "Q1".into()),
            ("assistant".into(), "A1".into()),
            ("user".into(), "Q2".into()),
            ("assistant".into(), "A2".into()),
        ]
    );

    sqlx::query("DELETE FROM qa_thread_history WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn delegated_dm_copy_disabled_skips_dm() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_d2".into(),
        terminal_id: "term_e2e_d2".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let mut cfg = answer_cfg();
    cfg.dm_copy_enabled = false;
    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: cfg,
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    handle_answer(&ctx, input).await.unwrap();

    assert_eq!(slack.ephemerals().len(), 1);
    assert!(slack.posts().is_empty(), "flag off must suppress the DM");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn delegated_dm_failure_keeps_answer_flow_intact() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_d3".into(),
        terminal_id: "term_e2e_d3".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    slack.set_fail_open_dm(true); // im:write 未付与相当
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U1".into(),
        author: "U1".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();

    // DM 失敗は best-effort — 回答は成功扱いでエフェメラルは届いている。
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }
    ));
    assert_eq!(slack.ephemerals().len(), 1);
    assert!(slack.posts().is_empty());

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn self_mention_style_answer_targets_recipient_not_author() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_sm1".into(),
        terminal_id: "term_e2e_sm1".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U_ME".into(),          // recipient = 自分
        author: "U_COLLEAGUE".into(), // 質問者 = 同僚
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    handle_answer(&ctx, input).await.unwrap();

    // エフェメラルは自分宛・from 句付き。
    let ephemerals = slack.ephemerals();
    assert_eq!(ephemerals.len(), 1);
    assert_eq!(ephemerals[0].1, "U_ME");
    assert!(
        ephemerals[0].3.contains("<@U_COLLEAGUE> からの質問"),
        "got: {}",
        ephemerals[0].3
    );
    // DM も自分宛(D_U_ME)・from 句付き。
    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, "D_U_ME");
    assert!(
        posts[0].2.contains("(from <@U_COLLEAGUE>)"),
        "got: {}",
        posts[0].2
    );

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn dm_only_skips_ephemeral_and_sends_dm_even_when_copy_disabled() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_sm2".into(),
        terminal_id: "term_e2e_sm2".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let mut cfg = answer_cfg();
    cfg.dm_copy_enabled = false; // dm_only は flag に優先して DM を送る
    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: cfg,
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C_PRIVATE".into(),
        user: "U_ME".into(),
        author: "U_COLLEAGUE".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: true,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }
    ));

    assert!(slack.ephemerals().is_empty(), "dm_only must skip ephemeral");
    let posts = slack.posts();
    assert_eq!(posts.len(), 1, "DM is the sole answer channel");
    assert_eq!(posts[0].0, "D_U_ME");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}
