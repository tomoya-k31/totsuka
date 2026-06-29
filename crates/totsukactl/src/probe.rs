//! Phase -1 (pgmq container) + Phase 0 (config / schema / herdr) preflight (spec §4).

use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::schema_check::check_schema_version;
use sqlx::PgPool;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;

pub struct Preflight<'a> {
    pub compose: Arc<dyn ComposeExec>,
    pub cfg: &'a totsuka_config::Config,
    pub paths: &'a Paths,
}

impl<'a> Preflight<'a> {
    pub async fn run_phase_minus1(&self, recreate: bool) -> Result<(), TotsukactlError> {
        self.compose.docker_info().await?;
        self.compose.compose_version().await?;
        let running = self.compose.ps_running("pgmq").await?;
        if !running {
            self.compose.up_detached("pgmq", recreate).await?;
        }
        ensure_image_match(
            self.compose.as_ref(),
            &self.cfg.postgres.container,
            &self.cfg.postgres.image,
            self.cfg.supervisor.recreate_on_image_mismatch,
        )
        .await
    }

    pub async fn run_phase_0(
        &self,
        pool: &PgPool,
        herdr_socket: &Path,
    ) -> Result<(), TotsukactlError> {
        let extv = pgmq_extversion(pool).await?;
        if !pgmq_compatible(&extv, "1.11.1") {
            return Err(TotsukactlError::Probe(format!(
                "pgmq extension version {extv} incompatible with expected 1.11.x"
            )));
        }
        check_schema_version(pool).await?;
        herdr_socket_ping(herdr_socket).await?;
        Ok(())
    }
}

pub async fn pgmq_extversion(pool: &PgPool) -> Result<String, TotsukactlError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT extversion FROM pg_extension WHERE extname='pgmq'")
            .fetch_optional(pool)
            .await?;
    row.map(|(v,)| v)
        .ok_or_else(|| TotsukactlError::Probe("pgmq extension not installed".into()))
}

/// Major.Minor must match `want`; patch ignored.
pub fn pgmq_compatible(extversion: &str, want: &str) -> bool {
    fn major_minor(v: &str) -> Option<(u32, u32)> {
        let mut it = v.split('.');
        let maj = it.next()?.parse().ok()?;
        let min = it.next()?.parse().ok()?;
        Some((maj, min))
    }
    match (major_minor(extversion), major_minor(want)) {
        (Some(g), Some(w)) => g == w,
        _ => false,
    }
}

pub async fn herdr_socket_ping(path: &Path) -> Result<(), TotsukactlError> {
    tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(path))
        .await
        .map_err(|_| TotsukactlError::Probe(format!("herdr socket {path:?}: connect timeout")))?
        .map_err(|e| TotsukactlError::Probe(format!("herdr socket {path:?}: {e}")))?;
    Ok(())
}

pub async fn ensure_image_match(
    compose: &dyn ComposeExec,
    container: &str,
    want_image: &str,
    recreate_allowed: bool,
) -> Result<(), TotsukactlError> {
    let got = compose.inspect_image(container).await?;
    if got == want_image {
        return Ok(());
    }
    if recreate_allowed {
        compose.up_detached("pgmq", true).await?;
        return Ok(());
    }
    Err(TotsukactlError::Probe(format!(
        "pgmq image mismatch: running={got} expected={want_image} (run `docker compose pull && totsukactl up --recreate`)"
    )))
}
