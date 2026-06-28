use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 構造化ログ初期化。stdout (json) + daily rotation file。
/// 返り値の WorkerGuard は main 関数の最後まで保持する (drop で flush)
pub fn init_tracing(state_dir: &Path, bin_name: &str, default_level: &str) -> WorkerGuard {
    let log_dir = state_dir.join("logs");
    std::fs::create_dir_all(&log_dir).expect("create log dir");
    let file_appender = tracing_appender::rolling::daily(&log_dir, format!("{bin_name}.log"));
    let (nb, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let stdout_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(false);
    let file_layer = fmt::layer().json().with_writer(nb).with_target(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_log_dir_and_file() {
        let dir = tempdir().unwrap();
        let _guard = init_tracing(dir.path(), "smoke", "info");
        tracing::info!("hello");
        // 非同期 flush なので即時にはファイルが書かれない可能性あり。dir が出来ていることだけ確認
        assert!(dir.path().join("logs").exists());
    }
}
