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
            Err(e) if attempt < CREATE_RETRIES => {
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

/// Drop leftover test databases from earlier (possibly panicked) runs.
/// Best-effort: any error is logged and ignored — sweeping must never be
/// able to fail a test.
async fn sweep_stale(admin: &PgPool) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs();
    let names: Vec<(String,)> =
        match sqlx::query_as("SELECT datname FROM pg_database WHERE datname LIKE 'totsuka_test_%'")
            .fetch_all(admin)
            .await
        {
            Ok(v) => v,
            Err(_) => return,
        };
    for (name,) in names {
        // totsuka_test_<unix>_<id> — segment 2 is the timestamp.
        let Some(ts) = name.split('_').nth(2).and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if now.saturating_sub(ts) < STALE_AFTER_SECS {
            continue;
        }
        if let Err(e) = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .execute(admin)
            .await
        {
            tracing_or_eprintln(&format!("testkit: sweep of {name} failed: {e}"));
        }
    }
}

/// Swap the database segment of a postgres URL, keeping any query string.
fn replace_db_name(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let cut = base.rfind('/').expect("postgres url has a path segment");
    match query {
        Some(q) => format!("{}/{}?{}", &base[..cut], db, q),
        None => format!("{}/{}", &base[..cut], db),
    }
}

fn tracing_or_eprintln(msg: &str) {
    eprintln!("{msg}");
}

#[cfg(test)]
mod tests {
    use super::replace_db_name;

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
}
