use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::supervisor::shutdown::{shutdown_stack, ShutdownCfg};

#[tokio::test]
async fn shutdown_clears_pid_files_and_sets_stopped_state() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    // Use a fictitious pid (kill on a non-existent pid is a no-op so we exercise
    // the cleanup path only without risking signalling the test runner).
    let pid: i32 = 0x7fff_fffe;
    for n in ["github-watcher", "qa-service", "orchestrator", "agent-adapter"] {
        registry.set_pid(n, Some(pid), Some(chrono::Utc::now())).await;
        std::fs::write(paths.child_pid(n), format!("{pid}\n")).unwrap();
    }
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(50),
        second_term: Duration::from_millis(50),
        force_grace: Duration::from_millis(50),
        also_postgres: false,
        force: true, // use force to skip the multi-stage waits in the test
    };
    shutdown_stack(cfg, registry.clone(), compose, paths.clone()).await.unwrap();
    for n in ["github-watcher", "qa-service", "orchestrator", "agent-adapter"] {
        assert!(!paths.child_pid(n).exists(), "{n}.pid still exists");
        assert_eq!(
            registry.get(n).await.unwrap().state,
            totsukactl::state::ChildState::Stopped
        );
    }
    assert!(!paths.supervisor_pid().exists());
}

#[tokio::test]
async fn shutdown_with_postgres_calls_compose_stop() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    let compose_concrete = Arc::new(MockCompose::default());
    let compose: Arc<dyn ComposeExec> = compose_concrete.clone();
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(10),
        second_term: Duration::from_millis(10),
        force_grace: Duration::from_millis(10),
        also_postgres: true,
        force: true,
    };
    shutdown_stack(cfg, registry, compose, paths).await.unwrap();
    assert!(compose_concrete.calls().iter().any(|c| c == "stop:pgmq"));
}
