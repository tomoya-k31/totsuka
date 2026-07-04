//! UDS + optional TCP listener factories. spec §7: UDS is the primary IPC.

use std::path::Path;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

pub async fn bind_uds(path: &Path) -> anyhow::Result<UnixListener> {
    // Best-effort cleanup of stale socket files. SO_REUSEADDR is not a thing
    // for UDS; previous restarts can leave a file behind.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    Ok(listener)
}

/// Serve until `shutdown` is cancelled, then stop accepting and drain
/// in-flight responses, bounded by `drain` (spec §5: 新規受付停止 →
/// in-flight を deadline 付きで drain → 即 exit).
pub async fn serve_uds(
    listener: UnixListener,
    router: axum::Router,
    shutdown: CancellationToken,
    drain: Duration,
) -> anyhow::Result<()> {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use hyper_util::server::graceful::GracefulShutdown;
    use tower::Service;

    let mut svc = router.into_make_service();
    let graceful = GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                let io = TokioIo::new(stream);
                let tower_service = svc.call(()).await?;
                let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                    let mut svc = tower_service.clone();
                    async move { svc.call(req).await }
                });
                let conn = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(io, hyper_service)
                    .into_owned();
                let watched = graceful.watch(conn);
                tokio::spawn(async move {
                    if let Err(e) = watched.await {
                        if totsuka_telemetry::is_benign_disconnect(e.as_ref()) {
                            tracing::debug!(error=?e, "uds connection closed by peer");
                        } else {
                            tracing::warn!(error=?e, "uds connection error");
                        }
                    }
                });
            }
            _ = shutdown.cancelled() => {
                drop(listener); // stop accepting new connections
                if tokio::time::timeout(drain, graceful.shutdown()).await.is_err() {
                    tracing::warn!(deadline=?drain, "drain deadline exceeded; abandoning open connections");
                }
                return Ok(());
            }
        }
    }
}
