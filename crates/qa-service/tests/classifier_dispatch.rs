use qa_service::classifier::build;
use qa_service::error::QaError;
use totsuka_config::schema::ClassifierSection;
use totsuka_core::Secret;

fn cfg(provider: &str, api_base: &str) -> ClassifierSection {
    ClassifierSection {
        provider: provider.into(),
        model: "m".into(),
        api_base: api_base.into(),
        api_key: Secret::new("k".into()),
        max_tokens: 256,
        confidence_threshold: 0.7,
        top_candidates: 3,
        on_low_confidence: "delegated_reaction".into(),
        include_thread_context: true,
        request_timeout_secs: 15,
    }
}

#[test]
fn anthropic_builds() {
    let c = build(&cfg("anthropic", "")).unwrap();
    assert_eq!(c.provider(), "anthropic");
}

#[test]
fn openai_builds_with_default_endpoint() {
    let c = build(&cfg("openai", "")).unwrap();
    assert_eq!(c.provider(), "openai");
}

#[test]
fn openrouter_builds_with_default_endpoint() {
    let c = build(&cfg("openrouter", "")).unwrap();
    assert_eq!(c.provider(), "openrouter");
}

#[test]
fn litellm_requires_api_base() {
    match build(&cfg("litellm", "")) {
        Err(QaError::Classifier(s)) => assert!(s.contains("litellm")),
        _ => panic!("expected litellm error"),
    }
    let c = build(&cfg("litellm", "http://localhost:4000")).unwrap();
    assert_eq!(c.provider(), "litellm");
}

#[test]
fn openai_compatible_requires_api_base() {
    match build(&cfg("openai_compatible", "")) {
        Err(QaError::Classifier(s)) => assert!(s.contains("openai_compatible")),
        _ => panic!("expected openai_compatible error"),
    }
}

#[test]
fn unknown_provider_errors() {
    match build(&cfg("does_not_exist", "")) {
        Err(QaError::Classifier(s)) => assert!(s.contains("unknown provider")),
        _ => panic!("expected unknown provider error"),
    }
}
