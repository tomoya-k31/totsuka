//! spec §11.14: a PR is linked to a task by either its branch name or a
//! `Totsuka-Task:` trailer in its body. Both are consulted; trailer wins on
//! mismatch because humans can rename branches but trailers come from the
//! Claude system prompt.

use crate::error::WatcherError;
use regex::Regex;
use sqlx::PgPool;
use std::sync::OnceLock;

pub fn task_id_short_from_branch(branch: &str) -> Option<String> {
    let mut parts = branch.split('/');
    if parts.next() != Some("totsuka") { return None; }
    let short = parts.next()?;
    let _phase = parts.next()?;
    if parts.next().is_some() { return None; } // exactly 3 segments
    if short.is_empty() { return None; }
    Some(short.to_string())
}

fn trailer_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^Totsuka-Task:\s+(\S+)\s*$").unwrap())
}

pub fn task_id_from_trailer(body: &str) -> Option<String> {
    trailer_re().captures_iter(body).last().map(|c| c[1].to_string())
}

pub async fn resolve_task_id(
    pool: &PgPool,
    branch: &str,
    body: Option<&str>,
) -> Result<Option<String>, WatcherError> {
    let by_branch = if let Some(short) = task_id_short_from_branch(branch) {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM tasks WHERE task_id_short = $1",
        )
        .bind(&short)
        .fetch_optional(pool)
        .await?;
        row.map(|r| r.0)
    } else {
        None
    };
    let by_trailer = if let Some(b) = body {
        if let Some(tid) = task_id_from_trailer(b) {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM tasks WHERE id = $1",
            )
            .bind(&tid)
            .fetch_optional(pool)
            .await?;
            row.map(|r| r.0)
        } else { None }
    } else { None };

    match (by_branch, by_trailer) {
        (Some(b), Some(t)) if b != t => {
            tracing::warn!(branch_task=%b, trailer_task=%t, "PR linkage mismatch; preferring trailer");
            Ok(Some(t))
        }
        (_, Some(t)) => Ok(Some(t)),
        (Some(b), None) => Ok(Some(b)),
        (None, None) => Ok(None),
    }
}
