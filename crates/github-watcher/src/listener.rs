//! TCP loopback HTTP listener (spec §7 IPC matrix: github-watcher uses TCP,
//! not UDS, so the same bin can run in a cloud environment later).

use crate::error::WatcherError;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use tokio::net::TcpListener;
use tower::Service;

pub async fn bind_tcp(addr: &str) -> Result<TcpListener, WatcherError> {
    TcpListener::bind(addr)
        .await
        .map_err(|e| WatcherError::Internal(format!("bind {addr}: {e}")))
}

pub async fn serve_tcp(listener: TcpListener, router: axum::Router) -> Result<(), WatcherError> {
    let mut svc = router.into_make_service();
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| WatcherError::Internal(format!("accept: {e}")))?;
        let io = TokioIo::new(stream);
        let tower_service = svc
            .call(())
            .await
            .map_err(|e| WatcherError::Internal(format!("svc.call: {e}")))?;
        tokio::spawn(async move {
            let hyper_service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                let mut svc = tower_service.clone();
                async move { svc.call(req).await }
            });
            if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                .serve_connection(io, hyper_service)
                .await
            {
                tracing::warn!(error=?e, "tcp connection error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_tcp_picks_arbitrary_port() {
        let l = bind_tcp("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0);
    }
}
