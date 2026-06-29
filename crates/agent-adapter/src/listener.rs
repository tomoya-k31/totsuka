//! UDS + optional TCP listener factories. spec §7: UDS is the primary IPC.

use std::path::Path;
use tokio::net::UnixListener;

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

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> anyhow::Result<()> {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use tower::Service;

    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let tower_service = svc.call(()).await?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "uds connection error");
            }
        });
    }
}
