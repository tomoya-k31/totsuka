use totsukactl::state::{next_state, ChildState, HealthOutcome, RestartPolicy};

#[test]
fn parse_restart_policy_recognises_all_three() {
    assert_eq!(
        RestartPolicy::parse("on-dead-only").unwrap(),
        RestartPolicy::OnDeadOnly
    );
    assert_eq!(
        RestartPolicy::parse("on-unhealthy").unwrap(),
        RestartPolicy::OnUnhealthy
    );
    assert_eq!(RestartPolicy::parse("never").unwrap(), RestartPolicy::Never);
    assert!(RestartPolicy::parse("garbage").is_err());
}

#[test]
fn ok_outcome_promotes_to_healthy_from_any_live_state() {
    for from in [
        ChildState::Starting,
        ChildState::Ready,
        ChildState::Degraded,
        ChildState::Unhealthy,
    ] {
        assert_eq!(
            next_state(from, HealthOutcome::Ok, 0, 2, 3),
            ChildState::Healthy
        );
    }
}

#[test]
fn degraded_outcome_only_after_threshold() {
    // below threshold: stays put
    assert_eq!(
        next_state(ChildState::Healthy, HealthOutcome::Degraded, 1, 2, 3),
        ChildState::Healthy
    );
    // hits degraded threshold (2)
    assert_eq!(
        next_state(ChildState::Healthy, HealthOutcome::Degraded, 2, 2, 3),
        ChildState::Degraded
    );
    // hits unhealthy threshold (3)
    assert_eq!(
        next_state(ChildState::Healthy, HealthOutcome::Degraded, 3, 2, 3),
        ChildState::Unhealthy
    );
}

#[test]
fn dead_outcome_overrides_everything() {
    for from in [
        ChildState::Healthy,
        ChildState::Degraded,
        ChildState::Unhealthy,
    ] {
        assert_eq!(
            next_state(from, HealthOutcome::Dead, 0, 2, 3),
            ChildState::Dead
        );
    }
}

#[test]
fn terminal_states_are_sticky_under_ticks() {
    for from in [
        ChildState::GivingUp,
        ChildState::Draining,
        ChildState::Stopped,
        ChildState::Restarting,
    ] {
        for outcome in [
            HealthOutcome::Ok,
            HealthOutcome::Degraded,
            HealthOutcome::Unhealthy,
            HealthOutcome::Dead,
        ] {
            assert_eq!(next_state(from, outcome, 5, 2, 3), from);
        }
    }
}
