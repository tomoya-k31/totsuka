use thiserror::Error;

#[derive(Debug, Error)]
pub enum TotsukactlError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(String),
    #[error("config: {0}")]
    Config(String),
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migrate: {0}")]
    Migrate(String),
    #[error("compose: {0}")]
    Compose(String),
    #[error("probe: {0}")]
    Probe(String),
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("health: {0}")]
    Health(String),
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("supervisor unreachable: {0}")]
    SupervisorUnreachable(String),
    #[error("stack already running: {0}")]
    AlreadyRunning(String),
    #[error("stack not running")]
    NotRunning,
    #[error("unknown child: {0}")]
    UnknownChild(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl TotsukactlError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "/errors/io",
            Self::Toml(_) => "/errors/toml",
            Self::Config(_) => "/errors/config",
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Migrate(_) => "/errors/migrate",
            Self::Compose(_) => "/errors/compose",
            Self::Probe(_) => "/errors/probe",
            Self::Spawn(_) => "/errors/spawn",
            Self::Health(_) => "/errors/health",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::SupervisorUnreachable(_) => "/errors/supervisor_unreachable",
            Self::AlreadyRunning(_) => "/errors/already_running",
            Self::NotRunning => "/errors/not_running",
            Self::UnknownChild(_) => "/errors/unknown_child",
            Self::Timeout(_) => "/errors/timeout",
            Self::Internal(_) => "/errors/internal",
        }
    }
}
