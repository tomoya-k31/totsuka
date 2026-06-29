use crate::error::TotsukactlError;
use crate::paths::resolve_tilde;
use async_trait::async_trait;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::Request;
use hyperlocal::{UnixClientExt, UnixConnector, Uri as HyperlocalUri};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum Endpoint {
    Uds(PathBuf),
    Tcp(String),
}

pub fn endpoint_for(name: &str, cfg: &totsuka_config::Config) -> Result<Endpoint, TotsukactlError> {
    match name {
        "agent-adapter" => Ok(Endpoint::Uds(resolve_tilde(&cfg.agent_adapter.uds_path))),
        "orchestrator" => Ok(Endpoint::Uds(resolve_tilde(&cfg.orchestrator.uds_path))),
        "qa-service" => Ok(Endpoint::Uds(resolve_tilde(&cfg.qa_service.uds_path))),
        "github-watcher" => Ok(Endpoint::Tcp(cfg.github_watcher.bind.clone())),
        other => Err(TotsukactlError::UnknownChild(other.into())),
    }
}

#[async_trait]
pub trait HealthProbe: Send + Sync {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError>;
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError>;
}

pub struct HttpHealthProbe {
    endpoints: HashMap<String, Endpoint>,
}

impl HttpHealthProbe {
    pub fn new(endpoints: HashMap<String, Endpoint>) -> Self {
        Self { endpoints }
    }

    async fn hit(&self, name: &str, path: &str) -> Result<u16, TotsukactlError> {
        let ep = self
            .endpoints
            .get(name)
            .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
        match ep {
            Endpoint::Uds(sock) => {
                let client: hyper_util::client::legacy::Client<UnixConnector, Empty<Bytes>> =
                    hyper_util::client::legacy::Client::unix();
                let uri: hyper::Uri = HyperlocalUri::new(sock, path).into();
                let req = Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Empty::<Bytes>::new())
                    .map_err(|e| TotsukactlError::Health(format!("build req: {e}")))?;
                let resp = client
                    .request(req)
                    .await
                    .map_err(|e| TotsukactlError::Health(format!("{name} {path}: {e}")))?;
                let code = resp.status().as_u16();
                let _ = resp.into_body().collect().await;
                Ok(code)
            }
            Endpoint::Tcp(addr) => {
                let url = format!("http://{addr}{path}");
                let resp = reqwest::Client::new()
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                    .map_err(|e| TotsukactlError::Health(format!("{name} {url}: {e}")))?;
                Ok(resp.status().as_u16())
            }
        }
    }
}

#[async_trait]
impl HealthProbe for HttpHealthProbe {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(self.hit(name, "/healthz").await? == 200)
    }
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(self.hit(name, "/readyz").await? == 200)
    }
}

#[derive(Default)]
pub struct MockHealthProbe {
    pub healthy: Mutex<HashMap<String, bool>>,
    pub ready: Mutex<HashMap<String, bool>>,
}

impl MockHealthProbe {
    pub fn set_healthy(&self, name: &str, v: bool) {
        self.healthy.lock().unwrap().insert(name.into(), v);
    }
    pub fn set_ready(&self, name: &str, v: bool) {
        self.ready.lock().unwrap().insert(name.into(), v);
    }
}

#[async_trait]
impl HealthProbe for MockHealthProbe {
    async fn healthz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(*self.healthy.lock().unwrap().get(name).unwrap_or(&true))
    }
    async fn readyz(&self, name: &str) -> Result<bool, TotsukactlError> {
        Ok(*self.ready.lock().unwrap().get(name).unwrap_or(&true))
    }
}
