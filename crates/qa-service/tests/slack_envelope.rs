use qa_service::slack::envelope::{parse, SlackEnvelope, SlackEvent};

#[test]
fn parses_hello() {
    let r = parse(r#"{"type":"hello","num_connections":1}"#).unwrap();
    assert_eq!(r, SlackEnvelope::Hello);
}

#[test]
fn parses_disconnect_with_reason() {
    let r = parse(r#"{"type":"disconnect","reason":"warning"}"#).unwrap();
    match r {
        SlackEnvelope::Disconnect { reason } => assert_eq!(reason, "warning"),
        _ => panic!(),
    }
}

#[test]
fn parses_events_api_message() {
    let raw = r#"{"type":"events_api","envelope_id":"env-1","payload":{
      "event_id":"Ev0001",
      "event":{"type":"message","channel":"C1","user":"U1","text":"hi","ts":"17500000001.000100"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { envelope_id, event: SlackEvent::Message(m) } => {
            assert_eq!(envelope_id, "env-1");
            assert_eq!(m.text, "hi");
            assert_eq!(m.event_id, "Ev0001");
            assert_eq!(m.channel, "C1");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_reaction_added() {
    let raw = r#"{"type":"events_api","envelope_id":"env-2","payload":{
      "event_id":"Ev0002",
      "event":{"type":"reaction_added","user":"U1","reaction":"memo",
               "item":{"type":"message","channel":"C1","ts":"17500000001.000100"},
               "event_ts":"17500000003.000100"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event: SlackEvent::ReactionAdded { reaction, channel, item_ts, .. }, .. } => {
            assert_eq!(reaction, "memo");
            assert_eq!(channel, "C1");
            assert_eq!(item_ts, "17500000001.000100");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ignores_subtype_messages() {
    let raw = r#"{"type":"events_api","envelope_id":"e","payload":{
      "event_id":"Ev","event":{"type":"message","subtype":"message_changed","channel":"C1"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event, .. } => assert_eq!(event, SlackEvent::Other),
        _ => panic!(),
    }
}

#[test]
fn ignores_bot_messages() {
    let raw = r#"{"type":"events_api","envelope_id":"e","payload":{
      "event_id":"Ev","event":{"type":"message","bot_id":"B1","channel":"C1","text":"x","ts":"1"}
    }}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event, .. } => assert_eq!(event, SlackEvent::Other),
        _ => panic!(),
    }
}

#[test]
fn unknown_envelope_type_errors() {
    assert!(parse(r#"{"type":"slash_commands","..":""}"#).is_err());
}
