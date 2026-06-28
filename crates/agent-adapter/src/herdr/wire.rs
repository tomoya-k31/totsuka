//! NDJSON-over-Unix-domain-socket herdr client. See module-level doc on
//! `mod.rs` for the wire envelope. Single multiplexed connection per
//! `WireHerdr` instance; one request at a time (herdr's load is low: a few
//! calls per second total across all panes).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use super::{AgentId, HerdrClient, HerdrError, ListItem, PaneSnapshot, SpawnRequest, SpawnResult};

#[derive(Serialize)]
struct Request<'a, P: Serialize> {
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct Response {
    #[allow(dead_code)]
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
}

pub struct WireHerdr {
    // Mutex serialises in-flight calls. Single-connection is fine for current
    // herdr load (a few RPC/sec total).
    conn: Mutex<Conn>,
    next_id: AtomicU64,
}

struct Conn {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl WireHerdr {
    pub async fn connect(socket_path: &Path) -> Result<Self, HerdrError> {
        let stream = UnixStream::connect(socket_path).await?;
        let (rd, wr) = stream.into_split();
        Ok(Self {
            conn: Mutex::new(Conn {
                reader: BufReader::new(rd),
                writer: wr,
            }),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, HerdrError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request { id, method, params };
        let line =
            serde_json::to_string(&req).map_err(|e| HerdrError::Decode(format!("encode: {e}")))?;

        let mut conn = self.conn.lock().await;
        conn.writer.write_all(line.as_bytes()).await?;
        conn.writer.write_all(b"\n").await?;
        conn.writer.flush().await?;

        let mut buf = String::new();
        let n = conn.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(HerdrError::Decode("herdr closed connection".into()));
        }
        let resp: Response =
            serde_json::from_str(buf.trim_end()).map_err(|e| HerdrError::Decode(e.to_string()))?;
        if let Some(err) = resp.error {
            return Err(HerdrError::Remote {
                code: err.code,
                message: err.message,
            });
        }
        let result = resp
            .result
            .ok_or_else(|| HerdrError::Decode("missing result".into()))?;
        serde_json::from_value(result).map_err(|e| HerdrError::Decode(e.to_string()))
    }
}

#[async_trait]
impl HerdrClient for WireHerdr {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError> {
        self.call("agent.start", req).await
    }

    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
            text: &'a str,
        }
        let _: Value = self
            .call(
                "agent.send",
                P {
                    agent_id: id.as_str(),
                    text,
                },
            )
            .await?;
        Ok(())
    }

    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
        }
        self.call(
            "pane.read",
            P {
                agent_id: id.as_str(),
            },
        )
        .await
    }

    async fn close(&self, id: &AgentId) -> Result<(), HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            agent_id: &'a str,
        }
        let _: Value = self
            .call(
                "pane.close",
                P {
                    agent_id: id.as_str(),
                },
            )
            .await?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ListItem>, HerdrError> {
        #[derive(Serialize)]
        struct P {}
        self.call("agent.list", P {}).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{HerdrClient, SpawnRequest};
    use std::collections::HashMap;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Spin up a fake herdr that responds to `agent.start` with a canned result.
    #[tokio::test]
    async fn start_sends_jsonrpc_envelope_and_parses_result() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        // Spawn a one-shot fake server
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&req_line).unwrap();
            assert_eq!(req["method"], "agent.start");
            assert_eq!(req["params"]["cwd"], "/w");
            let reply = serde_json::json!({
                "id": req["id"],
                "result": { "agent_id": "ag_42", "terminal_id": "t_42" },
            });
            wr.write_all(format!("{}\n", reply).as_bytes())
                .await
                .unwrap();
        });

        let client = WireHerdr::connect(&sock).await.unwrap();
        let res = client
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                label: "lbl".into(),
            })
            .await
            .unwrap();
        assert_eq!(res.agent_id.as_str(), "ag_42");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_error_is_propagated() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&req_line).unwrap();
            let reply = serde_json::json!({
                "id": req["id"],
                "error": { "code": "capacity", "message": "no slots" },
            });
            wr.write_all(format!("{}\n", reply).as_bytes())
                .await
                .unwrap();
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let err = client.list().await.unwrap_err();
        assert!(
            matches!(err, super::super::HerdrError::Remote { ref code, .. } if code == "capacity")
        );
        server.await.unwrap();
    }
}
