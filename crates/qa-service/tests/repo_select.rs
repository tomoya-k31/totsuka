use qa_service::classifier::{ClassifyResponse, RepoVerdict};
use qa_service::repo_select::{RepoSelector, SelectOutcome};

fn response(verdicts: Vec<RepoVerdict>) -> ClassifyResponse {
    ClassifyResponse {
        top_candidates: verdicts,
        provider: "mock".into(),
        model: "m".into(),
        latency_ms: 1,
    }
}
fn v(repo: &str, confidence: f64) -> RepoVerdict {
    RepoVerdict {
        repo: repo.into(),
        confidence,
        rationale: "".into(),
    }
}

#[test]
fn high_confidence_picks_top1() {
    let sel = RepoSelector::from_cfg(0.70, "refuse").unwrap();
    let r = response(vec![v("acme/api", 0.91), v("acme/web", 0.30)]);
    match sel.decide(&r) {
        SelectOutcome::HighConfidence { repo, verdict } => {
            assert_eq!(repo, "acme/api");
            assert!((verdict.confidence - 0.91).abs() < 1e-9);
        }
        other => panic!("expected HighConfidence, got {other:?}"),
    }
}

#[test]
fn delegated_reaction_returns_candidates() {
    let sel = RepoSelector::from_cfg(0.70, "delegated_reaction").unwrap();
    let r = response(vec![v("acme/api", 0.42), v("acme/web", 0.31)]);
    match sel.decide(&r) {
        SelectOutcome::LowConfidenceDelegated { candidates } => {
            assert_eq!(candidates.len(), 2);
            assert_eq!(candidates[0].repo, "acme/api");
        }
        other => panic!("expected LowConfidenceDelegated, got {other:?}"),
    }
}

#[test]
fn refuse_returns_refused() {
    let sel = RepoSelector::from_cfg(0.70, "refuse").unwrap();
    let r = response(vec![v("acme/api", 0.42)]);
    assert_eq!(sel.decide(&r), SelectOutcome::LowConfidenceRefused);
}

#[test]
fn use_top1_forces_top1_below_threshold() {
    let sel = RepoSelector::from_cfg(0.70, "use_top1").unwrap();
    let r = response(vec![v("acme/api", 0.40)]);
    match sel.decide(&r) {
        SelectOutcome::LowConfidenceUseTop1 { repo, .. } => assert_eq!(repo, "acme/api"),
        other => panic!("expected LowConfidenceUseTop1, got {other:?}"),
    }
}

#[test]
fn empty_response_refuses() {
    let sel = RepoSelector::from_cfg(0.70, "delegated_reaction").unwrap();
    assert_eq!(
        sel.decide(&response(vec![])),
        SelectOutcome::LowConfidenceRefused
    );
}

#[test]
fn invalid_policy_string_errors() {
    assert!(RepoSelector::from_cfg(0.70, "made_up_policy").is_err());
}
