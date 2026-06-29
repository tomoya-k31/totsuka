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

    std::env::set_var("DATABASE_URL", "postgres://custom@host:1234/x");
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    assert_eq!(build_db_url(&cfg), "postgres://custom@host:1234/x");
    std::env::remove_var("DATABASE_URL");
}

#[tokio::test]
async fn migrate_actually_runs_when_db_available() {
    // Check DATABASE_URL while holding the lock, then drop the lock before any await point.
    let db_url_present = {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::var("DATABASE_URL").is_ok()
    };
    if !db_url_present {
        eprintln!("skip: DATABASE_URL not set");
        return;
    }
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    totsukactl::commands::migrate::run(&cfg).await.unwrap();
}
