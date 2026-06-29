use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::sock_api::SupervisorClient;

pub async fn run(paths: &Paths, bin: &str) -> Result<(), TotsukactlError> {
    if bin != "agent-adapter" {
        return Err(TotsukactlError::Config(format!(
            "reload is only meaningful for agent-adapter (spec §6); refusing {bin}"
        )));
    }
    let client = SupervisorClient::new(paths.supervisor_sock());
    match client.reload(bin).await {
        Ok(()) => Ok(()),
        Err(TotsukactlError::SupervisorUnreachable(_)) => Err(TotsukactlError::NotRunning),
        Err(e) => Err(e),
    }
}
