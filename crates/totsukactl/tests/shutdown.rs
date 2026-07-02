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
    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        registry
            .set_pid(n, Some(pid), Some(chrono::Utc::now()))
            .await;
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
    shutdown_stack(cfg, registry.clone(), compose, paths.clone())
        .await
        .unwrap();
    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
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

#[tokio::test]
async fn shutdown_graceful_three_stage_walks_full_sequence() {
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
    let dead_pid = 0x7fff_fffe;
    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        registry
            .set_pid(n, Some(dead_pid), Some(chrono::Utc::now()))
            .await;
        std::fs::write(paths.child_pid(n), format!("{dead_pid}\n")).unwrap();
    }
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(20),
        second_term: Duration::from_millis(20),
        force_grace: Duration::from_millis(20),
        also_postgres: false,
        force: false, // exercises 3-stage path
    };
    shutdown_stack(cfg, registry.clone(), compose, paths.clone())
        .await
        .unwrap();

    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        assert!(!paths.child_pid(n).exists(), "{n}.pid should be removed");
        assert_eq!(
            registry.get(n).await.unwrap().state,
            totsukactl::state::ChildState::Stopped
        );
    }
    assert!(!paths.supervisor_pid().exists());
}

fn tmp_paths(tmp: &TempDir) -> Paths {
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    paths
}

/// The per-stage grace/second-term waits are deadlines, not mandatory
/// sleeps: when every child in a stage is already dead the stage must move
/// on immediately, so `down` on an already-exited stack is near-instant
/// instead of a fixed 3×(grace+second).
#[tokio::test]
async fn shutdown_returns_early_when_children_already_dead() {
    let tmp = TempDir::new().unwrap();
    let paths = tmp_paths(&tmp);
    let registry = Arc::new(Registry::new());
    let dead_pid = 0x7fff_fffe;
    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        registry
            .set_pid(n, Some(dead_pid), Some(chrono::Utc::now()))
            .await;
        std::fs::write(paths.child_pid(n), format!("{dead_pid}\n")).unwrap();
    }
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_secs(2),
        second_term: Duration::from_secs(2),
        force_grace: Duration::from_secs(2),
        also_postgres: false,
        force: false, // 3-stage path: fixed sleeps would cost 3×(2+2) = 12s
    };
    let started = std::time::Instant::now();
    shutdown_stack(cfg, registry, compose, paths).await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1500),
        "already-dead children must not be waited on for the full budget (took {elapsed:?})"
    );
}

/// After shutdown the sock dir must be empty: children unlink stale
/// sockets only at startup, so `down` owns removing the leftovers.
/// Otherwise every clean stop shows up as "N stale sockets" in `status`.
#[tokio::test]
async fn shutdown_removes_leftover_socket_files() {
    let tmp = TempDir::new().unwrap();
    let paths = tmp_paths(&tmp);
    let registry = Arc::new(Registry::new());
    let dead_pid = 0x7fff_fffe;
    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        registry
            .set_pid(n, Some(dead_pid), Some(chrono::Utc::now()))
            .await;
    }
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();
    for sock in ["supervisor.sock", "adapter.sock", "qa-service.sock"] {
        std::fs::write(paths.sock_dir.join(sock), "").unwrap();
    }

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(50),
        second_term: Duration::from_millis(50),
        force_grace: Duration::from_millis(50),
        also_postgres: false,
        force: false,
    };
    shutdown_stack(cfg, registry, compose, paths.clone())
        .await
        .unwrap();

    let leftover: Vec<_> = std::fs::read_dir(&paths.sock_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftover.is_empty(),
        "sock dir must be cleaned by shutdown, found: {leftover:?}"
    );
}

/// Regression guard for early-exit polling: a child that ignores SIGTERM
/// must still walk the 2nd-SIGTERM → SIGKILL escalation and end up dead.
#[tokio::test]
async fn shutdown_still_escalates_to_sigkill_for_stubborn_child() {
    let tmp = TempDir::new().unwrap();
    let paths = tmp_paths(&tmp);
    let registry = Arc::new(Registry::new());
    let dead_pid = 0x7fff_fffe;
    for n in ["github-watcher", "qa-service", "orchestrator"] {
        registry
            .set_pid(n, Some(dead_pid), Some(chrono::Utc::now()))
            .await;
        std::fs::write(paths.child_pid(n), format!("{dead_pid}\n")).unwrap();
    }
    // Real process that ignores SIGTERM; only SIGKILL can end it.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "trap '' TERM; while :; do sleep 0.1; done"])
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    registry
        .set_pid("agent-adapter", Some(pid), Some(chrono::Utc::now()))
        .await;
    std::fs::write(paths.child_pid("agent-adapter"), format!("{pid}\n")).unwrap();
    std::fs::write(paths.supervisor_pid(), "1\n").unwrap();

    let compose: Arc<dyn ComposeExec> = Arc::new(MockCompose::default());
    let cfg = ShutdownCfg {
        grace: Duration::from_millis(200),
        second_term: Duration::from_millis(200),
        force_grace: Duration::from_millis(200),
        also_postgres: false,
        force: false,
    };
    shutdown_stack(cfg, registry, compose, paths).await.unwrap();

    // SIGKILL is delivered by shutdown_stack; reap and confirm death.
    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "stubborn child must have been SIGKILLed, got {status:?}"
    );
}
