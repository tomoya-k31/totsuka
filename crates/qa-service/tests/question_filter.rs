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
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_OTHER", "<@U_BOT> hi", None), false),
        Trigger::None
    );
}

#[test]
fn detects_mention_on_top_level_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "<@U_BOT> hi", None), false),
        Trigger::Mention
    );
}

#[test]
fn detects_thread_continuation_only_with_existing_mapping() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
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
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(
            &msg(
                "U_ALLOWED",
                "<@U_BOT> in-thread",
                Some("17500000001.000100")
            ),
            true
        ),
        Trigger::Mention,
    );
}

#[test]
fn top_level_no_mention_returns_none() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "hi", None), false),
        Trigger::None
    );
}

#[test]
fn self_mention_fires_for_non_allowed_author() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    // 同僚(allowed 外)が自分をメンション → 発火
    assert_eq!(
        f.evaluate(
            &msg("U_COLLEAGUE", "<@U_ME> これどうなってる?", None),
            false
        ),
        Trigger::SelfMention
    );
}

#[test]
fn self_mention_does_not_fire_for_own_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f.evaluate(&msg("U_ME", "<@U_ME> メモ", None), false),
        Trigger::None
    );
}

#[test]
fn bot_mention_takes_precedence_over_self_mention() {
    // allowed ユーザーが bot と自分の両方をメンション → 既存フロー優先
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ALLOWED".into());
    assert_eq!(
        f.evaluate(&msg("U_OTHER", "<@U_BOT> <@U_ALLOWED> hi", None), false),
        Trigger::SelfMention,
        "bot メンションでも author が allowed 外なら Mention にはならず SelfMention"
    );
    let f2 = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f2.evaluate(&msg("U_ALLOWED", "<@U_BOT> <@U_ME> hi", None), false),
        Trigger::Mention
    );
}

#[test]
fn empty_self_mention_id_disables_feature() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_COLLEAGUE", "hi <@> there", None), false),
        Trigger::None
    );
}
