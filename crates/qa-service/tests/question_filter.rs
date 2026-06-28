use qa_service::question_filter::{QuestionFilter, Trigger};
use qa_service::slack::SlackMessage;

fn msg(user: &str, text: &str, thread_ts: Option<&str>) -> SlackMessage {
    SlackMessage {
        channel: "C1".into(),
        user: user.into(),
        text: text.into(),
        ts: "17500000001.000100".into(),
        thread_ts: thread_ts.map(str::to_string),
        event_id: "Ev1".into(),
    }
}

#[test]
fn rejects_non_allowed_user() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_OTHER", "<@U_BOT> hi", None), false), Trigger::None);
}

#[test]
fn detects_mention_on_top_level_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_ALLOWED", "<@U_BOT> hi", None), false), Trigger::Mention);
}

#[test]
fn detects_thread_continuation_only_with_existing_mapping() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "more", Some("17500000001.000100")), true),
        Trigger::ThreadContinuation,
    );
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "more", Some("17500000001.000100")), false),
        Trigger::None,
    );
}

#[test]
fn mention_takes_precedence_over_thread_continuation() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "<@U_BOT> in-thread", Some("17500000001.000100")), true),
        Trigger::Mention,
    );
}

#[test]
fn top_level_no_mention_returns_none() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into());
    assert_eq!(f.evaluate(&msg("U_ALLOWED", "hi", None), false), Trigger::None);
}
