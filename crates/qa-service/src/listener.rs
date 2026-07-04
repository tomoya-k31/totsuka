use crate::error::QaError;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use std::path::Path;
use tokio::net::UnixListener;
use tower::Service;

pub async fn bind_uds(path: &Path) -> Result<UnixListener, QaError> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| QaError::Internal(format!("remove old uds: {e}")))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| QaError::Internal(format!("create dir: {e}")))?;
    }
    UnixListener::bind(path).map_err(|e| QaError::Internal(format!("bind uds: {e}")))
}

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> Result<(), QaError> {
    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| QaError::Internal(format!("accept: {e}")))?;
        let io = TokioIo::new(stream);
        let tower_service = svc
            .call(())
            .await
            .map_err(|e| QaError::Internal(format!("svc.call: {e}")))?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                if totsuka_telemetry::is_benign_disconnect(e.as_ref()) {
                    tracing::debug!(error=?e, "uds connection closed by peer");
                } else {
                    tracing::warn!(error=?e, "uds connection error");
                }
            }
        });
    }
}
