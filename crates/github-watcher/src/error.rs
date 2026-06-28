use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("bus: {0}")]
    Bus(#[from] totsuka_bus::pgmq::BusError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("graphql: {0}")]
    GraphQl(String),
    #[error("rate limited until {reset_at}")]
    RateLimited { reset_at: DateTime<Utc> },
    #[error("schema out of range (got={got}, want=[{min}..{target}])")]
    SchemaOutOfRange { got: i32, min: i32, target: i32 },
    #[error("column map: {0}")]
    ColumnMap(String),
    #[error("unknown column display: {0}")]
    UnknownColumn(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl WatcherError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sqlx(_) => "/errors/sqlx",
            Self::Bus(_) => "/errors/bus",
            Self::Http(_) => "/errors/http",
            Self::GraphQl(_) => "/errors/graphql",
            Self::RateLimited { .. } => "/errors/rate_limited",
            Self::SchemaOutOfRange { .. } => "/errors/schema_out_of_range",
            Self::ColumnMap(_) => "/errors/column_map",
            Self::UnknownColumn(_) => "/errors/unknown_column",
            Self::Internal(_) => "/errors/internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn schema_oor_codes() {
        let e = WatcherError::SchemaOutOfRange { got: 3, min: 6, target: 6 };
        assert_eq!(e.code(), "/errors/schema_out_of_range");
    }
    #[test]
    fn rate_limited_codes() {
        let t = Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap();
        let e = WatcherError::RateLimited { reset_at: t };
        assert_eq!(e.code(), "/errors/rate_limited");
    }
    #[test]
    fn unknown_column_codes() {
        assert_eq!(
            WatcherError::UnknownColumn("🚧 ???".into()).code(),
            "/errors/unknown_column"
        );
    }
}
