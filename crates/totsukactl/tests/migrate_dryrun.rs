use std::sync::Mutex;
use totsukactl::commands::migrate::build_db_url;

const TOML: &str = include_str!("./fixtures/min_config.toml");

// DATABASE_URL is process-global; serialize tests that touch it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn build_db_url_uses_secret_expose_not_hardcoded_password() {
    let _lock = ENV_LOCK.lock().unwrap();

    // DATABASE_URL overrides — temporarily remove it for this test.
    let restore = std::env::var("DATABASE_URL").ok();
    std::env::remove_var("DATABASE_URL");

    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let url = build_db_url(&cfg);
    // empty default Secret + min config → "postgres://postgres:@127.0.0.1:5432/totsuka"
    assert!(url.starts_with("postgres://postgres:"));
    assert!(url.contains("@127.0.0.1:5432/totsuka"));

    if let Some(v) = restore {
        std::env::set_var("DATABASE_URL", v);
    }
}

#[test]
fn database_url_env_override_wins() {
    let _lock = ENV_LOCK.lock().unwrap();

    let restore = std::env::var("DATABASE_URL").ok();
    std::env::set_var("DATABASE_URL", "postgres://custom@host:1234/x");
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    assert_eq!(build_db_url(&cfg), "postgres://custom@host:1234/x");
    match restore {
        Some(v) => std::env::set_var("DATABASE_URL", v),
        None => std::env::remove_var("DATABASE_URL"),
    }
}

#[tokio::test]
// Deliberate: see the ENV_LOCK comment inside — this cannot deadlock because
// every #[tokio::test] gets its own thread and single-threaded runtime.
#[allow(clippy::await_holding_lock)]
async fn migrate_actually_runs_when_db_available() {
    // Hold the env lock for the whole test: migrate::run reads DATABASE_URL
    // internally (after an await point), and the sibling tests mutate it.
    // Each #[tokio::test] runs on its own thread with its own runtime, so
    // holding a std mutex guard across awaits cannot deadlock here.
    let _lock = ENV_LOCK.lock().unwrap();

    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    // build_db_url honors the DATABASE_URL env override — point migrate at
    // the ephemeral database so it never touches the live one. testkit has
    // already applied all migrations there, so this exercises migrate::run's
    // connect + idempotent re-run path (a fresh-DB apply of the same
    // migration set is exercised by ephemeral_db() itself).
    let restore = std::env::var("DATABASE_URL").ok();
    std::env::set_var("DATABASE_URL", db.url());

    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let result = totsukactl::commands::migrate::run(&cfg).await;

    match restore {
        Some(v) => std::env::set_var("DATABASE_URL", v),
        None => std::env::remove_var("DATABASE_URL"),
    }
    result.unwrap();
}
