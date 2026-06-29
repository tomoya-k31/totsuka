use totsukactl::health::{endpoint_for, Endpoint, HealthProbe, MockHealthProbe};

const TOML: &str = include_str!("./fixtures/min_config.toml");

#[test]
fn endpoint_for_maps_each_known_bin() {
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let ep = endpoint_for("agent-adapter", &cfg).unwrap();
    assert!(matches!(ep, Endpoint::Uds(_)));
    let ep = endpoint_for("github-watcher", &cfg).unwrap();
    assert!(matches!(ep, Endpoint::Tcp(addr) if addr.starts_with("127.0.0.1:")));
}

#[test]
fn endpoint_for_unknown_errors() {
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    assert!(endpoint_for("not-a-bin", &cfg).is_err());
}

#[tokio::test]
async fn mock_probe_returns_canned_values() {
    let m = MockHealthProbe::default();
    m.set_healthy("orchestrator", false);
    assert!(!m.healthz("orchestrator").await.unwrap());
    assert!(m.readyz("orchestrator").await.unwrap()); // default true
}
