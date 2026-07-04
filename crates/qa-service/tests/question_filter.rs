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
        f.evaluate(&msg("U_OTHER", "<@U_BOT> hi", None), None),
        Trigger::None
    );
}

#[test]
fn detects_mention_on_top_level_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "<@U_BOT> hi", None), None),
        Trigger::Mention
    );
}

#[test]
fn detects_thread_continuation_only_with_existing_mapping() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(
            &msg("U_ALLOWED", "more", Some("17500000001.000100")),
            Some("owner")
        ),
        Trigger::ThreadContinuation,
    );
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "more", Some("17500000001.000100")), None),
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
            Some("owner")
        ),
        Trigger::Mention,
    );
}

#[test]
fn top_level_no_mention_returns_none() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_ALLOWED", "hi", None), None),
        Trigger::None
    );
}

#[test]
fn self_mention_fires_for_non_allowed_author() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    // 同僚(allowed 外)が自分をメンション → 発火
    assert_eq!(
        f.evaluate(&msg("U_COLLEAGUE", "<@U_ME> これどうなってる?", None), None),
        Trigger::SelfMention
    );
}

#[test]
fn self_mention_does_not_fire_for_own_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f.evaluate(&msg("U_ME", "<@U_ME> メモ", None), None),
        Trigger::None
    );
}

#[test]
fn bot_mention_takes_precedence_over_self_mention() {
    // allowed ユーザーが bot と自分の両方をメンション → 既存フロー優先
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ALLOWED".into());
    assert_eq!(
        f.evaluate(&msg("U_OTHER", "<@U_BOT> <@U_ALLOWED> hi", None), None),
        Trigger::SelfMention,
        "bot メンションでも author が allowed 外なら Mention にはならず SelfMention"
    );
    let f2 = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f2.evaluate(&msg("U_ALLOWED", "<@U_BOT> <@U_ME> hi", None), None),
        Trigger::Mention
    );
}

#[test]
fn empty_self_mention_id_disables_feature() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_COLLEAGUE", "hi <@> there", None), None),
        Trigger::None
    );
}

#[test]
fn no_continuation_for_self_mention_origin_thread() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ALLOWED".into());
    assert_eq!(
        f.evaluate(
            &msg("U_ALLOWED", "見ておきます", Some("17500000001.000100")),
            Some("self_mention")
        ),
        Trigger::None,
    );
}

#[test]
fn self_mention_refires_in_mapped_thread() {
    // 同僚の再メンションは(マッピング有無に関係なく)SelfMention で発火し、
    // handle_answer 側で既存 agent を再利用する。
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f.evaluate(
            &msg(
                "U_COLLEAGUE",
                "<@U_ME> 追加で質問",
                Some("17500000001.000100")
            ),
            Some("self_mention")
        ),
        Trigger::SelfMention,
    );
}

#[test]
fn unknown_origin_fails_closed_no_continuation() {
    // 想定外の origin 値(破損・手動編集・将来の拡張)では継続を許可しない。
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(
            &msg("U_ALLOWED", "more", Some("17500000001.000100")),
            Some("mystery_origin")
        ),
        Trigger::None,
    );
}
