use tempfile::TempDir;
use totsukactl::paths::{resolve_tilde, Paths};

#[test]
fn resolve_tilde_expands_when_home_set() {
    std::env::set_var("HOME", "/h");
    let p = resolve_tilde("~/.local/state/totsuka");
    assert_eq!(p, std::path::PathBuf::from("/h/.local/state/totsuka"));
}

#[test]
fn resolve_tilde_passthrough_for_absolute() {
    let p = resolve_tilde("/absolute/path");
    assert_eq!(p, std::path::PathBuf::from("/absolute/path"));
}

#[test]
fn ensure_creates_layout_and_sets_sock_mode_0700() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().join("state");
    let data = tmp.path().join("data");
    let paths = Paths {
        state_dir: state.clone(),
        data_dir: data.clone(),
        log_dir: state.join("logs"),
        pid_dir: state.join("pids"),
        sock_dir: state.join("sock"),
    };
    paths.ensure().unwrap();
    for p in [
        &paths.state_dir,
        &paths.data_dir,
        &paths.log_dir,
        &paths.pid_dir,
        &paths.sock_dir,
    ] {
        assert!(p.is_dir(), "{p:?} missing");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&paths.sock_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}
