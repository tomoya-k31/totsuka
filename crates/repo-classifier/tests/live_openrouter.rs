//! Live checks against OpenRouter's real Decisions API (#723).
//!
//! Ignored by default: they need a real key and cost money (a few hundred
//! input tokens each, ~$0.00002). CI never runs them. Run them when the
//! decisions backend or OpenRouter's alpha endpoint may have changed:
//!
//! ```bash
//! OPENROUTER_API_KEY="$(op read 'op://Dev/Openrouter/api_key')" \
//!   cargo test -p repo-classifier --test live_openrouter -- --ignored --test-threads=1
//! ```
//!
//! They go through [`ReqwestTransport`], so they exercise the exact request and
//! response handling production uses — what the unit tests' canned answers
//! cannot: that OpenRouter still accepts the body, still returns
//! `probabilities`, and still takes repository names verbatim as option keys.

use repo_classifier::{
    ApiKey, Candidate, Classification, ClassifyRequest, DecisionsClassifier, DecisionsSettings,
    RepoClassifier, ReqwestTransport, RetryPolicy,
};

const MODEL: &str = "~typesafe/jev-latest";

fn key() -> ApiKey {
    ApiKey::new(std::env::var("OPENROUTER_API_KEY").expect(
        "set OPENROUTER_API_KEY to run the live checks (they are #[ignore]d for this reason)",
    ))
}

fn classifier(api_key: ApiKey) -> DecisionsClassifier<ReqwestTransport> {
    let mut settings =
        DecisionsSettings::new(DecisionsSettings::OPENROUTER_ENDPOINT, MODEL, api_key);
    settings.retry = RetryPolicy::NONE;
    DecisionsClassifier::new(ReqwestTransport::new(), settings)
}

/// Names with `/`, `.` and `_`, the shapes `[[repositories]].name` takes.
fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            name: "tomoya-k31/totsuka".into(),
            summary: Some(
                "Rust workspace: AI dev-flow orchestrator with Slack / GitHub / Notion task-source \
                 plugins"
                    .into(),
            ),
            readme_head: Some("# totsuka\nDetects task instructions and dispatches them to AI agents.".into()),
        },
        Candidate {
            name: "dotfiles.nvim".into(),
            summary: Some("Personal dotfiles managed with GNU Stow: zsh, mise, neovim".into()),
            readme_head: None,
        },
        Candidate {
            name: "my_org/web.app-v2".into(),
            summary: Some("Next.js marketing website".into()),
            readme_head: None,
        },
    ]
}

fn request(title: &str, body: &str) -> ClassifyRequest {
    ClassifyRequest {
        subject: vec![
            ("Task".into(), title.into()),
            ("Description".into(), body.into()),
        ],
        candidates: candidates(),
        chat_prompt: None,
    }
}

#[tokio::test]
#[ignore = "calls the real OpenRouter Decisions API; needs OPENROUTER_API_KEY"]
async fn a_clear_task_is_classified_into_its_repository() {
    let verdict = classifier(key())
        .classify(&request(
            "Slack reaction trigger should ignore bot posts",
            "The task-source-slack plugin picks up reactions on bot messages; add a from_bot \
             filter in the Rust plugin.",
        ))
        .await
        .expect("the live call succeeds");
    eprintln!("verdict: {verdict:?}");
    match verdict {
        Classification::Repo {
            repo, confidence, ..
        } => {
            assert_eq!(repo, "tomoya-k31/totsuka");
            assert!(confidence >= 0.6, "a clear task, yet p={confidence}");
        }
        other => panic!("expected a repository, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "calls the real OpenRouter Decisions API; needs OPENROUTER_API_KEY"]
async fn an_unrelated_task_is_none_fits() {
    let verdict = classifier(key())
        .classify(&request("Book a dentist appointment for Tuesday", ""))
        .await
        .expect("the live call succeeds");
    eprintln!("verdict: {verdict:?}");
    assert!(
        matches!(verdict, Classification::NoneFits { .. }),
        "{verdict:?}"
    );
}

#[tokio::test]
#[ignore = "calls the real OpenRouter Decisions API; needs OPENROUTER_API_KEY"]
async fn the_probe_passes_with_a_real_key_and_fails_as_auth_with_a_bad_one() {
    classifier(key()).probe().await.expect("the probe succeeds");
    let err = classifier(ApiKey::new("sk-or-v1-not-a-real-key"))
        .probe()
        .await
        .expect_err("a bad key is rejected");
    assert!(err.is_auth_failure(), "{err}");
}
