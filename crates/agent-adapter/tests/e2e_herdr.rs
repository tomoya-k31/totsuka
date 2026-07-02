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

    // Spawn a shell (no Claude needed) that stays alive long enough to be
    // read and closed — herdr deregisters the agent as soon as the process
    // exits, so a bare `echo` would vanish before the read.
    let res = client
        .start(SpawnRequest {
            cwd: "/tmp".into(),
            argv: vec!["bash".into(), "-c".into(), "echo hello; sleep 60".into()],
            env: HashMap::new(),
            label: "totsuka-e2e".into(),
        })
        .await
        .expect("spawn");
    // Poll until the pane has rendered the output (bounded).
    let mut snap = client.read(&res.agent_id).await.expect("read");
    for _ in 0..20 {
        if snap.text.contains("hello") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        snap = client.read(&res.agent_id).await.expect("read");
    }
    assert!(
        snap.text.contains("hello"),
        "pane output should contain the echo, got: {:?}",
        snap.text
    );
    client.close(&res.agent_id).await.expect("close");
}
