#![forbid(unsafe_code)]
//! Test-only support crate. `ephemeral_db()` gives every DB test its own
//! throwaway database on the server `DATABASE_URL` points at, so tests can
//! never pollute the database itself — which is the dev stack's live DB on
//! local machines (spec: docs/superpowers/specs/2026-07-03-testkit-ephemeral-db-design.md).

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Test databases whose embedded timestamp is older than this are dropped
/// opportunistically by the next `ephemeral_db()` call. Long enough that a
/// running `cargo test` invocation (~1 min for the workspace) can never
/// have its databases swept out from under it, short enough that heavy
/// local iteration doesn't pile up hundreds of leftover databases.
const STALE_AFTER_SECS: u64 = 10 * 60;

/// CREATE DATABASE copies template1 and fails with 55006 when another
/// session is doing the same concurrently — normal when parallel test
/// binaries start up. Retry a bounded number of times.
const CREATE_RETRIES: u32 = 10;

/// A migrated, isolated database that lives for the duration of a test.
/// No Drop cleanup — stale databases are swept by later `ephemeral_db()`
/// calls (see `STALE_AFTER_SECS`).
pub struct EphemeralDb {
    pub pool: PgPool,
    url: String,
    name: String,
}

impl EphemeralDb {
    /// Connection URL of the ephemeral database — hand this to spawned
    /// binaries as their `DATABASE_URL`.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// `None` when `DATABASE_URL` is unset — callers keep the existing
/// "silently skip without Postgres" convention. Panics on infrastructure
/// errors (a reachable server that fails to create a DB is a bug worth
/// failing loudly on, not skipping).
pub async fn ephemeral_db() -> Option<EphemeralDb> {
    let admin_url = std::env::var("DATABASE_URL").ok()?;
    Some(
        create_from(&admin_url)
            .await
            .expect("totsuka-testkit: failed to create ephemeral database"),
    )
}

/// Same as [`ephemeral_db`] with an explicit admin URL (testable without
/// mutating process-global env).
pub async fn create_from(admin_url: &str) -> Result<EphemeralDb, sqlx::Error> {
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await?;

    sweep_stale(&admin).await;

    // Test-only code: the production Clock convention doesn't apply here,
    // and the wall-clock second is part of the sweep protocol.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("totsuka_test_{}_{}", ts, &id[..8]);

    let mut attempt = 0;
    loop {
        match sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
        {
            Ok(_) => break,
            Err(e) if attempt < CREATE_RETRIES && is_concurrent_template_copy(&e) => {
                attempt += 1;
                tracing_or_eprintln(&format!("testkit: CREATE DATABASE retry {attempt}: {e}"));
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            Err(e) => return Err(e),
        }
    }

    let url = replace_db_name(admin_url, &name);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("totsuka-testkit: migrations failed on ephemeral database");

    Ok(EphemeralDb { pool, url, name })
}

/// SQLSTATE 55006: another session is copying the same template database.
/// The only condition worth retrying — anything else is a real failure.
pub fn is_concurrent_template_copy(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c == "55006")
        .unwrap_or(false)
}

/// Drop leftover test databases from earlier (possibly panicked) runs.
/// Best-effort: any error is logged and ignored — sweeping must never be
/// able to fail a test. Databases with live connections are skipped:
/// finished or panicked runs hold none, so only genuinely in-use
/// databases (e.g. a long local debugging session) survive the window.
async fn sweep_stale(admin: &PgPool) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs();
    let names: Vec<(String,)> = match sqlx::query_as(
        "SELECT datname FROM pg_database d
         WHERE datname LIKE 'totsuka_test_%'
           AND NOT EXISTS (
               SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname
           )",
    )
    .fetch_all(admin)
    .await
    {
        Ok(v) => v,
        Err(_) => return,
    };
    for (name,) in names {
        if !is_sweepable_name(&name) {
            continue;
        }
        // totsuka_test_<unix>_<id> — segment 2 is the timestamp.
        let Some(ts) = name.split('_').nth(2).and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if now.saturating_sub(ts) < STALE_AFTER_SECS {
            continue;
        }
        // No FORCE: if something connected between the query above and
        // here, the drop fails and the db is retried on a later sweep.
        if let Err(e) = sqlx::query(&format!("DROP DATABASE IF EXISTS {name}"))
            .execute(admin)
            .await
        {
            tracing_or_eprintln(&format!("testkit: sweep of {name} failed: {e}"));
        }
    }
}

/// Only names this crate could have generated are eligible for DROP —
/// they get interpolated into SQL, so restrict to the exact shape
/// `totsuka_test_<digits>_<lowercase hex/alnum>` (identifier-safe).
fn is_sweepable_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("totsuka_test_") else {
        return false;
    };
    let Some((ts, id)) = rest.split_once('_') else {
        return false;
    };
    !ts.is_empty()
        && ts.bytes().all(|b| b.is_ascii_digit())
        && !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Swap the database segment of a postgres URL, keeping any query string.
/// URLs without a path segment (`postgres://user@host`) get one appended —
/// the `//` of the scheme is not a path separator.
fn replace_db_name(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let after_authority = base.find("://").map(|i| i + 3).unwrap_or(0);
    let base = match base[after_authority..].find('/') {
        Some(rel) => &base[..after_authority + rel],
        None => base,
    };
    match query {
        Some(q) => format!("{base}/{db}?{q}"),
        None => format!("{base}/{db}"),
    }
}

fn tracing_or_eprintln(msg: &str) {
    eprintln!("{msg}");
}

#[cfg(test)]
mod tests {
    use super::{is_sweepable_name, replace_db_name};

    #[test]
    fn replaces_db_segment_and_keeps_query() {
        assert_eq!(
            replace_db_name("postgres://u:p@h:5432/totsuka", "t_x"),
            "postgres://u:p@h:5432/t_x"
        );
        assert_eq!(
            replace_db_name("postgres://u@h/db?sslmode=disable", "t_x"),
            "postgres://u@h/t_x?sslmode=disable"
        );
    }

    #[test]
    fn appends_db_segment_when_url_has_no_path() {
        // Valid Postgres URLs may omit the database path entirely; the
        // "//" of the scheme must not be mistaken for a path separator.
        assert_eq!(
            replace_db_name("postgres://u@h", "t_x"),
            "postgres://u@h/t_x"
        );
        assert_eq!(
            replace_db_name("postgres://u@h:5432?sslmode=disable", "t_x"),
            "postgres://u@h:5432/t_x?sslmode=disable"
        );
    }

    #[test]
    fn sweep_only_accepts_identifier_safe_generated_names() {
        assert!(is_sweepable_name("totsuka_test_1000000000_deadbeef"));
        // Anything outside [a-z0-9_] must be rejected — names are
        // interpolated into DROP DATABASE statements.
        assert!(!is_sweepable_name("totsuka_test_1_a\"; DROP TABLE x;--"));
        assert!(!is_sweepable_name("totsuka_test_1_日本語"));
        assert!(!is_sweepable_name("totsuka_test__nots"));
    }
}
