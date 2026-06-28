pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("worktree in use: {0}")]
    WorktreeInUse(String),
    #[error("capacity full")]
    CapacityFull,
    #[error("argv contains secret-like flag")]
    ArgvSecretViolation,
    #[error("schema version out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("config error: {0}")]
    Config(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl Error {
    /// RFC7807 `type` URI (spec §11.6)
    pub fn code(&self) -> &'static str {
        match self {
            Error::RepoNotRegistered(_) => "/errors/repo_not_registered",
            Error::WorktreeInUse(_) => "/errors/worktree_in_use",
            Error::CapacityFull => "/errors/capacity_full",
            Error::ArgvSecretViolation => "/errors/argv_secret_violation",
            Error::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Error::Config(_) => "/errors/config",
            Error::Io(_) => "/errors/io",
            Error::Serde(_) => "/errors/serde",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_for_each_variant_matches_uri_prefix() {
        let e = Error::RepoNotRegistered("x/y".into());
        assert_eq!(e.code(), "/errors/repo_not_registered");
        let e = Error::CapacityFull;
        assert_eq!(e.code(), "/errors/capacity_full");
        let e = Error::SchemaOutOfRange {
            got: 3,
            min: 5,
            target: 7,
        };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
}
