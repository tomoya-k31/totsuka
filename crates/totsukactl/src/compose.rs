use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub mod mock;

#[async_trait]
pub trait ComposeExec: Send + Sync {
    async fn docker_info(&self) -> Result<(), TotsukactlError>;
    async fn compose_version(&self) -> Result<(), TotsukactlError>;
    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError>;
    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError>;
    async fn stop(&self, service: &str) -> Result<(), TotsukactlError>;
    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError>;
    async fn logs_tail(&self, service: &str, n: u32) -> Result<String, TotsukactlError>;
}

pub struct DockerCompose {
    pub compose_file: PathBuf,
}

impl DockerCompose {
    pub fn new(compose_file: PathBuf) -> Self {
        Self { compose_file }
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, TotsukactlError> {
        let out = Command::new("docker")
            .args(args)
            .output()
            .await
            .map_err(|e| TotsukactlError::Compose(format!("spawn docker {args:?}: {e}")))?;
        Ok(out)
    }

    fn ensure_ok(out: &std::process::Output, ctx: &str) -> Result<(), TotsukactlError> {
        if out.status.success() {
            Ok(())
        } else {
            Err(TotsukactlError::Compose(format!(
                "{ctx} failed (code {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }
}

#[async_trait]
impl ComposeExec for DockerCompose {
    async fn docker_info(&self) -> Result<(), TotsukactlError> {
        let out = self.run(&["info"]).await?;
        Self::ensure_ok(&out, "docker info")
    }

    async fn compose_version(&self) -> Result<(), TotsukactlError> {
        let out = self.run(&["compose", "version"]).await?;
        Self::ensure_ok(&out, "docker compose version")
    }

    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let out = self
            .run(&["compose", "-f", &cf, "ps", "--status=running", "--services"])
            .await?;
        Self::ensure_ok(&out, "docker compose ps")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout.lines().any(|l| l.trim() == service))
    }

    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let mut args = vec!["compose", "-f", &cf, "up", "-d"];
        if recreate {
            args.push("--force-recreate");
        }
        args.push(service);
        let out = self.run(&args).await?;
        Self::ensure_ok(&out, "docker compose up -d")
    }

    async fn stop(&self, service: &str) -> Result<(), TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let out = self.run(&["compose", "-f", &cf, "stop", service]).await?;
        Self::ensure_ok(&out, "docker compose stop")
    }

    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError> {
        let out = self
            .run(&["inspect", "--format", "{{.Config.Image}}", container])
            .await?;
        Self::ensure_ok(&out, "docker inspect")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    async fn logs_tail(&self, service: &str, n: u32) -> Result<String, TotsukactlError> {
        let cf = self.compose_file.to_string_lossy().to_string();
        let n_s = n.to_string();
        let out = self
            .run(&["compose", "-f", &cf, "logs", "--tail", &n_s, service])
            .await?;
        // logs returns non-zero if service unknown; treat as Compose error.
        Self::ensure_ok(&out, "docker compose logs")?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
