//! NDJSON-over-Unix-domain-socket herdr client. herdr answers exactly one
//! request per connection and then closes it, so each call dials the
//! socket fresh (herdr's load is low: a few calls per second total).
//! Protocol shapes were captured against a real herdr on 2026-07-03 —
//! string request ids, typed result envelopes (`agent_started`,
//! `pane_read`, `agent_list`, `agent_info`, `ok`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{AgentId, HerdrClient, HerdrError, ListItem, PaneSnapshot, SpawnRequest, SpawnResult};

#[derive(Serialize)]
struct Request<'a, P: Serialize> {
    // herdr rejects non-string ids with invalid_request.
    id: String,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct Response {
    #[allow(dead_code)]
    id: String,
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

/// The subset of herdr's agent object we consume. Every typed result
/// envelope (`agent_started`, `agent_info`, `agent_list`) embeds it.
#[derive(Deserialize)]
struct AgentObj {
    terminal_id: String,
    name: String,
    workspace_id: String,
}

#[derive(Deserialize)]
struct AgentStartedResult {
    agent: AgentObj,
}

#[derive(Deserialize)]
struct WorkspaceCreatedResult {
    workspace: WorkspaceObj,
}

#[derive(Deserialize)]
struct WorkspaceObj {
    workspace_id: String,
}

#[derive(Deserialize)]
struct AgentInfoResult {
    agent: AgentObj,
}

#[derive(Deserialize)]
struct AgentListResult {
    agents: Vec<AgentObj>,
}

#[derive(Deserialize)]
struct PaneReadResult {
    read: PaneReadObj,
}

#[derive(Deserialize)]
struct PaneReadObj {
    text: String,
    revision: u64,
}

pub struct WireHerdr {
    // herdr answers exactly one request per connection and then closes it,
    // so every call dials the socket fresh.
    socket_path: std::path::PathBuf,
    next_id: AtomicU64,
}

impl WireHerdr {
    pub async fn connect(socket_path: &Path) -> Result<Self, HerdrError> {
        // Probe once so startup still fails fast when herdr isn't there;
        // the actual RPC connections are dialed per call.
        let _probe = UnixStream::connect(socket_path).await?;
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, HerdrError> {
        let id = format!("totsuka-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let req = Request { id, method, params };
        let line =
            serde_json::to_string(&req).map_err(|e| HerdrError::Decode(format!("encode: {e}")))?;

        let stream = UnixStream::connect(&self.socket_path).await?;
        let (rd, mut wr) = stream.into_split();
        wr.write_all(line.as_bytes()).await?;
        wr.write_all(b"\n").await?;
        wr.flush().await?;

        let mut reader = BufReader::new(rd);
        let mut buf = String::new();
        let n = reader.read_line(&mut buf).await?;
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
        // herdr calls the pane label `name`.
        // One workspace per task: the agent pane lives in its own herdr
        // workspace labelled with the task, and the workspace's root shell
        // opens in the worktree for easy manual intervention. The root
        // shell gets the same env as the agent — without it the shell's
        // mise hook can't trust the worktree's mise.toml and greets the
        // user with mise ERROR (verified against a live herdr: the env
        // param propagates to the root pane's shell).
        #[derive(Serialize)]
        struct Ws<'a> {
            label: &'a str,
            cwd: &'a str,
            env: &'a std::collections::HashMap<String, String>,
        }
        let ws: WorkspaceCreatedResult = self
            .call(
                "workspace.create",
                Ws {
                    label: &req.label,
                    cwd: &req.cwd,
                    env: &req.env,
                },
            )
            .await?;
        // NB: the RPC param is `workspace_id` (unlike the CLI's --workspace);
        // an unknown `workspace` key is silently ignored and the agent lands
        // in whichever workspace happens to be focused.
        #[derive(Serialize)]
        struct P<'a> {
            name: &'a str,
            cwd: &'a str,
            argv: &'a [String],
            env: &'a std::collections::HashMap<String, String>,
            workspace_id: &'a str,
        }
        let res: AgentStartedResult = self
            .call(
                "agent.start",
                P {
                    name: &req.label,
                    cwd: &req.cwd,
                    argv: &req.argv,
                    env: &req.env,
                    workspace_id: &ws.workspace.workspace_id,
                },
            )
            .await?;
        // Guard: if the agent landed anywhere but the workspace we just
        // created, fail loudly now — otherwise close() would later tear
        // down whichever workspace the user happened to have focused.
        if res.agent.workspace_id != ws.workspace.workspace_id {
            return Err(HerdrError::Decode(format!(
                "agent landed in workspace {} instead of the created {} — \
                 herdr ignored workspace_id?",
                res.agent.workspace_id, ws.workspace.workspace_id
            )));
        }
        Ok(SpawnResult {
            agent_id: AgentId::new(res.agent.terminal_id.clone()),
            terminal_id: res.agent.terminal_id,
        })
    }

    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            target: &'a str,
            text: &'a str,
        }
        let _: Value = self
            .call(
                "agent.send",
                P {
                    target: id.as_str(),
                    text,
                },
            )
            .await?;
        Ok(())
    }

    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError> {
        #[derive(Serialize)]
        struct P<'a> {
            target: &'a str,
            source: &'a str,
        }
        let res: PaneReadResult = self
            .call(
                "agent.read",
                P {
                    target: id.as_str(),
                    source: "visible",
                },
            )
            .await?;
        Ok(PaneSnapshot {
            revision: res.read.revision,
            text: res.read.text,
        })
    }

    async fn close(&self, id: &AgentId) -> Result<(), HerdrError> {
        // Every totsuka agent lives in its own workspace (see start), so
        // closing tears down the whole workspace — agent pane and root
        // shell alike. Resolve it via agent.get; herdr has no
        // agent-addressed close.
        #[derive(Serialize)]
        struct Get<'a> {
            target: &'a str,
        }
        let info: AgentInfoResult = self
            .call(
                "agent.get",
                Get {
                    target: id.as_str(),
                },
            )
            .await?;
        #[derive(Serialize)]
        struct Close<'a> {
            workspace_id: &'a str,
        }
        let _: Value = self
            .call(
                "workspace.close",
                Close {
                    workspace_id: &info.agent.workspace_id,
                },
            )
            .await?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ListItem>, HerdrError> {
        #[derive(Serialize)]
        struct P {}
        let res: AgentListResult = self.call("agent.list", P {}).await?;
        Ok(res
            .agents
            .into_iter()
            .map(|a| ListItem {
                agent_id: AgentId::new(a.terminal_id),
                label: a.name,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{HerdrClient, SpawnRequest};
    use std::collections::HashMap;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::unix::OwnedWriteHalf;
    use tokio::net::UnixListener;

    /// Accept connections until one carries a request line. `connect()`'s
    /// probe connection sends nothing and is skipped — the real herdr sees
    /// the same thing. Returns the parsed request and the write half for
    /// the reply; the connection closes when the half is dropped, matching
    /// herdr's one-request-per-connection behaviour.
    async fn next_request(listener: &UnixListener) -> (serde_json::Value, OwnedWriteHalf) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, wr) = stream.into_split();
            let mut lines = BufReader::new(rd).lines();
            if let Some(line) = lines.next_line().await.unwrap() {
                return (serde_json::from_str(&line).unwrap(), wr);
            }
        }
    }

    async fn reply(mut wr: OwnedWriteHalf, body: serde_json::Value) {
        wr.write_all(format!("{}\n", body).as_bytes())
            .await
            .unwrap();
    }

    fn agent_obj(terminal_id: &str, name: &str, workspace_id: &str) -> serde_json::Value {
        let pane_id = format!("{workspace_id}:p2");
        serde_json::json!({
            "terminal_id": terminal_id,
            "name": name,
            "agent_status": "unknown",
            "workspace_id": workspace_id,
            "tab_id": format!("{workspace_id}:t1"),
            "pane_id": pane_id,
            "focused": false,
            "cwd": "/w",
            "foreground_cwd": "/w",
            "revision": 0
        })
    }

    /// Fake herdr speaking the REAL protocol (captured from herdr on
    /// 2026-07-03): string request ids, `name` (not `label`) in
    /// `agent.start` params, and typed result envelopes. Each task gets its
    /// own workspace: `workspace.create` first, then `agent.start` into it.
    #[tokio::test]
    async fn start_creates_workspace_then_starts_agent_inside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "workspace.create");
            assert!(
                req["id"].is_string(),
                "herdr requires string request ids, got {}",
                req["id"]
            );
            assert_eq!(req["params"]["label"], "lbl");
            assert_eq!(
                req["params"]["cwd"], "/w",
                "workspace root shell should open in the worktree"
            );
            assert_eq!(
                req["params"]["env"]["MISE_TRUSTED_CONFIG_PATHS"], "/w",
                "the root shell needs the spawn env too (e.g. mise trust); \
                 without it the workspace's root pane greets the user with \
                 mise ERROR"
            );
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "workspace_created",
                        "workspace": {
                            "workspace_id": "w9",
                            "number": 3,
                            "label": "lbl",
                            "focused": false,
                            "pane_count": 1,
                            "tab_count": 1,
                            "active_tab_id": "w9:t1",
                            "agent_status": "unknown"
                        },
                        "tab": { "tab_id": "w9:t1", "workspace_id": "w9", "number": 1,
                                 "label": "1", "focused": false, "pane_count": 1,
                                 "agent_status": "unknown" },
                        "root_pane": agent_obj("term_root", "", "w9")
                    },
                }),
            )
            .await;

            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "agent.start");
            assert_eq!(req["params"]["cwd"], "/w");
            assert_eq!(
                req["params"]["name"], "lbl",
                "herdr wants the label under `name`"
            );
            assert_eq!(
                req["params"]["workspace_id"], "w9",
                "agent must start inside the workspace created for the task \
                 (RPC param is workspace_id — `workspace` is silently ignored \
                 and the agent lands in whichever workspace is focused)"
            );
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "agent_started",
                        "agent": agent_obj("term_42", "lbl", "w9"),
                        "argv": ["claude"]
                    },
                }),
            )
            .await;
        });

        let client = WireHerdr::connect(&sock).await.unwrap();
        let res = client
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::from_iter([(
                    "MISE_TRUSTED_CONFIG_PATHS".to_string(),
                    "/w".to_string(),
                )]),
                label: "lbl".into(),
            })
            .await
            .unwrap();
        assert_eq!(res.agent_id.as_str(), "term_42");
        server.await.unwrap();
    }

    /// If herdr places the agent somewhere other than the workspace we
    /// created (e.g. a future param rename ignored again), `start` must
    /// fail loudly — otherwise `close` would later tear down whichever
    /// workspace the user happened to have focused.
    #[tokio::test]
    async fn start_errors_when_agent_lands_outside_created_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "workspace.create");
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "workspace_created",
                        "workspace": { "workspace_id": "w9" },
                        "root_pane": agent_obj("term_root", "", "w9")
                    },
                }),
            )
            .await;

            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "agent.start");
            // herdr "succeeds" but the agent landed in the focused
            // workspace w1, not the requested w9.
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "agent_started",
                        "agent": agent_obj("term_42", "lbl", "w1"),
                        "argv": ["claude"]
                    },
                }),
            )
            .await;
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let err = client
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                label: "lbl".into(),
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("w9") && err.to_string().contains("w1"),
            "error should name both workspaces, got: {err}"
        );
        server.await.unwrap();
    }

    /// `agent.read` (target-addressed) returns a `pane_read` envelope; the
    /// snapshot must be lifted out of `result.read`.
    #[tokio::test]
    async fn read_uses_agent_read_target_and_unwraps_pane_read_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "agent.read");
            assert_eq!(req["params"]["target"], "term_42");
            assert!(req["params"]["source"].is_string(), "source is required");
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "pane_read",
                        "read": {
                            "pane_id": "w1:p2",
                            "workspace_id": "w1",
                            "tab_id": "w1:t1",
                            "source": "visible",
                            "format": "text",
                            "text": "hello\n",
                            "revision": 7,
                            "truncated": false
                        }
                    },
                }),
            )
            .await;
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let snap = client
            .read(&super::super::AgentId::new("term_42".into()))
            .await
            .unwrap();
        assert_eq!(snap.text, "hello\n");
        assert_eq!(snap.revision, 7);
        server.await.unwrap();
    }

    /// `close` must resolve the pane via `agent.get` first — herdr has no
    /// agent-addressed close; `pane.close` wants a `pane_id`.
    #[tokio::test]
    async fn close_resolves_workspace_via_agent_get_then_closes_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "agent.get");
            assert_eq!(req["params"]["target"], "term_42");
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "agent_info",
                        "agent": agent_obj("term_42", "lbl", "w1"),
                    },
                }),
            )
            .await;

            // Every totsuka agent lives in its own workspace, so close tears
            // down the whole workspace (root shell included), not just the pane.
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "workspace.close");
            assert_eq!(req["params"]["workspace_id"], "w1");
            reply(
                wr,
                serde_json::json!({ "id": req["id"], "result": { "type": "ok" } }),
            )
            .await;
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        client
            .close(&super::super::AgentId::new("term_42".into()))
            .await
            .unwrap();
        server.await.unwrap();
    }

    /// `agent.list` wraps the items in `{type: "agent_list", agents: [...]}`.
    #[tokio::test]
    async fn list_unwraps_agent_list_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            assert_eq!(req["method"], "agent.list");
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "result": {
                        "type": "agent_list",
                        "agents": [agent_obj("term_1", "totsuka:abc:impl", "w1")]
                    },
                }),
            )
            .await;
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let items = client.list().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].agent_id.as_str(), "term_1");
        assert_eq!(items[0].label, "totsuka:abc:impl");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_error_is_propagated() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (req, wr) = next_request(&listener).await;
            reply(
                wr,
                serde_json::json!({
                    "id": req["id"],
                    "error": { "code": "capacity", "message": "no slots" },
                }),
            )
            .await;
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        let err = client.list().await.unwrap_err();
        assert!(
            matches!(err, super::super::HerdrError::Remote { ref code, .. } if code == "capacity")
        );
        server.await.unwrap();
    }

    /// The real herdr answers exactly one request per connection, then
    /// closes it. Sequential calls on one client must keep working.
    #[tokio::test]
    async fn sequential_calls_survive_per_request_connection_close() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (req, wr) = next_request(&listener).await;
                assert_eq!(req["method"], "agent.list");
                reply(
                    wr,
                    serde_json::json!({
                        "id": req["id"],
                        "result": { "type": "agent_list", "agents": [] },
                    }),
                )
                .await;
            }
        });
        let client = WireHerdr::connect(&sock).await.unwrap();
        assert!(client.list().await.unwrap().is_empty());
        assert!(
            client.list().await.unwrap().is_empty(),
            "second call must dial a fresh connection"
        );
        server.await.unwrap();
    }
}
