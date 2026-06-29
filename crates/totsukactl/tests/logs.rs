use tempfile::TempDir;
use totsukactl::commands::logs::{run, tail_lines};
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

#[test]
fn tail_lines_takes_last_n_only() {
    let text = "a\nb\nc\nd\ne\n";
    assert_eq!(tail_lines(text, 2), "d\ne\n");
    assert_eq!(tail_lines(text, 0), "\n");
    assert_eq!(tail_lines(text, 99), text);
}

#[test]
fn tail_lines_preserves_no_trailing_newline() {
    let text = "a\nb\nc";
    assert_eq!(tail_lines(text, 2), "b\nc");
}

#[tokio::test]
async fn run_errors_for_unknown_bin() {
    let tmp = TempDir::new().unwrap();
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    let err = run(&p, "no-such-bin", 10, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::UnknownChild(_)));
}

#[tokio::test]
async fn run_errors_when_log_missing() {
    let tmp = TempDir::new().unwrap();
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    let err = run(&p, "orchestrator", 10, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Internal(_)));
}
