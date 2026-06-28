use std::sync::Arc;
use std::time::Duration;

use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, HttpGhClient};
use github_watcher::lifecycle::{probe_db, probe_github, wait_for_signals};
use github_watcher::listener::{bind_tcp, serve_tcp};
use github_watcher::polling::{
    issues::{run_issues_loop, IssuesLoopConfig},
    project::{run_project_loop, ProjectLoopConfig},
    prs::{run_prs_loop, PrsLoopConfig},
    releases::{run_releases_loop, ReleasesLoopConfig},
    RepoTracker,
};
use github_watcher::schema_check::check_schema_version;
use github_watcher::snapshot::PgSnapshotStore;
use github_watcher::{column_map, WatcherApp};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_bus::Publisher;
use totsuka_core::{ColumnMap, SystemClock};
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + tracing
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);
    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard =
        totsuka_telemetry::init_tracing(&state_dir, "github-watcher", &config.totsuka.log_level);

    // 2. DB
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            config.postgres.user,
            config.postgres.password.expose(),
            config.postgres.host,
            config.postgres.port,
            config.postgres.database,
        )
    });
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await?;
    check_schema_version(&pool).await?;
    create_queue(&pool, &config.bus.queue_name).await?;

    // 3. ColumnMap + clock + publisher
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let column_map: Arc<ColumnMap> = Arc::new(column_map::build(&config)?);
    let publisher = Arc::new(Publisher::new(config.bus.queue_name.clone(), clock.clone()));

    // 4. GhClient
    let token = config.github_watcher.github_token.clone();
    let client: Arc<dyn GhClient> = Arc::new(HttpGhClient::new(token));
    let project_node_id = client
        .resolve_project_node_id(&config.github.project_owner, config.github.project_number)
        .await?;

    // 5. Health + probes
    let health = HealthState::new();
    probe_db(&pool, &health).await;
    probe_github(
        &client,
        &config.github.project_owner,
        config.github.project_number,
        &health,
    )
    .await;
    health.set_ready(true).await;

    // 6. Shared state
    let tracker = RepoTracker::new();
    let snapshot = Arc::new(PgSnapshotStore::new(pool.clone(), publisher.clone()));
    let shutdown = CancellationToken::new();

    // 7. Loops
    let project_h = {
        let cfg = ProjectLoopConfig {
            project_node_id,
            page_size: config.github_watcher.graphql_page_size,
            poll_interval: Duration::from_secs(config.github_watcher.project_poll_interval_secs),
        };
        let s = shutdown.clone();
        let pool = pool.clone();
        let client = client.clone();
        let snapshot = snapshot.clone();
        let column_map = column_map.clone();
        let tracker = tracker.clone();
        let clock = clock.clone();
        let health = health.clone();
        tokio::spawn(async move {
            run_project_loop(
                pool, client, snapshot, column_map, tracker, clock, health, cfg, s,
            )
            .await
        })
    };

    let catchup = chrono::Duration::hours(config.github_watcher.catchup_window_hours as i64);
    let issues_poll = Duration::from_secs(config.github_watcher.issues_poll_interval_secs);

    let issues_h = {
        let pool = pool.clone();
        let publisher = publisher.clone();
        let client = client.clone();
        let tracker = tracker.clone();
        let clock = clock.clone();
        let health = health.clone();
        let s = shutdown.clone();
        spawn_loop("issues", async move {
            run_issues_loop(
                pool,
                publisher,
                client,
                tracker,
                clock,
                health,
                IssuesLoopConfig {
                    poll_interval: issues_poll,
                    catchup_window: catchup,
                },
                s,
            )
            .await
        })
    };
    let prs_h = {
        let pool = pool.clone();
        let publisher = publisher.clone();
        let client = client.clone();
        let tracker = tracker.clone();
        let clock = clock.clone();
        let health = health.clone();
        let s = shutdown.clone();
        spawn_loop("prs", async move {
            run_prs_loop(
                pool,
                publisher,
                client,
                tracker,
                clock,
                health,
                PrsLoopConfig {
                    poll_interval: issues_poll,
                    catchup_window: catchup,
                },
                s,
            )
            .await
        })
    };
    let releases_h = {
        let pool = pool.clone();
        let publisher = publisher.clone();
        let client = client.clone();
        let tracker = tracker.clone();
        let clock = clock.clone();
        let health = health.clone();
        let s = shutdown.clone();
        spawn_loop("releases", async move {
            run_releases_loop(
                pool,
                publisher,
                client,
                tracker,
                clock,
                health,
                ReleasesLoopConfig {
                    poll_interval: issues_poll,
                    catchup_window: catchup,
                },
                s,
            )
            .await
        })
    };

    // 8. Listener
    let listener = bind_tcp(&config.github_watcher.bind).await?;
    let router = totsuka_telemetry::http::router(health.clone()).layer(axum::middleware::from_fn(
        totsuka_telemetry::request_id::middleware,
    ));
    let listener_h = tokio::spawn(async move { serve_tcp(listener, router).await });

    // 9. Signals
    let _signals = tokio::spawn(wait_for_signals(shutdown.clone()));

    // 10. WatcherApp::new() is left as a no-op constructor for tests
    let _app = WatcherApp::new(config, clock);

    // 11. Wait on first
    tokio::select! {
        r = project_h  => { let _ = r?; },
        r = issues_h   => { let _ = r?; },
        r = prs_h      => { let _ = r?; },
        r = releases_h => { let _ = r?; },
        r = listener_h => { let _ = r?; },
    }
    // 12. Probe: did the cursor get persisted? (smoke for logs)
    if let Ok(Some(c)) = get(&pool, &CursorKey::project_items()).await {
        tracing::info!(cursor=%c, "project_items cursor at exit");
    }
    Ok(())
}

fn spawn_loop<F>(
    name: &'static str,
    fut: F,
) -> tokio::task::JoinHandle<Result<(), github_watcher::error::WatcherError>>
where
    F: std::future::Future<Output = Result<(), github_watcher::error::WatcherError>>
        + Send
        + 'static,
{
    tokio::spawn(async move {
        tracing::info!(loop_name = name, "starting");
        let r = fut.await;
        tracing::info!(loop_name = name, "exited");
        r
    })
}
