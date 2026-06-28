//! Real-herdr smoke test, skipped unless `HERDR_SOCKET` is set.

use agent_adapter::herdr::wire::WireHerdr;
use agent_adapter::herdr::{HerdrClient, SpawnRequest};
use std::collections::HashMap;
use std::path::PathBuf;

fn herdr_socket() -> Option<PathBuf> {
    std::env::var_os("HERDR_SOCKET").map(PathBuf::from)
}

#[tokio::test]
async fn spawn_read_close_against_real_herdr() {
    let Some(sock) = herdr_socket() else {
        eprintln!("HERDR_SOCKET not set; skipping real-herdr e2e");
        return;
    };
    let client = WireHerdr::connect(&sock).await.expect("connect herdr");

    // Spawn a no-op shell so we don't need Claude itself installed.
    let res = client
        .start(SpawnRequest {
            cwd: "/tmp".into(),
            argv: vec!["bash".into(), "-c".into(), "echo hello".into()],
            env: HashMap::new(),
            label: "totsuka-e2e".into(),
        })
        .await
        .expect("spawn");
    let _snap = client.read(&res.agent_id).await.expect("read");
    client.close(&res.agent_id).await.expect("close");
}
