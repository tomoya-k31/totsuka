use super::{IssueUpdate, PrUpdate, ReleaseUpdate, RepoSlug};
use crate::error::WatcherError;
use chrono::{DateTime, Utc};
use reqwest::{header, Client, Response};
use serde::Deserialize;
use totsuka_core::Secret;

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn next_link(resp: &Response) -> Option<String> {
    let link = resp.headers().get(header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let part = part.trim();
        if part.ends_with("rel=\"next\"") {
            let lt = part.find('<')?;
            let gt = part.find('>')?;
            return Some(part[lt + 1..gt].to_string());
        }
    }
    None
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &Client,
    url: &str,
    token: &Secret<String>,
) -> Result<(T, Option<String>), WatcherError> {
    let resp = client
        .get(url)
        .bearer_auth(token.expose())
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(WatcherError::Internal(format!("REST {s}: {body}")));
    }
    let next = next_link(&resp);
    let body: T = resp.json().await?;
    Ok((body, next))
}

#[derive(Deserialize)]
struct IssueRow {
    node_id: String,
    number: u64,
    state: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>, // present iff this row is actually a PR
}

pub(crate) async fn list_issues(
    client: &Client,
    endpoint_rest: &str,
    token: &Secret<String>,
    repo: &RepoSlug,
    since: DateTime<Utc>,
) -> Result<Vec<IssueUpdate>, WatcherError> {
    let mut url = format!(
        "{endpoint_rest}/repos/{}/{}/issues?since={}&state=all&per_page=100",
        repo.owner,
        repo.repo,
        rfc3339(since),
    );
    let mut out = Vec::new();
    loop {
        let (rows, next): (Vec<IssueRow>, _) = get_json(client, &url, token).await?;
        for r in rows {
            if r.pull_request.is_some() {
                continue;
            }
            out.push(IssueUpdate {
                node_id: r.node_id,
                repo: repo.clone(),
                number: r.number,
                updated_at: r.updated_at,
                state: r.state,
            });
        }
        match next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

// Task 10 fills these in.
pub(crate) async fn list_prs(
    _client: &Client,
    _endpoint_rest: &str,
    _token: &Secret<String>,
    _repo: &RepoSlug,
    _since: DateTime<Utc>,
) -> Result<Vec<PrUpdate>, WatcherError> {
    Err(WatcherError::Internal(
        "rest::list_prs not yet implemented (Task 10)".into(),
    ))
}

pub(crate) async fn list_releases(
    _client: &Client,
    _endpoint_rest: &str,
    _token: &Secret<String>,
    _repo: &RepoSlug,
    _since: DateTime<Utc>,
) -> Result<Vec<ReleaseUpdate>, WatcherError> {
    Err(WatcherError::Internal(
        "rest::list_releases not yet implemented (Task 10)".into(),
    ))
}
