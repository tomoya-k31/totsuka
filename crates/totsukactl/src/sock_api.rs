pub mod client;
pub mod dto;
pub mod server;

use crate::error::TotsukactlError;
use std::path::Path;
use tokio::net::UnixListener;

pub use client::SupervisorClient;
pub use dto::{ProcessDto, ShutdownReq};
pub use server::{router, ControlMsg, SockApiState};

pub async fn bind_uds(path: &Path) -> Result<UnixListener, TotsukactlError> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(UnixListener::bind(path)?)
}

pub async fn serve_uds(listener: UnixListener, router: axum::Router) -> Result<(), TotsukactlError> {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use tower::Service;

    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let tower_service = svc
            .call(())
            .await
            .map_err(|e| TotsukactlError::Internal(format!("router make_service: {e}")))?;
        tokio::spawn(async move {
            let hyper_service =
                hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                    let mut svc = tower_service.clone();
                    async move { svc.call(req).await }
                });
            if let Err(e) = ConnBuilder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "supervisor.sock connection error");
            }
        });
    }
}
