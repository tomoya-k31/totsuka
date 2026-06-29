use std::time::Duration;
use totsukactl::restart::{backoff_for, decide, RestartCfg, RestartDecision};
use totsukactl::state::{ChildState, RestartPolicy};

fn cfg(policy: RestartPolicy) -> RestartCfg {
    RestartCfg { policy, backoff_secs: vec![5, 15, 60], max_attempts: 5 }
}

#[test]
fn never_policy_always_skips() {
    let c = cfg(RestartPolicy::Never);
    assert_eq!(decide(ChildState::Dead, 0, &c), RestartDecision::Skip);
    assert_eq!(decide(ChildState::Unhealthy, 0, &c), RestartDecision::Skip);
}

#[test]
fn on_dead_only_skips_unhealthy() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(decide(ChildState::Unhealthy, 0, &c), RestartDecision::Skip);
    assert_eq!(decide(ChildState::Dead, 0, &c), RestartDecision::Wait(Duration::from_secs(5)));
}

#[test]
fn on_unhealthy_wakes_for_both() {
    let c = cfg(RestartPolicy::OnUnhealthy);
    assert_eq!(decide(ChildState::Unhealthy, 1, &c), RestartDecision::Wait(Duration::from_secs(15)));
    assert_eq!(decide(ChildState::Dead, 2, &c), RestartDecision::Wait(Duration::from_secs(60)));
}

#[test]
fn backoff_clamps_to_last_entry() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(backoff_for(0, &c), Duration::from_secs(5));
    assert_eq!(backoff_for(2, &c), Duration::from_secs(60));
    assert_eq!(backoff_for(99, &c), Duration::from_secs(60));
}

#[test]
fn give_up_at_max_attempts() {
    let c = cfg(RestartPolicy::OnDeadOnly);
    assert_eq!(decide(ChildState::Dead, 5, &c), RestartDecision::GiveUp);
    assert_eq!(decide(ChildState::Dead, 6, &c), RestartDecision::GiveUp);
}

#[test]
fn from_section_parses_kebab_case() {
    use totsuka_config::schema::HeartbeatSection;
    let s = HeartbeatSection {
        healthz_interval_secs: 5,
        readyz_interval_secs: 30,
        pgmq_interval_secs: 30,
        unhealthy_threshold: 3,
        degraded_threshold: 2,
        restart_policy: "on-dead-only".into(),
        restart_backoff_secs: vec![1, 2],
        restart_max_attempts: 2,
        notify_on_degraded: false,
    };
    let c = RestartCfg::from_section(&s).unwrap();
    assert_eq!(c.policy, RestartPolicy::OnDeadOnly);
    assert_eq!(c.backoff_secs, vec![1, 2]);
    assert_eq!(c.max_attempts, 2);
}
