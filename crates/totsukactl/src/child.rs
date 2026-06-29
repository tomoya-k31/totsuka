use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

pub mod mock;
pub mod spec;

pub use spec::{specs_from_config, ChildSpec};

#[async_trait]
pub trait ChildSpawner: Send + Sync {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError>;
}

pub struct ForkExecSpawner;

#[async_trait]
impl ChildSpawner for ForkExecSpawner {
    async fn spawn(&self, spec: &ChildSpec) -> Result<i32, TotsukactlError> {
        if let Some(parent) = spec.log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.log_path)?;
        let stderr = stdout.try_clone()?;
        let mut cmd = Command::new(&spec.bin_path);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .stdin(Stdio::null());
        let child = cmd.spawn().map_err(|e| {
            TotsukactlError::Spawn(format!("spawn {}: {e}", spec.bin_path.display()))
        })?;
        child
            .id()
            .map(|pid| pid as i32)
            .ok_or_else(|| TotsukactlError::Spawn(format!("{} pid unavailable", spec.name)))
    }
}
