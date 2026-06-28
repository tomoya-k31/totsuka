use thiserror::Error;

#[derive(Debug, Error)]
pub enum QaError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket: {0}")]
    WebSocket(String),
    #[error("adapter: {0}")]
    Adapter(String),
    #[error("classifier: {0}")]
    Classifier(String),
    #[error("slack: {0}")]
    Slack(String),
    #[error("graphql: {0}")]
    GraphQl(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl QaError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Http(_) => "/errors/http",
            Self::WebSocket(_) => "/errors/websocket",
            Self::Adapter(_) => "/errors/adapter",
            Self::Classifier(_) => "/errors/classifier",
            Self::Slack(_) => "/errors/slack",
            Self::GraphQl(_) => "/errors/graphql",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::RepoNotRegistered(_) => "/errors/repo_not_registered",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn schema_oor_codes() {
        assert_eq!(QaError::SchemaOutOfRange { got: 3, min: 6, target: 6 }.code(), "/errors/schema_out_of_range");
    }
    #[test] fn websocket_codes() {
        assert_eq!(QaError::WebSocket("drop".into()).code(), "/errors/websocket");
    }
    #[test] fn classifier_codes() {
        assert_eq!(QaError::Classifier("provider 500".into()).code(), "/errors/classifier");
    }
}
