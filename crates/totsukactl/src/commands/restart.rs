use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::registry::ORDER;
use crate::sock_api::SupervisorClient;

pub async fn run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError> {
    if bin == "pgmq" {
        return Err(TotsukactlError::Config(
            "restarting pgmq is forbidden (data integrity); use docker compose manually".into(),
        ));
    }
    if !ORDER.contains(&bin) {
        return Err(TotsukactlError::UnknownChild(bin.into()));
    }
    let client = SupervisorClient::new(paths.supervisor_sock());
    match client.restart(bin).await {
        Ok(()) => Ok(()),
        Err(TotsukactlError::SupervisorUnreachable(_)) => Err(TotsukactlError::NotRunning),
        Err(e) => Err(e),
    }
}
