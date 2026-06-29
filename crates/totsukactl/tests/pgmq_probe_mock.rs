use totsukactl::pgmq_probe::{MockPgmqProbe, PgmqProbe};

#[tokio::test]
async fn mock_pgmq_probe_returns_canned() {
    let p = MockPgmqProbe::new(true);
    assert!(p.ping().await.unwrap());
    p.set(false);
    assert!(!p.ping().await.unwrap());
}
