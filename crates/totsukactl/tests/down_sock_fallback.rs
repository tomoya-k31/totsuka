//! Verify that `totsukactl down` falls back to talking to supervisor.sock when
//! the pidfile is absent. Spins an in-process UDS server that accepts a
//! `POST /v1/shutdown`, then calls `down::run`, then asserts the shutdown was
//! delivered.

use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use totsukactl::commands::down;
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState};

fn paths(tmp: &TempDir) -> Paths {
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn down_falls_back_to_sock_when_pid_absent() {
    let tmp = TempDir::new().unwrap();
    let paths = paths(&tmp);

    // No supervisor.pid is written — simulate the "pidfile vanished while
    // supervisor still runs" race that the smoke test surfaced.
    assert!(!paths.supervisor_pid().exists(), "pidfile must be absent");

    // Stand up a minimal supervisor.sock that accepts shutdown messages.
    let registry = Arc::new(Registry::new());
    let (tx, mut rx) = mpsc::channel::<ControlMsg>(8);
    let state = SockApiState {
        registry,
        control_tx: tx,
    };
    let listener = bind_uds(&paths.supervisor_sock()).unwrap();
    let r = router(state);
    let _h_sock = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    // Run down. It must NOT return NotRunning.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        down::run(
            &paths,
            /*force=*/ true,
            /*postgres=*/ false,
            std::time::Duration::from_secs(1),
        ),
    )
    .await
    .expect("down::run should not hang");

    // We DO accept `Err(Timeout(...))` here because there's no actual supervisor
    // process to die — the goal is to confirm the shutdown was *delivered*.
    // What we MUST NOT see is NotRunning.
    if let Err(totsukactl::error::TotsukactlError::NotRunning) = &result {
        panic!("down returned NotRunning despite live sock — fallback failed");
    }

    // The supervisor.sock received the shutdown control msg.
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("shutdown msg should arrive within 2s")
        .expect("shutdown msg present");
    assert!(
        matches!(
            msg,
            ControlMsg::Shutdown {
                postgres: false,
                force: true
            }
        ),
        "expected Shutdown(postgres=false, force=true); got {msg:?}"
    );
}
