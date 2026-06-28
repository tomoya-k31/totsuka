use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("adapter: {0}")]
    Adapter(String),
    #[error("writeback: {0}")]
    Writeback(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl OrchestratorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Adapter(_) => "/errors/adapter",
            Self::Writeback(_) => "/errors/writeback",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::RepoNotRegistered(_) => "/errors/repo_not_registered",
            Self::Conflict(_) => "/errors/conflict",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_oor_codes_correctly() {
        let e = OrchestratorError::SchemaOutOfRange {
            got: 3,
            min: 5,
            target: 6,
        };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
    #[test]
    fn conflict_codes() {
        assert_eq!(
            OrchestratorError::Conflict("x".into()).code(),
            "/errors/conflict"
        );
    }
}
