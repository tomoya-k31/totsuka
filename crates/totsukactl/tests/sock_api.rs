use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use totsukactl::registry::Registry;
use totsukactl::sock_api::{
    bind_uds, router, serve_uds, ControlMsg, SockApiState, SupervisorClient,
};
use totsukactl::state::ChildState;

#[tokio::test]
async fn list_round_trip_returns_registry_entries() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    registry
        .set_state("orchestrator", ChildState::Healthy)
        .await;
    let (tx, _rx) = mpsc::channel(8);
    let state = SockApiState {
        registry: registry.clone(),
        control_tx: tx,
    };
    let listener = bind_uds(&sock).unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    let client = SupervisorClient::new(sock.clone());
    let list = client.list().await.unwrap();
    let orch = list.into_iter().find(|p| p.name == "orchestrator").unwrap();
    assert_eq!(orch.state, ChildState::Healthy);
}

#[tokio::test]
async fn shutdown_post_enqueues_control_msg() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    let (tx, mut rx) = mpsc::channel(8);
    let state = SockApiState {
        registry,
        control_tx: tx,
    };
    let listener = bind_uds(&sock).unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    let client = SupervisorClient::new(sock);
    client.shutdown(true, false).await.unwrap();
    let msg = rx.recv().await.unwrap();
    assert!(matches!(
        msg,
        ControlMsg::Shutdown {
            postgres: true,
            force: false
        }
    ));
}

#[tokio::test]
async fn reload_rejects_non_adapter() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("supervisor.sock");
    let registry = Arc::new(Registry::new());
    let (tx, _rx) = mpsc::channel(8);
    let state = SockApiState {
        registry,
        control_tx: tx,
    };
    let listener = bind_uds(&sock).unwrap();
    let r = router(state);
    let _h = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    let client = SupervisorClient::new(sock);
    let err = client.reload("orchestrator").await.unwrap_err();
    assert!(format!("{err}").contains("400"));
}
