use async_trait::async_trait;
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyperlocal::UnixConnector;
use std::collections::HashMap;
use std::path::PathBuf;

use super::{AdapterClient, ReadRes, SpawnReq, SpawnRes, WireSpawn};
use crate::error::OrchestratorError;

pub struct HyperlocalAdapter {
    socket: PathBuf,
    client: hyper_util::client::legacy::Client<UnixConnector, http_body_util::Full<Bytes>>,
}

impl HyperlocalAdapter {
    pub fn new(socket: PathBuf) -> Self {
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build::<_, http_body_util::Full<Bytes>>(UnixConnector);
        Self { socket, client }
    }

    async fn call_json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, OrchestratorError> {
        let uri: hyper::Uri = hyperlocal::Uri::new(&self.socket, path).into();
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(Bytes::from(body.to_string())))
            .map_err(|e| OrchestratorError::Adapter(format!("build req: {e}")))?;
        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| OrchestratorError::Adapter(format!("send: {e}")))?;
        let status = resp.status();
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .map_err(|e| OrchestratorError::Adapter(format!("read: {e}")))?
            .to_bytes();
        if !status.is_success() {
            return Err(OrchestratorError::Adapter(format!(
                "{} {}: {}",
                status.as_u16(),
                path,
                String::from_utf8_lossy(&body)
            )));
        }
        if body.is_empty() {
            return serde_json::from_str("null")
                .map_err(|e| OrchestratorError::Adapter(e.to_string()));
        }
        serde_json::from_slice(&body).map_err(|e| OrchestratorError::Adapter(e.to_string()))
    }
}

#[async_trait]
impl AdapterClient for HyperlocalAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, OrchestratorError> {
        let env: HashMap<&str, &str> = req
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose().as_str()))
            .collect();
        let wire = WireSpawn {
            task_id: &req.task_id,
            phase: &req.phase,
            attempt: req.attempt,
            repo: &req.repo,
            branch: &req.branch,
            argv: &req.argv,
            env,
            detached: req.detached,
        };
        let v =
            serde_json::to_value(&wire).map_err(|e| OrchestratorError::Adapter(e.to_string()))?;
        self.call_json(Method::POST, "/v1/agents", v).await
    }

    async fn send(&self, agent_id: &str, text: &str) -> Result<(), OrchestratorError> {
        let _: serde_json::Value = self
            .call_json(
                Method::POST,
                &format!("/v1/agents/{agent_id}/messages"),
                serde_json::json!({ "text": text }),
            )
            .await?;
        Ok(())
    }

    async fn read(
        &self,
        agent_id: &str,
        since_revision: u64,
    ) -> Result<ReadRes, OrchestratorError> {
        let path = format!("/v1/agents/{agent_id}/output?since_revision={since_revision}");
        self.call_json(Method::GET, &path, serde_json::Value::Null)
            .await
    }

    async fn stop(
        &self,
        agent_id: &str,
        repo: &str,
        branch: &str,
    ) -> Result<(), OrchestratorError> {
        let uri: hyper::Uri =
            hyperlocal::Uri::new(&self.socket, &format!("/v1/agents/{agent_id}")).into();
        let req = Request::builder()
            .method(Method::DELETE)
            .uri(uri)
            .header("x-totsuka-repo", repo)
            .header("x-totsuka-branch", branch)
            .body(http_body_util::Full::new(Bytes::new()))
            .map_err(|e| OrchestratorError::Adapter(format!("build delete: {e}")))?;
        let resp = self
            .client
            .request(req)
            .await
            .map_err(|e| OrchestratorError::Adapter(format!("send delete: {e}")))?;
        if !resp.status().is_success() {
            return Err(OrchestratorError::Adapter(format!(
                "delete: {}",
                resp.status()
            )));
        }
        Ok(())
    }
}
