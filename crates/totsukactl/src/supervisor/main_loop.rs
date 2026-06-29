use crate::child::{specs_from_config, ChildSpawner, ForkExecSpawner};
use crate::compose::{ComposeExec, DockerCompose};
use crate::error::TotsukactlError;
use crate::health::{endpoint_for, Endpoint, HttpHealthProbe};
use crate::heartbeat::{run_healthz_loop, run_pgmq_loop, run_readyz_loop, HeartbeatCfg};
use crate::paths::{resolve_tilde, Paths};
use crate::pgmq_probe::{LivePgmqProbe, PgmqProbe};
use crate::probe::Preflight;
use crate::registry::Registry;
use crate::restart::RestartCfg;
use crate::sock_api::{bind_uds, router, serve_uds, ControlMsg, SockApiState};
use crate::supervisor::boot::{boot, BootCtx};
use crate::supervisor::shutdown::{shutdown_stack, ShutdownCfg};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;

pub async fn run_supervisor(
    cfg: totsuka_config::Config,
    paths: Paths,
    recreate: bool,
) -> Result<(), TotsukactlError> {
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let compose: Arc<dyn ComposeExec> = Arc::new(DockerCompose::new(PathBuf::from(
        &cfg.postgres.compose_file,
    )));

    // Phase -1: ensure pgmq container is running
    let pre = Preflight {
        compose: compose.clone(),
        cfg: &cfg,
    };
    pre.run_phase_minus1(recreate).await?;

    // Open pool (after compose up — pgmq may still need a few seconds; PgPool retries on connect).
    let db_url = crate::commands::migrate::build_db_url(&cfg);
    let pool = retry_connect(&db_url, Duration::from_secs(30)).await?;

    // Phase 0: schema check + herdr socket ping
    pre.run_phase_0(&pool, &resolve_tilde(&cfg.agent_adapter.herdr_socket))
        .await?;

    let registry = Arc::new(Registry::new());
    let spawner_arc: Arc<dyn ChildSpawner> = Arc::new(ForkExecSpawner);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|pp| pp.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin"));
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let specs = specs_from_config(&cfg, &paths, &exe_dir, &config_path);

    let mut eps: HashMap<String, Endpoint> = HashMap::new();
    for n in [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ] {
        eps.insert(n.into(), endpoint_for(n, &cfg)?);
    }
    let probe: Arc<dyn crate::health::HealthProbe> = Arc::new(HttpHealthProbe::new(eps));

    let ctx = BootCtx {
        spawner: spawner_arc.clone(),
        probe: probe.clone(),
        registry: registry.clone(),
        clock: clock.clone(),
        paths: paths.clone(),
        ready_timeout: Duration::from_secs(cfg.supervisor.ready_timeout_secs),
    };

    // Boot sequence: pgmq + phase-0 already done above; pass no-ops.
    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) }).await?;

    // Three heartbeat tickers
    let hb: HeartbeatCfg = (&cfg.supervisor.heartbeat).into();
    let shutdown_tok = CancellationToken::new();
    let bins = vec![
        "agent-adapter".to_string(),
        "orchestrator".into(),
        "github-watcher".into(),
        "qa-service".into(),
    ];
    let pgmq_probe: Arc<dyn PgmqProbe> = Arc::new(LivePgmqProbe {
        compose: compose.clone(),
        pool: pool.clone(),
    });
    let h_hb = tokio::spawn(run_healthz_loop(
        hb.clone(),
        probe.clone(),
        registry.clone(),
        clock.clone(),
        bins.clone(),
        shutdown_tok.clone(),
    ));
    let h_rd = tokio::spawn(run_readyz_loop(
        hb.clone(),
        probe.clone(),
        registry.clone(),
        clock.clone(),
        bins.clone(),
        shutdown_tok.clone(),
    ));
    let h_pg = tokio::spawn(run_pgmq_loop(
        hb.clone(),
        pgmq_probe,
        registry.clone(),
        clock.clone(),
        shutdown_tok.clone(),
    ));

    // Capture shutdown config values before spawning tasks that need them.
    let shutdown_cfg_grace = Duration::from_secs(cfg.supervisor.shutdown_grace_secs);
    let shutdown_cfg_kill = Duration::from_secs(cfg.supervisor.shutdown_kill_secs);

    let restart_cfg = RestartCfg::from_section(&cfg.supervisor.heartbeat)?;
    let h_rt = tokio::spawn(crate::supervisor::restart_tick::run_restart_tick(
        std::time::Duration::from_secs(10),
        registry.clone(),
        spawner_arc.clone(),
        specs.clone(),
        paths.clone(),
        clock.clone(),
        restart_cfg.clone(),
        shutdown_cfg_kill,
        shutdown_tok.clone(),
    ));

    // sock_api server
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ControlMsg>(16);
    let listener = bind_uds(&paths.supervisor_sock())?;
    let state = SockApiState {
        registry: registry.clone(),
        control_tx: ctl_tx,
    };
    let r = router(state);
    let h_sock = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    // Control dispatcher: loop until SIGTERM/SIGINT or ControlMsg::Shutdown.
    {
        let registry = registry.clone();
        let compose = compose.clone();
        let paths = paths.clone();
        let shutdown_tok = shutdown_tok.clone();

        let mut term = signal(SignalKind::terminate())
            .map_err(|e| TotsukactlError::Internal(format!("install SIGTERM: {e}")))?;
        let mut int = signal(SignalKind::interrupt())
            .map_err(|e| TotsukactlError::Internal(format!("install SIGINT: {e}")))?;

        let (also_postgres, force): (bool, bool) = loop {
            tokio::select! {
                _ = term.recv() => break (false, false),
                _ = int.recv()  => break (false, false),
                msg = ctl_rx.recv() => match msg {
                    Some(ControlMsg::Shutdown { postgres, force }) => break (postgres, force),
                    Some(ControlMsg::Restart(name)) => {
                        if let Err(e) = crate::supervisor::control::handle_restart(
                            &name,
                            registry.clone(),
                            spawner_arc.clone(),
                            &specs,
                            &paths,
                            clock.clone(),
                            &restart_cfg,
                            shutdown_cfg_grace,
                        )
                        .await
                        {
                            tracing::error!(child = %name, error = %e, "restart failed");
                        }
                    }
                    Some(ControlMsg::Reload(name)) => {
                        if let Err(e) =
                            crate::supervisor::control::handle_reload(&name, registry.clone())
                                .await
                        {
                            tracing::error!(child = %name, error = %e, "reload failed");
                        }
                    }
                    None => {
                        ctl_rx = crate::supervisor::ctl_replace::replace_closed_ctl_rx(ctl_rx).await;
                    }
                },
            }
        };

        shutdown_tok.cancel();
        shutdown_stack(
            ShutdownCfg {
                grace: shutdown_cfg_grace,
                second_term: shutdown_cfg_kill,
                force_grace: Duration::from_secs(3),
                also_postgres,
                force,
            },
            registry,
            compose,
            paths,
        )
        .await?;
    }

    h_sock.abort();
    h_rt.abort();
    let _ = tokio::join!(h_hb, h_rd, h_pg, h_rt, h_sock);
    Ok(())
}

async fn retry_connect(url: &str, total: Duration) -> Result<sqlx::PgPool, TotsukactlError> {
    let deadline = std::time::Instant::now() + total;
    let mut delay = Duration::from_millis(500);
    loop {
        match PgPoolOptions::new().max_connections(4).connect(url).await {
            Ok(p) => return Ok(p),
            Err(e) if std::time::Instant::now() < deadline => {
                tracing::warn!(error = %e, "postgres connect failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(3));
            }
            Err(e) => {
                return Err(TotsukactlError::Probe(format!("postgres connect: {e}")));
            }
        }
    }
}
