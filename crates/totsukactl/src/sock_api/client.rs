use super::dto::{ProcessDto, ShutdownReq};
use crate::error::TotsukactlError;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::Request;
use hyperlocal::{UnixClientExt, UnixConnector, Uri as HyperlocalUri};
use std::path::PathBuf;

pub struct SupervisorClient {
    pub sock: PathBuf,
}

impl SupervisorClient {
    pub fn new(sock: PathBuf) -> Self {
        Self { sock }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, TotsukactlError> {
        let client: hyper_util::client::legacy::Client<UnixConnector, Empty<Bytes>> =
            hyper_util::client::legacy::Client::unix();
        let uri: hyper::Uri = HyperlocalUri::new(&self.sock, path).into();
        let req = Request::get(uri)
            .body(Empty::<Bytes>::new())
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("build {path}: {e}")))?;
        let resp = client
            .request(req)
            .await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("{path}: {e}")))?;
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("read {path}: {e}")))?
            .to_bytes();
        serde_json::from_slice(&bytes)
            .map_err(|e| TotsukactlError::Internal(format!("decode {path}: {e}")))
    }

    async fn post_json<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<(), TotsukactlError> {
        let json = serde_json::to_vec(body)
            .map_err(|e| TotsukactlError::Internal(format!("encode {path}: {e}")))?;
        let client: hyper_util::client::legacy::Client<UnixConnector, Full<Bytes>> =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(UnixConnector);
        let uri: hyper::Uri = HyperlocalUri::new(&self.sock, path).into();
        let req = Request::post(uri)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(json)))
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("build {path}: {e}")))?;
        let resp = client
            .request(req)
            .await
            .map_err(|e| TotsukactlError::SupervisorUnreachable(format!("{path}: {e}")))?;
        if !resp.status().is_success() {
            return Err(TotsukactlError::SupervisorUnreachable(format!(
                "{path}: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<ProcessDto>, TotsukactlError> {
        self.get_json("/v1/processes").await
    }

    pub async fn restart(&self, name: &str) -> Result<(), TotsukactlError> {
        self.post_json(
            &format!("/v1/processes/{name}/restart"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn reload(&self, name: &str) -> Result<(), TotsukactlError> {
        self.post_json(
            &format!("/v1/processes/{name}/reload"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn shutdown(&self, postgres: bool, force: bool) -> Result<(), TotsukactlError> {
        self.post_json("/v1/shutdown", &ShutdownReq { postgres, force })
            .await
    }
}
