use qa_service::answer::{extract, AnswerExtraction, ExtractConfig};

fn cfg<'a>() -> ExtractConfig<'a> {
    ExtractConfig {
        sentinel: "<<TOTSUKA_DONE>>",
        open_tag: "<answer>",
        close_tag: "</answer>",
        max_chars: 100,
        fallback_tail_lines: 3,
    }
}

#[test]
fn extracts_tag_delimited_content() {
    let snap = "noise\n<answer>here is the answer</answer>\n<<TOTSUKA_DONE>>\ntail";
    assert_eq!(
        extract(snap, &cfg()),
        AnswerExtraction::TagDelimited("here is the answer".into())
    );
}

#[test]
fn falls_back_to_tail_when_no_tags() {
    let snap = "noise\nline-a\nline-b\nline-c\n<<TOTSUKA_DONE>>\nignored";
    match extract(snap, &cfg()) {
        AnswerExtraction::FallbackTail(s) => {
            assert!(s.contains("line-a") || s.contains("line-b") || s.contains("line-c"));
            assert!(!s.contains("ignored"));
        }
        other => panic!("expected FallbackTail, got {other:?}"),
    }
}

#[test]
fn truncates_long_answer_at_max_chars() {
    let body = "x".repeat(500);
    let snap = format!("<answer>{body}</answer><<TOTSUKA_DONE>>");
    match extract(&snap, &cfg()) {
        AnswerExtraction::TagDelimited(s) => assert_eq!(s.chars().count(), 100),
        other => panic!("expected TagDelimited, got {other:?}"),
    }
}

#[test]
fn returns_empty_for_empty_snapshot() {
    assert_eq!(extract("", &cfg()), AnswerExtraction::Empty);
}

#[test]
fn returns_empty_when_no_lines_and_no_tags() {
    assert_eq!(extract("\n\n\n", &cfg()), AnswerExtraction::Empty);
}

#[test]
fn utf8_safe_truncate() {
    let body = "あ".repeat(200); // 200 chars, 600 bytes
    let snap = format!("<answer>{body}</answer><<TOTSUKA_DONE>>");
    match extract(&snap, &cfg()) {
        AnswerExtraction::TagDelimited(s) => assert_eq!(s.chars().count(), 100),
        other => panic!("got {other:?}"),
    }
}
