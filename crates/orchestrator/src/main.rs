use std::sync::Arc;

use orchestrator::adapter_client::HyperlocalAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::lifecycle::{probe_adapter, probe_db, wait_for_signals};
use orchestrator::listener::{bind_uds, resolve_uds_path, serve_uds};
use orchestrator::repository::PgRepository;
use orchestrator::schema_check::check_schema_version;
use orchestrator::sm::Engine;
use orchestrator::sweeper::run_sweeper;
use orchestrator::timer::run_timer;
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load config
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);

    // 2. Init tracing — hold WorkerGuard until process exit
    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard =
        totsuka_telemetry::init_tracing(&state_dir, "orchestrator", &config.totsuka.log_level);

    // 3. Open PgPool
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

    // 4. Schema version handshake
    check_schema_version(&pool).await?;

    // 5. Idempotent queue creation
    create_queue(&pool, &config.bus.queue_name).await?;

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);

    // 6. HyperlocalAdapter (type annotation required — not `as` cast)
    let adapter_path = resolve_uds_path(&config.orchestrator.adapter_uds);
    let adapter: Arc<dyn orchestrator::adapter_client::AdapterClient> =
        Arc::new(HyperlocalAdapter::new(adapter_path));

    // 7. Writeback: production = GraphqlWriteback; MockWriteback until
    //    option_ids loader lands (swap point noted here for Task 22).
    let writeback: Arc<dyn orchestrator::gh_writeback::WritebackClient> =
        Arc::new(MockWriteback::new());

    let health = HealthState::new();

    // 8. Build Engine
    let engine = Arc::new(Engine {
        repo: Arc::new(PgRepository::new(pool.clone(), clock.clone())),
        adapter: adapter.clone(),
        writeback,
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(config.orchestrator.wip_global)),
        clock: clock.clone(),
        config: config.clone(),
        owner_id: format!("orch-{}", std::process::id()),
    });

    // 9. Probes + mark ready
    probe_db(&pool, &health).await;
    probe_adapter(adapter.clone(), &health).await;
    health.set_ready(true).await;

    // 10. Spawn 4 tasks via CancellationToken
    let shutdown = CancellationToken::new();

    let consumer_h = {
        let e = engine.clone();
        let p = pool.clone();
        let q = config.bus.queue_name.clone();
        let bs = config.bus.batch_size as i32;
        let vt = config.bus.visibility_secs as i32;
        let s = shutdown.clone();
        tokio::spawn(async move { run_consumer(e, p, q, bs, vt, s).await })
    };
    let timer_h = {
        let e = engine.clone();
        let s = shutdown.clone();
        tokio::spawn(async move { run_timer(e, 30, s).await })
    };
    let sweeper_h = {
        let p = pool.clone();
        let s = shutdown.clone();
        tokio::spawn(async move { run_sweeper(p, 30, s).await })
    };

    let router = totsuka_telemetry::http::router(health.clone()).layer(axum::middleware::from_fn(
        totsuka_telemetry::request_id::middleware,
    ));
    let uds_path = resolve_uds_path(&config.orchestrator.uds_path);
    let listener = bind_uds(&uds_path).await?;
    let server_h = tokio::spawn(async move { serve_uds(listener, router).await });

    // 11. Signals task — cancels the token on SIGTERM
    let _signals_h = tokio::spawn(wait_for_signals(shutdown.clone()));

    // 12. Wait for any task to exit, then return
    tokio::select! {
        r = consumer_h => { let _ = r?; },
        r = timer_h    => { let _ = r?; },
        r = sweeper_h  => { let _ = r?; },
        r = server_h   => { let _ = r?; },
    }
    Ok(())
}
