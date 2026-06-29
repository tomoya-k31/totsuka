use crate::compose::{ComposeExec, DockerCompose};
use crate::error::TotsukactlError;
use crate::paths::{resolve_tilde, Paths};
use std::path::PathBuf;

const CONFIG_TMPL: &str = include_str!("templates/config.toml.tmpl");
const SECRETS_TMPL: &str = include_str!("templates/secrets.toml.tmpl");

pub async fn run(paths: &Paths) -> Result<(), TotsukactlError> {
    paths.ensure()?;
    let config_dir = config_home().join("totsuka");
    std::fs::create_dir_all(&config_dir)?;

    let cfg_path = config_dir.join("config.toml");
    write_if_absent(&cfg_path, CONFIG_TMPL, 0o644)?;
    let sec_path = config_dir.join("secrets.toml");
    write_if_absent(&sec_path, SECRETS_TMPL, 0o600)?;

    // Read back so we can drive compose + migrate from the user's actual values.
    let cfg = totsuka_config::Config::load(&cfg_path)
        .map_err(|e| TotsukactlError::Config(format!("re-reading freshly written config: {e:?}")))?;
    let compose: std::sync::Arc<dyn ComposeExec> =
        std::sync::Arc::new(DockerCompose::new(PathBuf::from(&cfg.postgres.compose_file)));
    compose.docker_info().await?;
    compose.compose_version().await?;
    if !compose.ps_running("pgmq").await? {
        compose.up_detached("pgmq", false).await?;
    }
    crate::commands::migrate::run(&cfg).await?;

    println!(
        "totsuka initialised. edit {} to add tokens, then run `totsukactl up`.",
        sec_path.display()
    );
    Ok(())
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| resolve_tilde("~/.config"))
}

fn write_if_absent(path: &std::path::Path, body: &str, mode: u32) -> Result<(), TotsukactlError> {
    if path.exists() {
        tracing::warn!(path=?path, "exists; not overwriting");
        return Ok(());
    }
    std::fs::write(path, body)?;
    set_mode(path, mode)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> Result<(), TotsukactlError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> Result<(), TotsukactlError> {
    Ok(())
}
